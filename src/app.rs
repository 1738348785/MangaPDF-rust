use crate::model::{ImageEntry, PageSizeMode, Sequence};
use crate::source;
use eframe::egui::{
    self, Align, Color32, FontId, Layout, Rounding, Stroke, Vec2,
};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;

pub struct MangaPdfApp {
    sequences: Vec<Sequence>,
    active_sequence_idx: usize,
    next_sequence_id: usize,
    next_image_id: usize,

    texture_cache: HashMap<usize, egui::TextureHandle>,

    is_dark_theme: bool,
    status_message: Option<(String, bool)>,
    is_generating: bool,
    progress: f32,
    progress_text: String,

    task_receiver: Receiver<TaskMessage>,
    task_sender: Sender<TaskMessage>,

    show_about_dialog: bool,
    last_output_path: Option<PathBuf>,
    scan_subfolders: bool,
    app_logo_texture: Option<egui::TextureHandle>,
    github_logo_texture: Option<egui::TextureHandle>,
    frame_count: usize,
    preview_image_idx: Option<usize>,
    preview_texture: Option<(usize, egui::TextureHandle)>,
    preview_actual_size: bool,
}

enum TaskMessage {
    Progress { current: usize, total: usize, desc: String },
    Finished { success_count: usize, output_path: PathBuf },
    BatchFinished { total_files: usize },
    Error(String),
}

