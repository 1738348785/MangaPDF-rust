use super::{spawn_guarded, ImportedGroup, MangaPdfApp, TaskMessage};
use crate::model::{ImageEntry, Sequence};
use crate::pdf::CancelledError;
use crate::source;
use crate::util::{
    check_file_replaceable, derive_folder_sequence_name, sanitize_filename, trim_process_memory,
};
use eframe::egui;
use std::collections::{BTreeSet, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

impl MangaPdfApp {
    pub(super) fn add_paths_to_current_sequence(&mut self, mut paths: Vec<PathBuf>) {
        if self.is_generating || self.is_importing || paths.is_empty() {
            return;
        }

        let scan_sub = self.scan_subfolders;
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel_flag = Arc::clone(&cancel);
        self.is_cancelling = false;
        self.is_importing = true;
        self.progress = 0.0;
        self.progress_text = "正在扫描并导入图片...".to_string();
        let tx = self.task_sender.clone();

        spawn_guarded(tx.clone(), "导入", move || {
            // 先按自然语义排序拖入路径，确保同时拖入多卷文件夹或 .cbz/.zip 时分卷顺序不乱
            paths.sort_by(|a, b| source::compare_natural_paths(a, b));

            // 展开待导入的目录与归档列表：
            // - 开启「包含子文件夹图片」(scan_sub == true)：每个所选文件夹递归汇总图片，并自动发现内部 .zip/.cbz 归档；
            // - 关闭「包含子文件夹图片」(默认)：
            //   1. 自动发现文件夹内直属的 .zip/.cbz 漫画归档（拖入装满 .cbz 的合集目录直接按卷导入）；
            //   2. 若文件夹根目录直接含有图片，仅导入根目录一级图片，并在存在含图子目录时记录提示；
            //   3. 若根目录无直接图片但含多个有效子文件夹，自动拆分为多个独立分卷；
            //   4. 若根目录无直接图片且仅有单层包装子目录，自动递归提取内部图片或归档。
            let mut expanded_dirs: Vec<(PathBuf, bool)> = Vec::new();
            let mut archive_files: Vec<PathBuf> = Vec::new();
            let mut loose_images: Vec<PathBuf> = Vec::new();
            let mut skipped_subfolders: Vec<String> = Vec::new();
            let mut unsupported_exts: BTreeSet<String> = BTreeSet::new();

            for p in paths {
                if cancel.load(Ordering::Relaxed) {
                    let _ = tx.send(TaskMessage::Cancelled {
                        message: "已取消导入".to_string(),
                        details: Vec::new(),
                        last_output_path: None,
                    });
                    return;
                }

                if p.is_dir() {
                    if scan_sub {
                        expanded_dirs.push((p.clone(), true));
                        archive_files.extend(source::collect_archive_files(
                            &p,
                            true,
                            Some(&cancel),
                        ));
                    } else {
                        let direct_archives =
                            source::collect_archive_files(&p, false, Some(&cancel));
                        if source::dir_has_root_images(&p) {
                            expanded_dirs.push((p.clone(), false));
                            archive_files.extend(direct_archives);
                            let skipped = source::get_direct_subfolders(&p);
                            if !skipped.is_empty() {
                                let parent_label = derive_folder_sequence_name(&p);
                                for sub in skipped {
                                    let sub_name =
                                        sub.file_name().unwrap_or_default().to_string_lossy();
                                    skipped_subfolders.push(format!("{}/{}", parent_label, sub_name));
                                }
                            }
                        } else {
                            let subfolders = source::get_direct_subfolders(&p);
                            if subfolders.len() > 1 {
                                for sub in subfolders {
                                    let sub_rec = !source::dir_has_root_images(&sub);
                                    expanded_dirs.push((sub, sub_rec));
                                }
                                archive_files.extend(direct_archives);
                            } else if !direct_archives.is_empty() {
                                archive_files.extend(direct_archives);
                                if let Some(only_sub) = subfolders.into_iter().next() {
                                    let sub_rec = !source::dir_has_root_images(&only_sub);
                                    expanded_dirs.push((only_sub, sub_rec));
                                }
                            } else if subfolders.len() == 1 {
                                expanded_dirs.push((p.clone(), true));
                            } else {
                                // 根目录既无图片也无直属归档/含图子目录：尝试探测嵌套 .zip/.cbz 归档
                                let nested_archives =
                                    source::collect_archive_files(&p, true, Some(&cancel));
                                if !nested_archives.is_empty() {
                                    archive_files.extend(nested_archives);
                                } else {
                                    // 记录目录内常见但不支持的漫画/图片格式，便于给出清晰引导
                                    for ext in source::collect_unsupported_media_exts_in_dir(
                                        &p,
                                        true,
                                        Some(&cancel),
                                    ) {
                                        unsupported_exts.insert(ext);
                                    }
                                }
                            }
                        }
                    }
                } else if source::is_archive(&p) {
                    archive_files.push(p);
                } else if p
                    .extension()
                    .and_then(|s| s.to_str())
                    .is_some_and(source::is_image_extension)
                {
                    loose_images.push(p);
                } else {
                    let ext_label = p
                        .extension()
                        .and_then(|s| s.to_str())
                        .filter(|s| !s.is_empty())
                        .map(|s| format!(".{}", s.to_ascii_lowercase()))
                        .unwrap_or_else(|| "(无扩展名)".to_string());
                    unsupported_exts.insert(ext_label);
                }
            }

            if cancel.load(Ordering::Relaxed) {
                let _ = tx.send(TaskMessage::Cancelled {
                    message: "已取消导入".to_string(),
                    details: Vec::new(),
                    last_output_path: None,
                });
                return;
            }

            archive_files.sort_by(|a, b| source::compare_natural_paths(a, b));
            archive_files.dedup();

            let mut groups = Vec::new();
            let mut errors: Vec<String> = Vec::new();

            for (p, recursive) in expanded_dirs {
                if cancel.load(Ordering::Relaxed) {
                    let _ = tx.send(TaskMessage::Cancelled {
                        message: "已取消导入".to_string(),
                        details: Vec::new(),
                        last_output_path: None,
                    });
                    return;
                }
                // 兼容直接选择盘符根目录 (如 `D:\`) 时 `file_name()` 为空的情况，避免产生空分卷名与空输出文件名
                let seq_name = derive_folder_sequence_name(&p);
                let progress_tx = tx.clone();
                let label = seq_name.clone();
                let cb = move |current: usize, total: usize, name: &str| {
                    let _ = progress_tx.send(TaskMessage::Progress {
                        current,
                        total,
                        desc: format!(
                            "正在导入文件夹 {} [{}/{}]: {}",
                            label, current, total, name
                        ),
                    });
                };
                match source::load_entries_from_folder_with_progress(
                    &p,
                    0,
                    recursive,
                    Some(&cb),
                    Some(&cancel),
                ) {
                    Ok((entries, errs)) => {
                        if cancel.load(Ordering::Relaxed) {
                            let _ = tx.send(TaskMessage::Cancelled {
                                message: "已取消导入".to_string(),
                                details: Vec::new(),
                                last_output_path: None,
                            });
                            return;
                        }
                        errors.extend(errs.into_iter().map(|e| format!("[{}] {}", seq_name, e)));
                        if !entries.is_empty() {
                            groups.push(ImportedGroup {
                                seq_name: Some(seq_name),
                                out_dir: p.parent().map(|d| d.to_path_buf()),
                                entries,
                            });
                        } else {
                            for ext in source::collect_unsupported_media_exts_in_dir(
                                &p,
                                recursive,
                                Some(&cancel),
                            ) {
                                unsupported_exts.insert(ext);
                            }
                        }
                    }
                    Err(e) => errors.push(format!("[{}] {:#}", seq_name, e)),
                }
            }

            for p in archive_files {
                if cancel.load(Ordering::Relaxed) {
                    let _ = tx.send(TaskMessage::Cancelled {
                        message: "已取消导入".to_string(),
                        details: Vec::new(),
                        last_output_path: None,
                    });
                    return;
                }
                let seq_name = p
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                let progress_tx = tx.clone();
                let label = seq_name.clone();
                let cb = move |current: usize, total: usize, name: &str| {
                    let _ = progress_tx.send(TaskMessage::Progress {
                        current,
                        total,
                        desc: format!(
                            "正在导入归档 {} [{}/{}]: {}",
                            label, current, total, name
                        ),
                    });
                };
                match source::load_entries_from_archive_with_progress(
                    &p,
                    0,
                    Some(&cb),
                    Some(&cancel),
                ) {
                    Ok((entries, errs)) => {
                        if cancel.load(Ordering::Relaxed) {
                            let _ = tx.send(TaskMessage::Cancelled {
                                message: "已取消导入".to_string(),
                                details: Vec::new(),
                                last_output_path: None,
                            });
                            return;
                        }
                        errors.extend(errs.into_iter().map(|e| format!("[{}] {}", seq_name, e)));
                        if !entries.is_empty() {
                            groups.push(ImportedGroup {
                                seq_name: Some(seq_name),
                                out_dir: p.parent().map(|d| d.to_path_buf()),
                                entries,
                            });
                        }
                    }
                    Err(e) => errors.push(format!("[{}] {:#}", seq_name, e)),
                }
            }

            if !loose_images.is_empty() {
                if cancel.load(Ordering::Relaxed) {
                    let _ = tx.send(TaskMessage::Cancelled {
                        message: "已取消导入".to_string(),
                        details: Vec::new(),
                        last_output_path: None,
                    });
                    return;
                }
                let progress_tx = tx.clone();
                let cb = move |current: usize, total: usize, name: &str| {
                    let _ = progress_tx.send(TaskMessage::Progress {
                        current,
                        total,
                        desc: format!("正在导入图片 [{}/{}]: {}", current, total, name),
                    });
                };
                let (entries, errs) = source::load_entries_from_paths_with_progress(
                    &loose_images,
                    0,
                    Some(&cb),
                    Some(&cancel),
                );
                if cancel.load(Ordering::Relaxed) {
                    let _ = tx.send(TaskMessage::Cancelled {
                        message: "已取消导入".to_string(),
                        details: Vec::new(),
                        last_output_path: None,
                    });
                    return;
                }
                errors.extend(errs);
                if !entries.is_empty() {
                    let out_dir = loose_images
                        .first()
                        .and_then(|first| first.parent())
                        .map(|d| d.to_path_buf());
                    groups.push(ImportedGroup {
                        seq_name: None,
                        out_dir,
                        entries,
                    });
                }
            }

            let _ = tx.send(TaskMessage::ImportFinished {
                groups,
                errors,
                skipped_subfolders,
                unsupported_exts: unsupported_exts.into_iter().collect(),
            });
        });
    }

    fn append_or_create_sequence(
        &mut self,
        name: String,
        out_dir: Option<PathBuf>,
        entries: Vec<ImageEntry>,
    ) {
        if self.current_sequence().items.is_empty() {
            let current = self.current_sequence_mut();
            current.name = name.clone();
            current.output_filename = name;
            // 导入盘符根目录 (如 `D:\`) 时 parent() 为空，此时保留默认输出目录，
            // 避免输出目录被清空后退化成"进程当前目录"
            if let Some(dir) = out_dir {
                current.output_dir = Some(dir);
            }
            current.items = entries;
        } else {
            let mut new_seq = Sequence::new(self.next_sequence_id, name.clone());
            new_seq.output_filename = name;
            if let Some(dir) = out_dir {
                new_seq.output_dir = Some(dir);
            }
            new_seq.items = entries;
            self.next_sequence_id += 1;
            self.sequences.push(new_seq);
            self.active_sequence_idx = self.sequences.len() - 1;
            // 导入后自动切到新分卷：预览窗原本指向旧分卷的某一页，直接关闭，
            // 避免它在新分卷里按相同的页号显示另一张图（看起来像"自己跳页"）
            self.clear_preview_state();
        }
    }

    pub(super) fn trigger_generate_current(&mut self) {
        if self.is_generating || self.is_importing {
            return;
        }

        let seq = self.current_sequence().clone();
        if seq.items.is_empty() {
            self.set_status(
                "当前序列中没有图片，无法合成！".to_string(),
                true,
                Vec::new(),
            );
            return;
        }

        let out_dir = crate::model::resolve_output_dir(seq.output_dir.as_deref());
        let safe_name = sanitize_filename(&seq.output_filename);
        let pdf_name = if safe_name.to_ascii_lowercase().ends_with(".pdf") {
            safe_name
        } else {
            format!("{}.pdf", safe_name)
        };
        let final_path = out_dir.join(pdf_name);

        // 若目标 PDF 文件已存在，弹出系统确认框防止误覆盖原文件
        if final_path.exists() {
            if final_path.is_dir() {
                self.set_status(
                    format!(
                        "生成失败：目标路径 \"{}\" 是一个已存在的文件夹",
                        final_path.display()
                    ),
                    true,
                    Vec::new(),
                );
                return;
            }
            let confirm = rfd::MessageDialog::new()
                .set_title("PDF 文件已存在")
                .set_description(format!(
                    "目标文件已存在：\n{}\n\n是否确认覆盖此文件？",
                    final_path.display()
                ))
                .set_buttons(rfd::MessageButtons::YesNo)
                .show();
            if confirm != rfd::MessageDialogResult::Yes {
                return;
            }
            // 提前检测目标文件是否被阅读器锁住或设为只读，避免白跑整轮转码流程。
            // 同时申请 DELETE 权限：Edge / Chrome 等以「禁止删除」方式占用 PDF 时仅探测可写权限会漏判，
            // 而原子替换恰恰依赖重命名所需的 DELETE 权限。
            if let Err(e) = check_file_replaceable(&final_path) {
                self.set_status(
                    format!(
                        "生成失败：无法覆盖 \"{}\"：{}（请检查该 PDF 是否正被阅读器打开占用，或已设为只读）",
                        final_path.display(),
                        e
                    ),
                    true,
                    Vec::new(),
                );
                return;
            }
        }

        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel_flag = Arc::clone(&cancel);
        self.is_cancelling = false;
        self.is_generating = true;
        self.progress = 0.0;
        self.progress_text = format!("正在生成: {}", final_path.display());
        let tx = self.task_sender.clone();

        spawn_guarded(tx.clone(), "生成", move || {
            let callback_tx = tx.clone();
            let cb = move |current: usize, total: usize, name: &str| {
                let percent = (current as f32 / total.max(1) as f32) * 100.0;
                let _ = callback_tx.send(TaskMessage::Progress {
                    current,
                    total,
                    desc: format!(
                        "正在打包 [{}/{}]: {} ({:.0}%)",
                        current, total, name, percent
                    ),
                });
            };

            match crate::pdf::build_pdf_from_sequence_with_cancel(
                &seq,
                &final_path,
                Some(cb),
                Some(&cancel),
            ) {
                Ok(report) => {
                    let _ = tx.send(TaskMessage::Finished {
                        success_count: report.pages,
                        skipped: report.skipped,
                        output_path: final_path,
                    });
                }
                Err(e) if e.is::<CancelledError>() => {
                    let _ = tx.send(TaskMessage::Cancelled {
                        message: "已取消打包".to_string(),
                        details: Vec::new(),
                        last_output_path: None,
                    });
                }
                Err(e) => {
                    let _ = tx.send(TaskMessage::Error(format!("生成失败: {}", e)));
                }
            }
        });
    }

    pub(super) fn trigger_generate_all(&mut self) {
        if self.is_generating || self.is_importing {
            return;
        }

        let sequences: Vec<Sequence> = self
            .sequences
            .iter()
            .filter(|s| !s.items.is_empty())
            .cloned()
            .collect();

        if sequences.is_empty() {
            self.set_status("没有任何分卷包含图片！".to_string(), true, Vec::new());
            return;
        }

        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel_flag = Arc::clone(&cancel);
        self.is_cancelling = false;
        self.is_generating = true;
        self.progress = 0.0;
        self.progress_text = format!("批量处理 {} 个分卷中...", sequences.len());
        let tx = self.task_sender.clone();

        spawn_guarded(tx.clone(), "批量生成", move || {
            let total_seqs = sequences.len();
            let mut success_total = 0;
            let mut failed_errors = Vec::new();
            let mut skipped_all = Vec::new();
            let mut used_paths = HashSet::new();
            let mut last_output_path = None;
            let mut was_cancelled = false;

            for (idx, seq) in sequences.into_iter().enumerate() {
                if cancel.load(Ordering::Relaxed) {
                    was_cancelled = true;
                    break;
                }

                let out_dir = crate::model::resolve_output_dir(seq.output_dir.as_deref());
                let safe_name = sanitize_filename(&seq.output_filename);
                let stem = if safe_name.to_ascii_lowercase().ends_with(".pdf") {
                    &safe_name[..safe_name.len() - 4]
                } else {
                    &safe_name
                };
                let mut final_path = out_dir.join(format!("{}.pdf", stem));

                // 防止同批次多个分卷重名或覆盖磁盘上已存在的文件
                let mut suffix_idx = 2usize;
                while final_path.exists()
                    || !used_paths.insert(final_path.to_string_lossy().to_ascii_lowercase())
                {
                    final_path = out_dir.join(format!("{}_{}.pdf", stem, suffix_idx));
                    suffix_idx += 1;
                }

                let callback_tx = tx.clone();
                let seq_name = seq.name.clone();
                let cb = move |current: usize, total: usize, _name: &str| {
                    let overall_current = idx * 100 + (current * 100 / total.max(1));
                    let overall_total = total_seqs * 100;
                    let _ = callback_tx.send(TaskMessage::Progress {
                        current: overall_current,
                        total: overall_total,
                        desc: format!(
                            "批量打包 [{}/{}]: {} (第 {}/{} 页)",
                            idx + 1,
                            total_seqs,
                            seq_name,
                            current,
                            total
                        ),
                    });
                };

                match crate::pdf::build_pdf_from_sequence_with_cancel(
                    &seq,
                    &final_path,
                    Some(cb),
                    Some(&cancel),
                ) {
                    Ok(report) => {
                        success_total += 1;
                        last_output_path = Some(final_path);
                        skipped_all.extend(
                            report
                                .skipped
                                .into_iter()
                                .map(|s| format!("[{}] {}", seq.name, s)),
                        );
                    }
                    Err(e) if e.is::<CancelledError>() => {
                        was_cancelled = true;
                        break;
                    }
                    Err(e) => {
                        failed_errors.push(format!("[{}] {}", seq.name, e));
                    }
                }
            }

            if was_cancelled {
                let mut details = failed_errors;
                details.extend(skipped_all);
                let message = if success_total == 0 {
                    "已取消批量打包".to_string()
                } else {
                    format!("已取消批量打包（已完成前 {} 个分卷，剩余已中止）", success_total)
                };
                let _ = tx.send(TaskMessage::Cancelled {
                    message,
                    details,
                    last_output_path,
                });
                return;
            }

            let _ = tx.send(TaskMessage::BatchFinished {
                total_files: success_total,
                failed_errors,
                skipped: skipped_all,
                last_output_path,
            });
        });
    }

    pub(super) fn handle_async_messages(&mut self, ctx: &egui::Context) {
        while let Ok(msg) = self.task_receiver.try_recv() {
            match msg {
                TaskMessage::Progress {
                    current,
                    total,
                    desc,
                } => {
                    if !self.is_cancelling {
                        self.progress = current as f32 / total.max(1) as f32;
                        self.progress_text = desc;
                    }
                }
                TaskMessage::ImportFinished {
                    groups,
                    errors,
                    skipped_subfolders,
                    unsupported_exts,
                } => {
                    self.is_importing = false;
                    self.is_cancelling = false;
                    self.progress = 1.0;
                    let mut total_imported = 0usize;

                    for mut group in groups {
                        for entry in &mut group.entries {
                            entry.id = self.next_image_id;
                            self.next_image_id += 1;
                        }
                        total_imported += group.entries.len();

                        if let Some(seq_name) = group.seq_name {
                            self.append_or_create_sequence(seq_name, group.out_dir, group.entries);
                        } else {
                            let current = self.current_sequence_mut();
                            if (current.items.is_empty() || current.output_dir.is_none())
                                && group.out_dir.is_some()
                            {
                                current.output_dir = group.out_dir;
                            }
                            current.items.extend(group.entries);
                            // 保留用户已手动调好的顺序，新散图已在导入阶段完成自然排序；
                            // 同时基于分卷内全部条目的源路径重算相对 sort_key，确保跨批次导入多话散图后仍能保留章节层级
                            current.refresh_sort_keys();
                        }
                    }

                    let exts_preview = {
                        let shown: Vec<&str> = unsupported_exts
                            .iter()
                            .take(5)
                            .map(String::as_str)
                            .collect();
                        if unsupported_exts.len() > 5 {
                            format!("{} 等", shown.join("、"))
                        } else {
                            shown.join("、")
                        }
                    };

                    let mut details = errors.clone();
                    if !skipped_subfolders.is_empty() {
                        details.push(
                            "已跳过含图子文件夹（如需合并导入请勾选左侧「包含子文件夹图片」）："
                                .to_string(),
                        );
                        details.extend(skipped_subfolders.iter().map(|s| format!("  • {}", s)));
                    }
                    if !unsupported_exts.is_empty() {
                        details.push(format!(
                            "检测到暂不支持的格式：{}（当前支持 JPG/PNG/WebP/BMP/ZIP/CBZ）",
                            unsupported_exts.join("、")
                        ));
                    }

                    let (msg, is_err) = if total_imported == 0 {
                        if !errors.is_empty() {
                            (
                                format!("导入失败：{} 个问题 (悬停查看详情)", errors.len()),
                                true,
                            )
                        } else if !unsupported_exts.is_empty() {
                            (
                                format!(
                                    "未找到可导入图片；检测到暂不支持的格式：{}（支持 JPG/PNG/WebP/BMP/ZIP/CBZ）",
                                    exts_preview
                                ),
                                true,
                            )
                        } else {
                            ("未在所选路径中找到有效图片".to_string(), true)
                        }
                    } else {
                        let mut text = if errors.is_empty() {
                            format!("已成功导入 {} 张图片", total_imported)
                        } else {
                            format!("已导入 {} 张图片，{} 个问题", total_imported, errors.len())
                        };
                        if !skipped_subfolders.is_empty() {
                            text.push_str(&format!(
                                "（已跳过 {} 个含图子文件夹，如需合并请勾选「包含子文件夹图片」）",
                                skipped_subfolders.len()
                            ));
                        }
                        if !unsupported_exts.is_empty() {
                            text.push_str(&format!("，已忽略不支持格式：{}", exts_preview));
                        }
                        if !details.is_empty() {
                            text.push_str(" (悬停查看详情)");
                        }
                        (text, !errors.is_empty())
                    };

                    self.set_status(msg, is_err, details);
                    self.cleanup_stale_textures();
                    trim_process_memory();
                }
                TaskMessage::Finished {
                    success_count,
                    skipped,
                    output_path,
                } => {
                    self.is_generating = false;
                    self.is_cancelling = false;
                    self.progress = 1.0;
                    self.last_output_path = Some(output_path);
                    if skipped.is_empty() {
                        self.set_status(
                            format!("完成！已生成 PDF ({} 页)", success_count),
                            false,
                            Vec::new(),
                        );
                    } else {
                        self.set_status(
                            format!(
                                "已生成 PDF ({} 页)，跳过 {} 张无法处理的图片 (悬停查看详情)",
                                success_count,
                                skipped.len()
                            ),
                            true,
                            skipped,
                        );
                    }
                    self.cleanup_stale_textures();
                    trim_process_memory();
                }
                TaskMessage::BatchFinished {
                    total_files,
                    failed_errors,
                    skipped,
                    last_output_path,
                } => {
                    self.is_generating = false;
                    self.is_cancelling = false;
                    self.progress = 1.0;
                    if let Some(out_path) = last_output_path {
                        self.last_output_path = Some(out_path);
                    }
                    let skipped_note = if skipped.is_empty() {
                        String::new()
                    } else {
                        format!("，共跳过 {} 张无法处理的图片 (悬停查看详情)", skipped.len())
                    };
                    let mut details = failed_errors.clone();
                    details.extend(skipped.iter().cloned());
                    if failed_errors.is_empty() {
                        self.set_status(
                            format!(
                                "完成！共成功生成 {} 个分卷 PDF 文件{}",
                                total_files, skipped_note
                            ),
                            !skipped.is_empty(),
                            details,
                        );
                    } else if total_files > 0 {
                        self.set_status(
                            format!(
                                "部分完成：成功 {} 个，失败 {} 个 ({}){}",
                                total_files,
                                failed_errors.len(),
                                failed_errors.join("；"),
                                skipped_note
                            ),
                            true,
                            details,
                        );
                    } else {
                        self.set_status(
                            format!("批量生成失败：{}", failed_errors.join("；")),
                            true,
                            details,
                        );
                    }
                    self.cleanup_stale_textures();
                    trim_process_memory();
                }
                TaskMessage::Cancelled {
                    message,
                    details,
                    last_output_path,
                } => {
                    self.is_generating = false;
                    self.is_importing = false;
                    self.is_cancelling = false;
                    self.progress = 0.0;
                    if let Some(out_path) = last_output_path {
                        self.last_output_path = Some(out_path);
                    }
                    self.set_status(message, false, details);
                    trim_process_memory();
                }
                TaskMessage::Error(err) => {
                    self.is_generating = false;
                    self.is_importing = false;
                    self.is_cancelling = false;
                    self.set_status(err, true, Vec::new());
                    trim_process_memory();
                }
            }
        }

        while let Ok((loaded_id, res)) = self.preview_receiver.try_recv() {
            if self.preview_loading_id == Some(loaded_id) {
                self.preview_loading_id = None;
            }
            let is_current = self
                .preview_image_idx
                .and_then(|idx| self.current_sequence().items.get(idx))
                .is_some_and(|item| item.id == loaded_id);

            if is_current {
                if let Ok(mut pending) = self.preview_pending.lock() {
                    if pending.as_ref().is_some_and(|(e, _)| e.id == loaded_id) {
                        *pending = None;
                    }
                }
                let tex = match res {
                    Ok(color_img) => Some(ctx.load_texture(
                        "large_preview_texture",
                        color_img,
                        egui::TextureOptions::LINEAR,
                    )),
                    Err(e) => {
                        eprintln!("大图解码失败: {}", e);
                        // 窗口程序无控制台，把真实失败原因交给预览窗展示，而不是只显示笼统提示
                        self.preview_error = Some((loaded_id, e));
                        None
                    }
                };
                self.preview_texture = Some((loaded_id, tex));
                ctx.request_repaint();
            }
        }
    }
}