impl MangaPdfApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let (tx, rx) = channel();
        let initial_seq = Sequence::new(1, "第 01 卷".to_string());

        Self {
            sequences: vec![initial_seq],
            active_sequence_idx: 0,
            next_sequence_id: 2,
            next_image_id: 1,
            texture_cache: HashMap::new(),
            is_dark_theme: false,
            status_message: None,
            is_generating: false,
            progress: 0.0,
            progress_text: String::new(),
            task_receiver: rx,
            task_sender: tx,
            show_about_dialog: false,
            last_output_path: None,
            scan_subfolders: false,
            app_logo_texture: None,
            github_logo_texture: None,
            frame_count: 0,
            preview_image_idx: None,
            preview_texture: None,
            preview_actual_size: false,
        }
    }

    fn get_app_logo(&mut self, ctx: &egui::Context) -> egui::TextureHandle {
        if let Some(tex) = &self.app_logo_texture {
            return tex.clone();
        }
        let bytes = include_bytes!("../assets/icon.png");
        let img = image::load_from_memory(bytes).expect("Failed to load icon.png").to_rgba8();
        let (w, h) = img.dimensions();
        let color_image = egui::ColorImage::from_rgba_unmultiplied(
            [w as usize, h as usize],
            &img.into_raw(),
        );
        let tex = ctx.load_texture("app_logo_texture", color_image, egui::TextureOptions::LINEAR);
        self.app_logo_texture = Some(tex.clone());
        tex
    }

    fn get_github_logo(&mut self, ctx: &egui::Context) -> egui::TextureHandle {
        if let Some(tex) = &self.github_logo_texture {
            return tex.clone();
        }
        let bytes = include_bytes!("../assets/github.png");
        let img = image::load_from_memory(bytes).expect("Failed to load github.png").to_rgba8();
        let (w, h) = img.dimensions();
        let color_image = egui::ColorImage::from_rgba_unmultiplied(
            [w as usize, h as usize],
            &img.into_raw(),
        );
        let tex = ctx.load_texture("github_logo_texture", color_image, egui::TextureOptions::LINEAR);
        self.github_logo_texture = Some(tex.clone());
        tex
    }

    fn current_sequence_mut(&mut self) -> &mut Sequence {
        if self.active_sequence_idx >= self.sequences.len() {
            self.active_sequence_idx = 0;
        }
        &mut self.sequences[self.active_sequence_idx]
    }

    fn current_sequence(&self) -> &Sequence {
        if self.active_sequence_idx >= self.sequences.len() {
            &self.sequences[0]
        } else {
            &self.sequences[self.active_sequence_idx]
        }
    }

    fn cleanup_stale_textures(&mut self) {
        let valid_ids: HashSet<usize> = self
            .sequences
            .iter()
            .flat_map(|s| s.items.iter().map(|i| i.id))
            .collect();
        self.texture_cache.retain(|id, _| valid_ids.contains(id));
        if let Some((id, _)) = &self.preview_texture {
            if !valid_ids.contains(id) {
                self.preview_texture = None;
                self.preview_image_idx = None;
            }
        }
        trim_process_memory();
    }

    fn add_paths_to_current_sequence(&mut self, paths: Vec<PathBuf>) {
        let scan_sub = self.scan_subfolders;
        let final_paths = if scan_sub && paths.len() == 1 && paths[0].is_dir() {
            let single_dir = &paths[0];
            let subfolders = source::get_direct_subfolders(single_dir);
            // 检查根目录下是否直接存在散落图片
            let root_has_images = std::fs::read_dir(single_dir).map(|entries| {
                entries.flatten().any(|e| {
                    let p = e.path();
                    p.is_file() && p.extension().and_then(|s| s.to_str()).map_or(false, source::is_image_extension)
                })
            }).unwrap_or(false);

            if !root_has_images && subfolders.len() > 1 {
                // 自动识别为整套漫画合集目录，拆分为多个独立分卷
                subfolders
            } else {
                paths
            }
        } else {
            paths
        };

        let mut loose_images = Vec::new();

        for p in final_paths {
            if source::is_archive(&p) {
                let seq_name = p.file_stem().unwrap_or_default().to_string_lossy().to_string();
                if let Ok(entries) = source::load_entries_from_archive(&p, self.next_image_id) {
                    if !entries.is_empty() {
                        self.next_image_id += entries.len() + 1;
                        self.append_or_create_sequence(seq_name, p.parent().map(|d| d.to_path_buf()), entries);
                    }
                }
            } else if p.is_dir() {
                let seq_name = p.file_name().unwrap_or_default().to_string_lossy().to_string();
                if let Ok(entries) = source::load_entries_from_folder(&p, self.next_image_id, scan_sub) {
                    if !entries.is_empty() {
                        self.next_image_id += entries.len() + 1;
                        self.append_or_create_sequence(seq_name, p.parent().map(|d| d.to_path_buf()), entries);
                    }
                }
            } else {
                loose_images.push(p);
            }
        }

        if !loose_images.is_empty() {
            let entries = source::load_entries_from_paths(&loose_images, self.next_image_id);
            if !entries.is_empty() {
                self.next_image_id += entries.len() + 1;
                let current = self.current_sequence_mut();
                if current.output_dir.is_none() {
                    if let Some(first) = loose_images.first() {
                        current.output_dir = first.parent().map(|d| d.to_path_buf());
                    }
                }
                current.items.extend(entries);
                current.natural_sort();
            }
        }

        self.cleanup_stale_textures();
    }

    fn append_or_create_sequence(&mut self, name: String, out_dir: Option<PathBuf>, entries: Vec<ImageEntry>) {
        if self.current_sequence().items.is_empty() {
            let current = self.current_sequence_mut();
            current.name = name.clone();
            current.output_filename = name;
            current.output_dir = out_dir;
            current.items = entries;
        } else {
            let mut new_seq = Sequence::new(self.next_sequence_id, name.clone());
            new_seq.output_filename = name;
            new_seq.output_dir = out_dir;
            new_seq.items = entries;
            self.next_sequence_id += 1;
            self.sequences.push(new_seq);
            self.active_sequence_idx = self.sequences.len() - 1;
        }
    }

    fn trigger_generate_current(&mut self) {
        if self.is_generating {
            return;
        }

        let seq = self.current_sequence().clone();
        if seq.items.is_empty() {
            self.status_message = Some(("当前序列中没有图片，无法合成！".to_string(), true));
            return;
        }

        let out_dir = seq.output_dir.clone().unwrap_or_else(|| PathBuf::from("."));
        let safe_name = sanitize_filename(&seq.output_filename);
        let pdf_name = if safe_name.ends_with(".pdf") {
            safe_name
        } else {
            format!("{}.pdf", safe_name)
        };
        let final_path = out_dir.join(pdf_name);

        self.is_generating = true;
        self.progress = 0.0;
        self.progress_text = format!("正在生成: {}", final_path.display());
        let tx = self.task_sender.clone();

        thread::spawn(move || {
            let callback_tx = tx.clone();
            let cb = move |current: usize, total: usize, name: &str| {
                let percent = (current as f32 / total.max(1) as f32) * 100.0;
                let _ = callback_tx.send(TaskMessage::Progress {
                    current,
                    total,
                    desc: format!("正在打包 [{}/{}]: {} ({:.0}%)", current, total, name, percent),
                });
            };

            match crate::pdf::build_pdf_from_sequence(&seq, &final_path, Some(cb)) {
                Ok(count) => {
                    let _ = tx.send(TaskMessage::Finished {
                        success_count: count,
                        output_path: final_path,
                    });
                }
                Err(e) => {
                    let _ = tx.send(TaskMessage::Error(format!("生成失败: {:?}", e)));
                }
            }
        });
    }

    fn trigger_generate_all(&mut self) {
        if self.is_generating {
            return;
        }

        let sequences: Vec<Sequence> = self
            .sequences
            .iter()
            .filter(|s| !s.items.is_empty())
            .cloned()
            .collect();

        if sequences.is_empty() {
            self.status_message = Some(("没有任何分卷包含图片！".to_string(), true));
            return;
        }

        self.is_generating = true;
        self.progress = 0.0;
        self.progress_text = format!("批量处理 {} 个分卷中...", sequences.len());
        let tx = self.task_sender.clone();

        thread::spawn(move || {
            let total_seqs = sequences.len();
            let mut success_total = 0;

            for (idx, seq) in sequences.into_iter().enumerate() {
                let out_dir = seq.output_dir.clone().unwrap_or_else(|| PathBuf::from("."));
                let safe_name = sanitize_filename(&seq.output_filename);
                let pdf_name = if safe_name.ends_with(".pdf") {
                    safe_name
                } else {
                    format!("{}.pdf", safe_name)
                };
                let final_path = out_dir.join(pdf_name);

                let callback_tx = tx.clone();
                let seq_name = seq.name.clone();
                let cb = move |current: usize, total: usize, _name: &str| {
                    let overall_current = idx * 100 + (current * 100 / total.max(1));
                    let overall_total = total_seqs * 100;
                    let _ = callback_tx.send(TaskMessage::Progress {
                        current: overall_current,
                        total: overall_total,
                        desc: format!("批量打包 [{}/{}]: {} (第 {}/{} 页)", idx + 1, total_seqs, seq_name, current, total),
                    });
                };

                if let Ok(_) = crate::pdf::build_pdf_from_sequence(&seq, &final_path, Some(cb)) {
                    success_total += 1;
                }
            }

            let _ = tx.send(TaskMessage::BatchFinished {
                total_files: success_total,
            });
        });
    }

    fn handle_async_messages(&mut self) {
        while let Ok(msg) = self.task_receiver.try_recv() {
            match msg {
                TaskMessage::Progress { current, total, desc } => {
                    self.progress = current as f32 / total.max(1) as f32;
                    self.progress_text = desc;
                }
                TaskMessage::Finished { success_count, output_path } => {
                    self.is_generating = false;
                    self.progress = 1.0;
                    self.last_output_path = Some(output_path);
                    self.status_message = Some((
                        format!("完成！已生成 PDF ({} 页)", success_count),
                        false,
                    ));
                    self.cleanup_stale_textures();
                }
                TaskMessage::BatchFinished { total_files } => {
                    self.is_generating = false;
                    self.progress = 1.0;
                    self.last_output_path = self.current_sequence().output_dir.clone();
                    self.status_message = Some((
                        format!("完成！共成功生成 {} 个分卷 PDF 文件", total_files),
                        false,
                    ));
                    self.cleanup_stale_textures();
                }
                TaskMessage::Error(err) => {
                    self.is_generating = false;
                    self.status_message = Some((err, true));
                }
            }
        }
    }

    fn apply_modern_styling(&self, ctx: &egui::Context) {
        let mut style = (*ctx.style()).clone();

        style.visuals.window_rounding = Rounding::same(8.0);
        style.visuals.menu_rounding = Rounding::same(6.0);
        style.visuals.widgets.noninteractive.rounding = Rounding::same(6.0);
        style.visuals.widgets.inactive.rounding = Rounding::same(6.0);
        style.visuals.widgets.hovered.rounding = Rounding::same(6.0);
        style.visuals.widgets.active.rounding = Rounding::same(6.0);

        style.spacing.item_spacing = Vec2::new(8.0, 7.0);
        style.spacing.window_margin = egui::Margin::same(12.0);
        style.spacing.button_padding = Vec2::new(10.0, 5.0);
        style.spacing.slider_width = 120.0;
        style.interaction.selectable_labels = false;

        if self.is_dark_theme {
            let mut dark = egui::Visuals::dark();
            dark.extreme_bg_color = Color32::from_rgb(17, 24, 39);
            dark.panel_fill = Color32::from_rgb(17, 24, 39); // Gray-900
            dark.window_fill = Color32::from_rgb(31, 41, 55); // Gray-800
            dark.widgets.noninteractive.bg_fill = Color32::from_rgb(31, 41, 55);
            dark.widgets.inactive.bg_fill = Color32::from_rgb(55, 65, 81);
            dark.widgets.hovered.bg_fill = Color32::from_rgb(75, 85, 99);
            dark.widgets.active.bg_fill = Color32::from_rgb(59, 130, 246);
            dark.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(75, 85, 99));
            dark.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(59, 130, 246));
            style.visuals = dark;
        } else {
            let mut light = egui::Visuals::light();
            light.extreme_bg_color = Color32::from_rgb(243, 244, 246);
            light.panel_fill = Color32::from_rgb(249, 250, 251); // Gray-50
            light.window_fill = Color32::WHITE;
            light.widgets.noninteractive.bg_fill = Color32::from_rgb(243, 244, 246);
            light.widgets.inactive.bg_fill = Color32::from_rgb(229, 231, 235);
            light.widgets.hovered.bg_fill = Color32::from_rgb(209, 213, 219);
            light.widgets.active.bg_fill = Color32::from_rgb(37, 99, 235);
            light.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(209, 213, 219));
            light.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(37, 99, 235));
            style.visuals = light;
        }

        ctx.set_style(style);
    }
}

impl eframe::App for MangaPdfApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_async_messages();
        self.apply_modern_styling(ctx);

        self.frame_count += 1;
        // 启动后第 3 帧（字体与显卡驱动着色器初始化上屏完毕），主动修剪工作集，交还显卡驱动预分配的大量非活跃内存
        if self.frame_count == 3 || (self.frame_count % 300 == 0 && !self.is_generating) {
            trim_process_memory();
        }

        if self.is_generating {
            ctx.request_repaint();
        }

        let dropped_files = ctx.input(|i| i.raw.dropped_files.clone());
        if !dropped_files.is_empty() {
            let paths: Vec<PathBuf> = dropped_files
                .into_iter()
                .filter_map(|f| f.path)
                .collect();
            if !paths.is_empty() {
                self.add_paths_to_current_sequence(paths);
            }
        }

        let is_dark = self.is_dark_theme;
        let card_bg = if is_dark { Color32::from_rgb(31, 41, 55) } else { Color32::WHITE };
        let border_stroke = Stroke::new(1.0_f32, if is_dark { Color32::from_rgb(55, 65, 81) } else { Color32::from_rgb(229, 231, 235) });
        let text_muted = if is_dark { Color32::from_rgb(156, 163, 175) } else { Color32::from_rgb(107, 114, 128) };
        let primary_color = if is_dark { Color32::from_rgb(59, 130, 246) } else { Color32::from_rgb(37, 99, 235) };
        let is_hovering_files = ctx.input(|i| !i.raw.hovered_files.is_empty());

        let github_logo = self.get_github_logo(ctx);
        let app_logo = self.get_app_logo(ctx);

        // 1. 顶部导航栏
        egui::TopBottomPanel::top("top_bar")
            .frame(egui::Frame::none().fill(card_bg).stroke(border_stroke).inner_margin(egui::Margin::symmetric(16.0, 10.0)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let logo_bg = primary_color;
                    egui::Frame::none()
                        .fill(logo_bg)
                        .rounding(Rounding::same(5.0))
                        .inner_margin(egui::Margin::symmetric(7.0, 3.0))
                        .show(ui, |ui| {
                            ui.label(egui::RichText::new("MangaPDF").color(Color32::WHITE).strong().font(FontId::proportional(14.0)));
                        });

                    ui.add_space(6.0);
                    ui.label(egui::RichText::new("漫画与图包转 PDF 工具").font(FontId::proportional(14.0)).strong());

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button("关于").clicked() {
                            self.show_about_dialog = true;
                        }

                        ui.separator();

                        let theme_btn = if is_dark { "浅色模式" } else { "深色模式" };
                        if ui.button(theme_btn).clicked() {
                            self.is_dark_theme = !self.is_dark_theme;
                        }
                    });
                });
            });

        // 2. 底部状态栏
        egui::TopBottomPanel::bottom("bottom_bar")
            .frame(egui::Frame::none().fill(card_bg).stroke(border_stroke).inner_margin(egui::Margin::symmetric(16.0, 7.0)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if self.is_generating {
                        ui.spinner();
                        ui.label(egui::RichText::new(&self.progress_text).color(primary_color).strong());
                        ui.add_space(10.0);
                        ui.add(egui::ProgressBar::new(self.progress).show_percentage());
                    } else {
                        if let Some((msg, is_err)) = &self.status_message {
                            let dot_color = if *is_err { Color32::from_rgb(239, 68, 68) } else { Color32::from_rgb(16, 185, 129) };
                            ui.label(egui::RichText::new("•").color(dot_color).font(FontId::proportional(18.0)));
                            ui.label(egui::RichText::new(msg).color(dot_color).strong());
                        } else {
                            ui.label(egui::RichText::new("•").color(Color32::from_rgb(16, 185, 129)).font(FontId::proportional(18.0)));
                            ui.label(egui::RichText::new("就绪 · 可直接将漫画文件夹或压缩包拖入窗口").color(text_muted));
                        }

                        if let Some(out_path) = &self.last_output_path {
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                let open_btn = egui::Button::new(
                                    egui::RichText::new("📂 打开所在目录")
                                        .font(FontId::proportional(12.0))
                                        .color(primary_color)
                                        .strong(),
                                );
                                if ui.add(open_btn).on_hover_text("在 Windows 资源管理器中高亮定位该文件").clicked() {
                                    open_in_file_explorer(out_path);
                                }
                            });
                        }
                    }
                });
            });

        // 3. 左侧控制面板 (精简、整齐、高度平衡)
        egui::SidePanel::left("left_control_panel")
            .exact_width(270.0)
            .resizable(false)
            .frame(egui::Frame::none().fill(card_bg).stroke(border_stroke).inner_margin(egui::Margin::same(14.0)))
            .show(ctx, |ui| {
                if self.is_generating {
                    ui.disable();
                }
                ui.set_max_width(242.0);

                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.set_max_width(242.0);
                        let total_pages = self.current_sequence().items.len();
                        let seq_name = self.current_sequence().name.clone();

                        // 头部统计卡片
                        egui::Frame::none()
                            .fill(if is_dark { Color32::from_rgb(17, 24, 39) } else { Color32::from_rgb(243, 244, 246) })
                            .rounding(Rounding::same(6.0))
                            .inner_margin(egui::Margin::symmetric(12.0, 10.0))
                            .show(ui, |ui| {
                                ui.set_max_width(218.0);
                                ui.label(
                                    egui::RichText::new(format!("共 {} 页", total_pages))
                                        .font(FontId::proportional(22.0))
                                        .strong()
                                        .color(primary_color),
                                );

                                let short_name = if seq_name.chars().count() > 18 {
                                    format!("{}...", seq_name.chars().take(16).collect::<String>())
                                } else {
                                    seq_name
                                };
                                ui.label(egui::RichText::new(format!("分卷: {}", short_name)).font(FontId::proportional(11.0)).color(text_muted));
                            });

                        ui.add_space(8.0);

                        // 快捷导入操作（紧凑 2 按钮排布）
                        ui.horizontal(|ui| {
                            if ui.add_sized([117.0, 30.0], egui::Button::new("导入文件夹")).clicked() {
                                if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                                    self.add_paths_to_current_sequence(vec![folder]);
                                }
                            }
                            if ui.add_sized([117.0, 30.0], egui::Button::new("选择图片文件")).clicked() {
                                if let Some(files) = rfd::FileDialog::new()
                                    .add_filter("图片文件", &["jpg", "jpeg", "png", "webp", "bmp"])
                                    .pick_files()
                                {
                                    self.add_paths_to_current_sequence(files);
                                }
                            }
                        });

                        ui.add_space(2.0);
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut self.scan_subfolders, "包含子文件夹图片");
                        }).response.on_hover_text("开启：导入时递归扫描子文件夹内全部图片\n关闭：仅导入根目录下的一级图片（自动忽略蒙版/翻译缓存等子目录）");

                        ui.add_space(8.0);
                        ui.separator();
                        ui.add_space(6.0);

                        // 输出设置
                        ui.label(egui::RichText::new("输出配置").font(FontId::proportional(13.0)).strong());
                        ui.add_space(4.0);

                        let active_seq = self.current_sequence_mut();

                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("页面模式:").color(text_muted));
                            egui::ComboBox::from_id_salt("page_mode_combo")
                                .selected_text(active_seq.page_size_mode.label())
                                .width(160.0)
                                .show_ui(ui, |ui| {
                                    for mode in PageSizeMode::all() {
                                        ui.selectable_value(&mut active_seq.page_size_mode, *mode, mode.label());
                                    }
                                });
                        });

                        ui.add_space(2.0);
                        ui.checkbox(&mut active_seq.lossless_direct, "保持原画无损直存 (100%原画)")
                            .on_hover_text("保留图片原始画质，不进行任何画质压缩，100% 还原原图");

                        ui.add_space(3.0);
                        ui.add_enabled_ui(!active_seq.lossless_direct, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new("压缩画质:").color(text_muted));
                                ui.add(egui::Slider::new(&mut active_seq.quality, 10..=100).suffix("%"));
                            });
                        });
                        if active_seq.lossless_direct {
                            ui.label(egui::RichText::new("💡 已启用原画无损直存，保持 100% 原始画质").font(FontId::proportional(10.0)).color(text_muted));
                        }

                        ui.add_space(3.0);
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("页面边距:").color(text_muted));
                            let mut margin_str = active_seq.margin_mm.to_string();
                            let resp = ui.add_sized([56.0, 22.0], egui::TextEdit::singleline(&mut margin_str));
                            if resp.changed() {
                                if let Ok(val) = margin_str.parse::<u32>() {
                                    active_seq.margin_mm = val;
                                }
                            }
                            ui.label(egui::RichText::new("mm (0为无边框)").font(FontId::proportional(11.0)).color(text_muted));
                        });

                        ui.add_space(4.0);
                        ui.label(egui::RichText::new("分卷 / 输出文件名:").color(text_muted));
                        if ui.add(egui::TextEdit::singleline(&mut active_seq.output_filename).desired_width(242.0)).changed() {
                            active_seq.name = active_seq.output_filename.clone();
                        }

                        ui.add_space(4.0);
                        ui.label(egui::RichText::new("输出保存目录:").color(text_muted));
                        ui.horizontal(|ui| {
                            let mut dir_str = active_seq
                                .output_dir
                                .as_ref()
                                .map(|p| p.to_string_lossy().to_string())
                                .unwrap_or_default();
                            if ui.add(egui::TextEdit::singleline(&mut dir_str).desired_width(185.0)).changed() {
                                active_seq.output_dir = Some(PathBuf::from(dir_str));
                            }
                            if ui.add_sized([45.0, 22.0], egui::Button::new("浏览")).clicked() {
                                if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                                    active_seq.output_dir = Some(folder);
                                }
                            }
                        });

                        ui.add_space(14.0);
                        ui.separator();
                        ui.add_space(8.0);

                        // 主打包按钮
                        let (btn_text, btn_fill) = if self.is_generating {
                            ("⏳ 正在打包中...", Color32::from_rgb(156, 163, 175))
                        } else {
                            ("开始打包为 PDF", primary_color)
                        };

                        let primary_btn = egui::Button::new(
                            egui::RichText::new(btn_text)
                                .font(FontId::proportional(15.0))
                                .strong()
                                .color(Color32::WHITE),
                        )
                        .fill(btn_fill);

                        if ui.add_sized([242.0, 38.0], primary_btn).clicked() && !self.is_generating {
                            self.trigger_generate_current();
                        }

                        ui.add_space(4.0);

                        if self.sequences.len() > 1 {
                            let batch_btn = egui::Button::new(
                                egui::RichText::new(format!("批量生成全部分卷 ({} 个)", self.sequences.len()))
                                    .font(FontId::proportional(12.0)),
                            );

                            if ui.add_sized([242.0, 30.0], batch_btn).clicked() && !self.is_generating {
                                self.trigger_generate_all();
                            }
                        }
                    });
            });

        // 4. 右侧中央主区域 (紧凑拖拽卡片 + 预览网格)
        egui::CentralPanel::default().show(ctx, |ui| {
            if self.is_generating {
                ui.disable();
            }
            // 分卷标签栏
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("分卷管理:").strong());

                let mut to_delete = None;
                for (idx, seq) in self.sequences.iter().enumerate() {
                    let is_active = idx == self.active_sequence_idx;
                    let pill_bg = if is_active {
                        primary_color
                    } else if is_dark {
                        Color32::from_rgb(31, 41, 55)
                    } else {
                        Color32::from_rgb(243, 244, 246)
                    };
                    let text_color = if is_active {
                        Color32::WHITE
                    } else {
                        ui.visuals().text_color()
                    };

                    let short_name = if seq.name.chars().count() > 18 {
                        format!("{}...", seq.name.chars().take(16).collect::<String>())
                    } else {
                        seq.name.clone()
                    };

                    let label_text = format!("{} ({}页)", short_name, seq.items.len());
                    let btn = egui::Button::new(egui::RichText::new(label_text).color(text_color).strong())
                        .fill(pill_bg)
                        .rounding(Rounding::same(14.0));

                    let resp = ui.add(btn).on_hover_text(format!("{} (点击切换，右键可删除)", seq.name));
                    if resp.clicked() {
                        self.active_sequence_idx = idx;
                    }
                    resp.context_menu(|ui| {
                        if self.sequences.len() > 1 {
                            if ui.button("删除分卷").clicked() {
                                to_delete = Some(idx);
                                ui.close_menu();
                            }
                        }
                    });
                }

                if ui.button("+ 新建分卷").clicked() {
                    let name = format!("第 {:02} 卷", self.next_sequence_id);
                    let new_seq = Sequence::new(self.next_sequence_id, name);
                    self.next_sequence_id += 1;
                    self.sequences.push(new_seq);
                    self.active_sequence_idx = self.sequences.len() - 1;
                }

                if self.sequences.len() > 1 {
                    if ui.button("× 删除").clicked() {
                        to_delete = Some(self.active_sequence_idx);
                    }
                }

                if let Some(idx) = to_delete {
                    self.sequences.remove(idx);
                    if self.active_sequence_idx >= self.sequences.len() {
                        self.active_sequence_idx = self.sequences.len() - 1;
                    }
                    self.cleanup_stale_textures();
                }
            });

            ui.add_space(8.0);

            // 紧凑、精致的拖拽上传卡片 (高 68px，绝不占满整个屏幕)
            let drop_zone_rect = ui.available_rect_before_wrap();
            let drop_height = 68.0;
            let (rect, response) = ui.allocate_exact_size(Vec2::new(drop_zone_rect.width(), drop_height), egui::Sense::click());

            let is_hovered = response.hovered();

            let drop_bg = if is_hovering_files {
                if is_dark { Color32::from_rgb(30, 58, 138) } else { Color32::from_rgb(239, 246, 255) }
            } else if is_hovered {
                if is_dark { Color32::from_rgb(38, 48, 65) } else { Color32::from_rgb(248, 250, 252) }
            } else if is_dark {
                Color32::from_rgb(31, 41, 55)
            } else {
                Color32::WHITE
            };

            let drop_border = if is_hovering_files || is_hovered {
                primary_color
            } else if is_dark {
                Color32::from_rgb(55, 65, 81)
            } else {
                Color32::from_rgb(209, 213, 219)
            };

            let stroke_width = if is_hovering_files { 2.0_f32 } else { 1.0_f32 };
            ui.painter().rect_filled(rect, 6.0, drop_bg);
            ui.painter().rect_stroke(rect, 6.0, Stroke::new(stroke_width, drop_border));

            if is_hovered && !is_hovering_files {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }

            ui.allocate_new_ui(egui::UiBuilder::new().max_rect(rect), |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(13.0);
                    let title = if is_hovering_files {
                        "📥 释放鼠标立即导入漫画与图片"
                    } else {
                        "点击或将图片 / 文件夹 / CBZ 压缩包拖拽到此处"
                    };
                    let title_color = if is_hovering_files || is_hovered {
                        primary_color
                    } else {
                        ui.visuals().text_color()
                    };
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(title)
                                .font(FontId::proportional(14.0))
                                .strong()
                                .color(title_color),
                        )
                        .selectable(false)
                        .sense(egui::Sense::hover()),
                    );
                    ui.add_space(3.0);
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new("支持 JPG · PNG · WebP · BMP · ZIP · CBZ · 自动按自然顺序整理")
                                .font(FontId::proportional(11.0))
                                .color(text_muted),
                        )
                        .selectable(false)
                        .sense(egui::Sense::hover()),
                    );
                });
            });

            if response.clicked() {
                if let Some(files) = rfd::FileDialog::new()
                    .add_filter("图片或漫画归档", &["jpg", "jpeg", "png", "webp", "bmp", "zip", "cbz"])
                    .pick_files()
                {
                    self.add_paths_to_current_sequence(files);
                }
            }

            ui.add_space(14.0);

            // 预览列表工具栏
            let total_items = self.current_sequence().items.len();
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(format!("预览与排序 (共 {} 页)", total_items)).font(FontId::proportional(14.0)).strong());

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("清空全部").clicked() {
                        self.current_sequence_mut().clear();
                        self.cleanup_stale_textures();
                    }

                    if ui.button("自然排序 (1->2->10)").clicked() {
                        self.current_sequence_mut().natural_sort();
                    }
                });
            });

            ui.add_space(10.0);

            // 自动控容
            if self.texture_cache.len() > 150 {
                let active_ids: HashSet<usize> = self.current_sequence().items.iter().map(|i| i.id).collect();
                self.texture_cache.retain(|id, _| active_ids.contains(id));
            }

            // 预加载当前可视所需的缩略图
            let needed_textures: Vec<(usize, usize, usize, Vec<u8>)> = self
                .current_sequence()
                .items
                .iter()
                .filter(|item| !self.texture_cache.contains_key(&item.id))
                .filter_map(|item| {
                    item.thumb_rgb
                        .as_ref()
                        .map(|rgb| (item.id, item.thumb_width, item.thumb_height, rgb.clone()))
                })
                .collect();

            for (id, w, h, rgb) in needed_textures {
                let color_image = egui::ColorImage::from_rgb([w, h], &rgb);
                let texture = ctx.load_texture(
                    format!("thumb_{}", id),
                    color_image,
                    egui::TextureOptions::LINEAR,
                );
                self.texture_cache.insert(id, texture);
            }

            // 缩略图网格 或 空状态占位
            if total_items == 0 {
                ui.add_space(60.0);
                ui.vertical_centered(|ui| {
                    ui.label(egui::RichText::new("暂无图片，请点击上方导入或直接拖入漫画文件").font(FontId::proportional(13.0)).color(text_muted));
                });
            } else {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let card_inner_w = 122.0;
                        let card_margin = 8.0;
                        let card_outer_w = card_inner_w + card_margin * 2.0; // 138.0
                        let gap = 12.0;
                        // 预留右侧滚动条空间（24px），严格杜绝右侧卡片被视口截断
                        let avail_w = (ui.available_width() - 24.0).max(card_outer_w);
                        let cols = ((avail_w + gap) / (card_outer_w + gap)).floor().max(1.0) as usize;

                        let mut swap_action = None;
                        let mut remove_action = None;
                        let mut preview_action = None;

                        egui::Grid::new("image_cards_grid")
                            .spacing(Vec2::new(gap, 16.0))
                            .show(ui, |ui| {
                                for (idx, item) in self.current_sequence().items.iter().enumerate() {
                                    egui::Frame::none()
                                        .fill(card_bg)
                                        .stroke(border_stroke)
                                        .rounding(Rounding::same(6.0))
                                        .inner_margin(egui::Margin::same(card_margin))
                                        .show(ui, |ui| {
                                            ui.set_width(card_inner_w);
                                            ui.vertical_centered(|ui| {
                                                let img_resp = if let Some(texture) = self.texture_cache.get(&item.id) {
                                                    let img_btn = egui::ImageButton::new((texture.id(), Vec2::new(card_inner_w, 154.0)))
                                                        .frame(false);
                                                    ui.add(img_btn)
                                                } else {
                                                    ui.allocate_response(Vec2::new(card_inner_w, 154.0), egui::Sense::click())
                                                };

                                                let img_resp = img_resp.on_hover_text("🔍 点击放大预览此图片");
                                                if img_resp.clicked() {
                                                    preview_action = Some(idx);
                                                }

                                                ui.add_space(4.0);

                                                let display_name = if item.name.chars().count() > 14 {
                                                    format!("{}...", item.name.chars().take(12).collect::<String>())
                                                } else {
                                                    item.name.clone()
                                                };

                                                ui.label(
                                                    egui::RichText::new(format!("#{}: {}", idx + 1, display_name))
                                                        .font(FontId::proportional(11.0))
                                                        .strong(),
                                                );

                                                ui.label(
                                                    egui::RichText::new(format!("{} × {}", item.width, item.height))
                                                        .font(FontId::proportional(10.0))
                                                        .color(text_muted),
                                                );

                                                ui.add_space(4.0);

                                                ui.horizontal(|ui| {
                                                    ui.spacing_mut().item_spacing = Vec2::new(4.0, 0.0);
                                                    ui.spacing_mut().button_padding = Vec2::new(4.0, 3.0);

                                                    if ui.add_sized([26.0, 22.0], egui::Button::new("◀")).on_hover_text("前移").clicked() {
                                                        swap_action = Some((idx, idx.saturating_sub(1)));
                                                    }
                                                    if ui.add_sized([26.0, 22.0], egui::Button::new("▶")).on_hover_text("后移").clicked() {
                                                        swap_action = Some((idx, idx + 1));
                                                    }
                                                    if ui.add_sized([26.0, 22.0], egui::Button::new("🔍")).on_hover_text("放大预览").clicked() {
                                                        preview_action = Some(idx);
                                                    }
                                                    if ui.add_sized([26.0, 22.0], egui::Button::new("×")).on_hover_text("移除").clicked() {
                                                        remove_action = Some(idx);
                                                    }
                                                });
                                            });
                                        });

                                    if (idx + 1) % cols == 0 {
                                        ui.end_row();
                                    }
                                }
                            });

                    if let Some((from, to)) = swap_action {
                        let seq = self.current_sequence_mut();
                        if to < seq.items.len() && from != to {
                            seq.items.swap(from, to);
                        }
                    }

                    if let Some(idx) = remove_action {
                        let seq = self.current_sequence_mut();
                        if idx < seq.items.len() {
                            let removed_id = seq.items[idx].id;
                            seq.remove_at(idx);
                            self.texture_cache.remove(&removed_id);
                        }
                    }

                    if let Some(idx) = preview_action {
                        self.preview_image_idx = Some(idx);
                    }
                });
            }
        });

        // 5. 关于对话框
        if self.show_about_dialog {
            let github_url = "https://github.com/1738348785/MangaPDF-rust";
            let mut close_dialog = false;

            egui::Window::new("关于 MangaPDF")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
                .default_width(380.0)
                .show(ctx, |ui| {
                    ui.spacing_mut().item_spacing = Vec2::new(8.0, 7.0);

                    // 顶部品牌区
                    ui.vertical_centered(|ui| {
                        ui.add(
                            egui::Image::new(&app_logo)
                                .max_size(Vec2::splat(48.0))
                                .rounding(Rounding::same(10.0))
                        );
                        ui.add_space(2.0);
                        ui.heading("MangaPDF 漫画与图片打包工具");
                        ui.add_space(2.0);
                        ui.label(egui::RichText::new("版本 v1.0.2 · 现代轻量 GUI").color(text_muted));
                        ui.add_space(4.0);

                        // GitHub 小图标与小字转跳链接
                        let label_text = "GitHub ↗";
                        let font_id = FontId::proportional(12.0);
                        let icon_size = 15.0;
                        let gap = 5.0;
                        let text_w = ui.fonts(|f| f.layout_no_wrap(label_text.to_string(), font_id.clone(), text_muted).size().x);
                        let total_w = icon_size + gap + text_w;

                        let (rect, resp) = ui.allocate_exact_size(Vec2::new(total_w, 20.0), egui::Sense::click());
                        let is_hovered = resp.hovered();
                        let is_clicked = resp.clicked();
                        resp.on_hover_text("在浏览器中打开 GitHub 开源仓库 ↗\nhttps://github.com/1738348785/MangaPDF-rust");

                        if is_clicked {
                            ctx.open_url(egui::OpenUrl::new_tab(github_url));
                        }
                        if is_hovered {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                        }

                        let color = if is_hovered {
                            primary_color
                        } else {
                            text_muted
                        };

                        let icon_rect = egui::Rect::from_min_size(
                            egui::pos2(rect.min.x, rect.min.y + (rect.height() - icon_size) / 2.0),
                            Vec2::splat(icon_size),
                        );
                        ui.painter().image(
                            github_logo.id(),
                            icon_rect,
                            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                            color,
                        );
                        let text_pos = egui::pos2(
                            rect.min.x + icon_size + gap,
                            rect.min.y + (rect.height() - 14.0) / 2.0,
                        );
                        ui.painter().text(
                            text_pos,
                            egui::Align2::LEFT_TOP,
                            label_text,
                            font_id,
                            color,
                        );
                    });

                    ui.add_space(8.0);
                    ui.separator();
                    ui.add_space(6.0);

                    // 靠左整齐排列特性要点
                    ui.with_layout(Layout::top_down(Align::Min), |ui| {
                        ui.label("• 高清画廊大图预览 (自适应窗口、1:1 原图与键盘快捷翻页)");
                        ui.label("• 原画无损直存 (JPEG 直通 + PNG 原生无损保留)");
                        ui.label("• 多核并行转码加速 (Rayon 线程池分批并发满载)");
                        ui.label("• 智能全路径自然语义排序 (1.jpg -> 2.jpg -> 10.jpg)");
                        ui.label("• 深度支持任意嵌套子文件夹扫描与多卷自动拆分");
                        ui.label("• 压缩画质动态重编 (10%~100% 自由可调)");
                        ui.label("• 页面尺寸无缝自适应 (消除黑边与留白)");
                        ui.label("• 纯本地静默生成 · 零依赖绿色运行");
                    });

                    ui.add_space(14.0);
                    ui.vertical_centered(|ui| {
                        let confirm_btn = egui::Button::new(
                            egui::RichText::new("确定").color(Color32::WHITE).strong()
                        ).fill(primary_color);
                        if ui.add_sized([80.0, 28.0], confirm_btn).clicked() {
                            close_dialog = true;
                        }
                    });
                });

            if close_dialog {
                self.show_about_dialog = false;
            }
        }

        // 5.5 图片大图预览浮窗 / 画廊查看器
        if let Some(cur_idx) = self.preview_image_idx {
            let page_info = {
                let current_seq = self.current_sequence();
                if cur_idx >= current_seq.items.len() {
                    None
                } else {
                    let item = &current_seq.items[cur_idx];
                    Some((
                        item.id,
                        item.name.clone(),
                        item.width,
                        item.height,
                        current_seq.items.len(),
                        item.get_bytes(),
                    ))
                }
            };

            if let Some((item_id, item_name, item_width, item_height, total_pages, bytes_res)) = page_info {
                // 载入大图的高清渲染纹理 (若尚未缓存此图)
                if self.preview_texture.as_ref().map(|(id, _)| *id) != Some(item_id) {
                    if let Ok(bytes) = bytes_res {
                        if let Ok(dyn_img) = image::load_from_memory(&bytes) {
                            let max_dim = 2560;
                            let img_to_show = if dyn_img.width() > max_dim || dyn_img.height() > max_dim {
                                dyn_img.resize(max_dim, max_dim, image::imageops::FilterType::Triangle)
                            } else {
                                dyn_img
                            };
                            let rgba = img_to_show.to_rgba8();
                            let color_img = egui::ColorImage::from_rgba_unmultiplied(
                                [rgba.width() as usize, rgba.height() as usize],
                                &rgba,
                            );
                            let tex = ctx.load_texture(
                                "large_preview_texture",
                                color_img,
                                egui::TextureOptions::LINEAR,
                            );
                            self.preview_texture = Some((item_id, tex));
                        }
                    }
                }

                let mut close_preview = false;
                let mut switch_idx = None;
                let mut is_open = true;

                let window_title = format!(
                    "图片预览 (第 {}/{} 页) - {}",
                    cur_idx + 1, total_pages, item_name
                );

                let screen_rect = ctx.screen_rect();
                let default_w = (screen_rect.width() * 0.85).min(1080.0);
                let default_h = (screen_rect.height() * 0.88).min(840.0);
                let default_pos = egui::pos2(
                    (screen_rect.width() - default_w).max(0.0) / 2.0 + screen_rect.min.x,
                    (screen_rect.height() - default_h).max(0.0) / 2.0 + screen_rect.min.y,
                );

                egui::Window::new(window_title)
                    .id(egui::Id::new("image_preview_window"))
                    .open(&mut is_open)
                    .default_pos(default_pos)
                    .default_size(Vec2::new(default_w, default_h))
                    .min_size(Vec2::new(420.0, 320.0))
                    .resizable(true)
                    .collapsible(false)
                    .order(egui::Order::Foreground)
                    .show(ctx, |ui| {
                        // 快捷键支持：← 上一页，→ 下一页，Esc 关闭
                        if ui.input(|i| i.key_pressed(egui::Key::ArrowLeft)) && cur_idx > 0 {
                            switch_idx = Some(cur_idx - 1);
                        }
                        if ui.input(|i| i.key_pressed(egui::Key::ArrowRight)) && cur_idx + 1 < total_pages {
                            switch_idx = Some(cur_idx + 1);
                        }
                        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                            close_preview = true;
                        }

                        // 预览控制栏 (极简整洁，无重复关闭按钮)
                        ui.horizontal(|ui| {
                            if ui.add_enabled(cur_idx > 0, egui::Button::new("◀ 上一页 (←)")).clicked() {
                                switch_idx = Some(cur_idx.saturating_sub(1));
                            }
                            if ui.add_enabled(cur_idx + 1 < total_pages, egui::Button::new("下一页 (→) ▶")).clicked() {
                                switch_idx = Some(cur_idx + 1);
                            }

                            ui.add_space(8.0);
                            ui.label(egui::RichText::new(format!("第 {} / {} 页", cur_idx + 1, total_pages)).strong());

                            ui.add_space(10.0);
                            ui.separator();
                            ui.add_space(10.0);

                            ui.selectable_value(&mut self.preview_actual_size, false, "适应窗口");
                            ui.selectable_value(&mut self.preview_actual_size, true, "100% 原始尺寸");

                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.label(egui::RichText::new(format!("{} × {} px · 按 Esc 退出", item_width, item_height)).color(text_muted));
                            });
                        });

                        ui.separator();

                        // 图片展示区：平滑居中自适应缩放
                        if let Some((_, tex)) = &self.preview_texture {
                            if self.preview_actual_size {
                                egui::ScrollArea::both()
                                    .auto_shrink([false, false])
                                    .show(ui, |ui| {
                                        ui.image((tex.id(), tex.size_vec2()));
                                    });
                            } else {
                                let avail_size = ui.available_size();
                                if avail_size.x > 10.0 && avail_size.y > 10.0 {
                                    let tex_size = tex.size_vec2();
                                    let aspect_ratio = tex_size.x / tex_size.y.max(1.0);
                                    let mut fit_w = avail_size.x;
                                    let mut fit_h = fit_w / aspect_ratio;
                                    if fit_h > avail_size.y {
                                        fit_h = avail_size.y;
                                        fit_w = fit_h * aspect_ratio;
                                    }

                                    let x_pad = (avail_size.x - fit_w) / 2.0;
                                    let y_pad = (avail_size.y - fit_h) / 2.0;

                                    ui.horizontal(|ui| {
                                        if x_pad > 0.0 {
                                            ui.add_space(x_pad);
                                        }
                                        ui.vertical(|ui| {
                                            if y_pad > 0.0 {
                                                ui.add_space(y_pad);
                                            }
                                            ui.image((tex.id(), Vec2::new(fit_w, fit_h)));
                                        });
                                    });
                                }
                            }
                        } else {
                            ui.centered_and_justified(|ui| {
                                ui.label("正在加载大图预览...");
                            });
                        }
                    });

                if !is_open || close_preview {
                    self.preview_image_idx = None;
                    self.preview_texture = None;
                    trim_process_memory();
                } else if let Some(new_idx) = switch_idx {
                    self.preview_image_idx = Some(new_idx);
                }
            } else {
                self.preview_image_idx = None;
                self.preview_texture = None;
                trim_process_memory();
            }
        }

        // 6. 拖拽文件悬停窗口时的视觉高亮（仅在窗口边缘勾勒一圈精致的主题色描边，绝不遮挡用户界面）
        if is_hovering_files {
            let screen_rect = ctx.screen_rect();
            ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("window_drag_outline")))
                .rect_stroke(
                    screen_rect.shrink(1.0),
                    Rounding::ZERO,
                    Stroke::new(2.5_f32, primary_color),
                );
        }
    }
}

fn open_in_file_explorer(path: &std::path::Path) {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        let mut cmd = std::process::Command::new("explorer.exe");
        cmd.creation_flags(CREATE_NO_WINDOW);

        if path.exists() {
            if path.is_file() {
                cmd.raw_arg(format!("/select,\"{}\"", path.display()));
            } else {
                cmd.raw_arg(format!("\"{}\"", path.display()));
            }
        } else if let Some(parent) = path.parent() {
            if parent.exists() {
                cmd.raw_arg(format!("\"{}\"", parent.display()));
            }
        } else {
            cmd.raw_arg(format!("\"{}\"", path.display()));
        }

        let _ = cmd.spawn();
    }
    #[cfg(not(target_os = "windows"))]
    {
        let target = if path.is_file() {
            path.parent().unwrap_or(path)
        } else {
            path
        };
        let _ = open::that(target);
    }
}

/// 智能净化 Windows 文件名，将非法字符转换为友好全角字符或合法字符
pub fn sanitize_filename(name: &str) -> String {
    let mut sanitized = String::with_capacity(name.len());
    for c in name.chars() {
        match c {
            '?' => sanitized.push('？'),       // 中文全角问号 (保留问号原意)
            ':' => sanitized.push('：'),       // 中文全角冒号
            '*' => sanitized.push('×'),        // 乘号替代星号
            '/' | '\\' => sanitized.push('-'), // 连字符替代斜杠
            '<' => sanitized.push('《'),       // 书名号替代尖括号
            '>' => sanitized.push('》'),
            '|' => sanitized.push('_'),
            '"' => sanitized.push('\''),
            c if c.is_control() => sanitized.push('_'),
            c => sanitized.push(c),
        }
    }

    let trimmed = sanitized.trim_matches(|c: char| c.is_whitespace() || c == '.');
    if trimmed.is_empty() {
        "未命名分卷".to_string()
    } else {
        trimmed.to_string()
    }
}

/// 针对 Windows 进程主动修剪工作集内存，将启动阶段显卡驱动预分配的大量非活跃编译缓存归还系统
#[cfg(target_os = "windows")]
pub fn trim_process_memory() {
    unsafe {
        #[link(name = "kernel32")]
        extern "system" {
            fn GetCurrentProcess() -> isize;
            fn SetProcessWorkingSetSize(
                h_process: isize,
                dw_minimum_working_set_size: usize,
                dw_maximum_working_set_size: usize,
            ) -> i32;
        }
        SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX);
    }
}

#[cfg(not(target_os = "windows"))]
pub fn trim_process_memory() {}
