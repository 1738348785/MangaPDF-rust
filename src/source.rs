use crate::img::create_image_entry;
use crate::model::{
    compare_natural_path_str, compute_relative_sort_keys, read_zip_entry, ArchiveEntryRef,
    ImageEntry,
};
use crate::util::{decode_zip_entry_names, RawZipName};
use anyhow::{Context, Result};
use rayon::prelude::*;
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

pub const SUPPORTED_IMG_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "webp", "bmp", "jfif"];

pub const SUPPORTED_ARCHIVE_EXTENSIONS: &[&str] = &["zip", "cbz"];

/// 常见但当前暂不支持的漫画归档/图像扩展名，用于在导入引导中给出清晰点名提示
pub const KNOWN_UNSUPPORTED_MEDIA_EXTENSIONS: &[&str] = &[
    "cbr", "rar", "cb7", "7z", "tar", "gif", "tif", "tiff", "avif", "heic", "heif", "psd", "pdf",
    "jxl", "tga", "ico", "svg",
];

#[inline]
fn is_cancelled(cancel_flag: Option<&AtomicBool>) -> bool {
    cancel_flag.is_some_and(|flag| flag.load(Ordering::Relaxed))
}

pub fn is_image_extension(ext: &str) -> bool {
    SUPPORTED_IMG_EXTENSIONS
        .iter()
        .any(|&e| e.eq_ignore_ascii_case(ext))
}

pub fn is_archive(path: &Path) -> bool {
    if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
        SUPPORTED_ARCHIVE_EXTENSIONS
            .iter()
            .any(|&e| e.eq_ignore_ascii_case(ext))
    } else {
        false
    }
}

pub fn is_known_unsupported_media_ext(ext: &str) -> bool {
    KNOWN_UNSUPPORTED_MEDIA_EXTENSIONS
        .iter()
        .any(|&e| e.eq_ignore_ascii_case(ext))
}

/// 检查是否为应该忽略的系统隐藏目录
pub fn is_system_hidden_dir(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower == "__macosx"
        || lower.starts_with('.')
        || lower == "$recycle.bin"
        || lower == "system volume information"
}

/// 收集目录下的图片文件 (支持递归开关、取消信号与智能过滤)
pub fn collect_image_files(
    dir: &Path,
    recursive: bool,
    cancel_flag: Option<&AtomicBool>,
) -> Vec<PathBuf> {
    let mut files = Vec::new();

    if !recursive {
        // 非递归模式：仅收集当前目录直属的图片文件（彻底忽略所有子文件夹，包括蒙版和翻译缓存）
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                if is_cancelled(cancel_flag) {
                    return Vec::new();
                }
                let path = entry.path();
                if path.is_file() {
                    if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
                        if is_image_extension(ext) {
                            files.push(path);
                        }
                    }
                }
            }
        }
    } else {
        // 递归模式：扫描所有子目录（完全忠实执行用户指令，仅跳过系统隐藏文件夹与符号链接目录防死循环）
        let mut stack = vec![dir.to_path_buf()];
        while let Some(current_dir) = stack.pop() {
            if is_cancelled(cancel_flag) {
                return Vec::new();
            }
            if let Ok(entries) = fs::read_dir(&current_dir) {
                for entry in entries.flatten() {
                    if is_cancelled(cancel_flag) {
                        return Vec::new();
                    }
                    let is_symlink = entry.file_type().is_ok_and(|ft| ft.is_symlink());
                    let path = entry.path();
                    if path.is_dir() {
                        if is_symlink {
                            continue;
                        }
                        let dir_name = path.file_name().unwrap_or_default().to_string_lossy();
                        if !is_system_hidden_dir(&dir_name) {
                            stack.push(path);
                        }
                    } else if path.is_file() {
                        if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
                            if is_image_extension(ext) {
                                files.push(path);
                            }
                        }
                    }
                }
            }
        }
    }

    files.sort_by(|a, b| compare_natural_paths(a, b));

    files
}

/// 收集目录下的 `.zip` / `.cbz` 漫画归档文件 (支持递归开关与自然排序)
pub fn collect_archive_files(
    dir: &Path,
    recursive: bool,
    cancel_flag: Option<&AtomicBool>,
) -> Vec<PathBuf> {
    let mut archives = Vec::new();

    if !recursive {
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                if is_cancelled(cancel_flag) {
                    return Vec::new();
                }
                let path = entry.path();
                if path.is_file() && is_archive(&path) {
                    archives.push(path);
                }
            }
        }
    } else {
        let mut stack = vec![dir.to_path_buf()];
        while let Some(current_dir) = stack.pop() {
            if is_cancelled(cancel_flag) {
                return Vec::new();
            }
            if let Ok(entries) = fs::read_dir(&current_dir) {
                for entry in entries.flatten() {
                    if is_cancelled(cancel_flag) {
                        return Vec::new();
                    }
                    let is_symlink = entry.file_type().is_ok_and(|ft| ft.is_symlink());
                    let path = entry.path();
                    if path.is_dir() {
                        if is_symlink {
                            continue;
                        }
                        let dir_name = path.file_name().unwrap_or_default().to_string_lossy();
                        if !is_system_hidden_dir(&dir_name) {
                            stack.push(path);
                        }
                    } else if path.is_file() && is_archive(&path) {
                        archives.push(path);
                    }
                }
            }
        }
    }

    archives.sort_by(|a, b| compare_natural_paths(a, b));
    archives
}

/// 扫描目录中常见的暂不支持漫画/图片格式扩展名（如 `.cbr` / `.rar` / `.7z` / `.avif`），用于无有效图片时的友好引导
pub fn collect_unsupported_media_exts_in_dir(
    dir: &Path,
    recursive: bool,
    cancel_flag: Option<&AtomicBool>,
) -> Vec<String> {
    let mut found = BTreeSet::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current_dir) = stack.pop() {
        if is_cancelled(cancel_flag) {
            break;
        }
        if let Ok(entries) = fs::read_dir(&current_dir) {
            for entry in entries.flatten() {
                if is_cancelled(cancel_flag) {
                    break;
                }
                let is_symlink = entry.file_type().is_ok_and(|ft| ft.is_symlink());
                let path = entry.path();
                if path.is_dir() {
                    if recursive && !is_symlink {
                        let dir_name = path.file_name().unwrap_or_default().to_string_lossy();
                        if !is_system_hidden_dir(&dir_name) {
                            stack.push(path);
                        }
                    }
                } else if path.is_file() {
                    if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
                        if is_known_unsupported_media_ext(ext) {
                            found.insert(format!(".{}", ext.to_ascii_lowercase()));
                        }
                    }
                }
            }
        }
    }
    found.into_iter().collect()
}

/// 按路径组件逐级进行自然语义比较，避免路径分隔符 `\` (ASCII 92) 与空格/连字符比较时发生顺序反转
pub fn compare_natural_paths(a: &Path, b: &Path) -> std::cmp::Ordering {
    let mut a_comps = a.components();
    let mut b_comps = b.components();
    loop {
        match (a_comps.next(), b_comps.next()) {
            (Some(ac), Some(bc)) => {
                let a_str = ac.as_os_str().to_string_lossy();
                let b_str = bc.as_os_str().to_string_lossy();
                let cmp = natord::compare_ignore_case(&a_str, &b_str);
                if cmp != std::cmp::Ordering::Equal {
                    return cmp;
                }
            }
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (None, None) => return a.cmp(b),
        }
    }
}

/// 从文件路径列表中加载图片 (按自然语言自然排序，支持分批并发、进度回调与中途取消)
///
/// 返回 (成功导入的条目, 问题列表)。读取失败的文件会被跳过；无法解码预览的图片仍会保留，
/// 但会记入问题列表提醒用户 (超大 JPEG 可能仍能直通写入 PDF)。
pub fn load_entries_from_paths_with_progress<F>(
    paths: &[PathBuf],
    mut start_id: usize,
    progress_cb: Option<&F>,
    cancel_flag: Option<&AtomicBool>,
) -> (Vec<ImageEntry>, Vec<String>)
where
    F: Fn(usize, usize, &str) + Send + Sync,
{
    if is_cancelled(cancel_flag) {
        return (Vec::new(), Vec::new());
    }

    let mut sorted_paths = paths.to_vec();
    sorted_paths.sort_by(|a, b| compare_natural_paths(a, b));

    let filtered_paths: Vec<PathBuf> = sorted_paths
        .into_iter()
        .filter(|p| {
            p.extension()
                .and_then(|s| s.to_str())
                .is_some_and(is_image_extension)
        })
        .collect();

    let path_strings: Vec<String> = filtered_paths
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    let sort_keys = compute_relative_sort_keys(&path_strings);

    let raw_files: Vec<(usize, String, String, PathBuf)> = filtered_paths
        .into_iter()
        .zip(sort_keys)
        .map(|(p, sort_key)| {
            let name = p
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let id = start_id;
            start_id += 1;
            (id, name, sort_key, p)
        })
        .collect();

    let total = raw_files.len();
    let counter = std::sync::atomic::AtomicUsize::new(0);
    let mut result = Vec::with_capacity(total);
    let mut errors = Vec::new();

    for chunk in raw_files.chunks(32) {
        if is_cancelled(cancel_flag) {
            break;
        }
        let chunk_entries: Vec<Option<Result<ImageEntry, String>>> = chunk
            .into_par_iter()
            .map(|(id, name, sort_key, path)| {
                if is_cancelled(cancel_flag) {
                    return None;
                }
                let entry = fs::read(path)
                    .map(|bytes| {
                        let mut e =
                            create_image_entry(*id, name.clone(), bytes, Some(path.clone()), None);
                        e.sort_key = sort_key.clone();
                        e
                    })
                    .map_err(|e| format!("{}: 读取失败 ({})", name, e));
                let done = counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                if let Some(cb) = progress_cb {
                    cb(done, total, name);
                }
                Some(entry)
            })
            .collect();
        if is_cancelled(cancel_flag) {
            break;
        }
        for res in chunk_entries.into_iter().flatten() {
            match res {
                Ok(entry) => {
                    if entry.thumb_rgb.is_none() {
                        errors.push(format!("{}: 无法解码预览，图片可能已损坏", entry.name));
                    }
                    result.push(entry);
                }
                Err(e) => errors.push(e),
            }
        }
    }

    (result, errors)
}

/// 从文件夹中加载所有图片 (可指定是否递归扫描，支持进度回调与中途取消)
pub fn load_entries_from_folder_with_progress<F>(
    dir: &Path,
    start_id: usize,
    recursive: bool,
    progress_cb: Option<&F>,
    cancel_flag: Option<&AtomicBool>,
) -> Result<(Vec<ImageEntry>, Vec<String>)>
where
    F: Fn(usize, usize, &str) + Send + Sync,
{
    let file_paths = collect_image_files(dir, recursive, cancel_flag);
    Ok(load_entries_from_paths_with_progress(
        &file_paths,
        start_id,
        progress_cb,
        cancel_flag,
    ))
}

/// 快速探测当前目录根一级是否直接包含至少一张有效图片
pub fn dir_has_root_images(dir: &Path) -> bool {
    fs::read_dir(dir)
        .map(|entries| {
            entries.flatten().any(|e| {
                let p = e.path();
                p.is_file()
                    && p.extension()
                        .and_then(|s| s.to_str())
                        .is_some_and(is_image_extension)
            })
        })
        .unwrap_or(false)
}

/// 快速探测目录树中是否存在至少一张有效图片（找到即短路返回，避免重复全量遍历排序）
fn dir_has_any_image(dir: &Path, cancel_flag: Option<&AtomicBool>) -> bool {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current_dir) = stack.pop() {
        // 「取消」在扫描阶段也必须生效：用户误拖入一个含几万个子目录的大目录时，
        // 只有让深层遍历也响应取消，按钮才不会看起来毫无反应。
        if is_cancelled(cancel_flag) {
            return false;
        }
        if let Ok(entries) = fs::read_dir(&current_dir) {
            for entry in entries.flatten() {
                let is_symlink = entry.file_type().is_ok_and(|ft| ft.is_symlink());
                let path = entry.path();
                if path.is_dir() {
                    if is_symlink {
                        continue;
                    }
                    let dir_name = path.file_name().unwrap_or_default().to_string_lossy();
                    if !is_system_hidden_dir(&dir_name) {
                        stack.push(path);
                    }
                } else if path.is_file() {
                    if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
                        if is_image_extension(ext) {
                            return true;
                        }
                    }
                }
            }
        }
    }
    false
}

/// 检查目录是否包含多个直接分卷子目录
pub fn get_direct_subfolders(dir: &Path, cancel_flag: Option<&AtomicBool>) -> Vec<PathBuf> {
    let mut subfolders = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            if is_cancelled(cancel_flag) {
                break;
            }
            let path = entry.path();
            if path.is_dir() {
                // 一级子目录同样要跳过目录联接/符号链接：否则 `D:\Manga\all -> C:\` 这类联接点
                // 会让下面的递归探测一路爬进整个磁盘，导入表现为长时间无响应。
                if entry.file_type().is_ok_and(|ft| ft.is_symlink()) {
                    continue;
                }
                let dir_name = path.file_name().unwrap_or_default().to_string_lossy();
                if is_system_hidden_dir(&dir_name) {
                    continue;
                }
                // 检查子目录内是否有有效图片
                if dir_has_any_image(&path, cancel_flag) {
                    subfolders.push(path);
                }
            }
        }
    }
    subfolders.sort_by(|a, b| compare_natural_paths(a, b));
    subfolders
}

/// 从 ZIP / CBZ 压缩包中分批读取图片生成缩略图（不常驻解压字节，严格控制峰值内存；
/// 自动探测非 UTF-8 中文 GBK / 日文 Shift-JIS / Big5 / CP949 文件名并按条目索引精准读取，支持中途取消）
///
/// 归档本身无法打开/解析时返回 Err；单个条目读取失败则跳过并记入问题列表。
pub fn load_entries_from_archive_with_progress<F>(
    archive_path: &Path,
    mut start_id: usize,
    progress_cb: Option<&F>,
    cancel_flag: Option<&AtomicBool>,
) -> Result<(Vec<ImageEntry>, Vec<String>)>
where
    F: Fn(usize, usize, &str) + Send + Sync,
{
    if is_cancelled(cancel_flag) {
        return Ok((Vec::new(), Vec::new()));
    }

    let file = File::open(archive_path)
        .with_context(|| format!("无法打开压缩文件: {}", archive_path.display()))?;
    let mut archive = zip::ZipArchive::new(file)
        .with_context(|| format!("无法解析 ZIP/CBZ 归档: {}", archive_path.display()))?;

    // 第一遍收集所有条目的原始字节名 + 「归档是否已声明它是 UTF-8」，整包联合探测 CJK 编码后再按路径
    // 分隔符拆解，彻底避免 Shift-JIS / GBK 第二字节 0x5C ('\') 导致的字符错切与乱码。
    //
    // `utf8` 的判定：zip 对**没有**置 UTF-8 标志的条目会用 CP437 展开（所有非 ASCII 字节都会变长），
    // 所以 `name() != name_raw()` 就 100% 说明该条目没有 UTF-8 标志、必须参与代码页探测；
    // 反之（包含 Info-ZIP `0x7075` Unicode Path 扩展字段生效时 raw 已被替换成 UTF-8 的情况）
    // 说明归档自己声明了编码，必须无条件信任，否则 GBK 里「目录」「页」这种合法 UTF-8 字节会被误判。
    let mut raw_names: Vec<Vec<u8>> = Vec::with_capacity(archive.len());
    let mut name_is_utf8: Vec<bool> = Vec::with_capacity(archive.len());
    for i in 0..archive.len() {
        let zip_file = archive.by_index_raw(i)?;
        let raw = zip_file.name_raw();
        name_is_utf8.push(zip_file.name().as_bytes() == raw);
        raw_names.push(raw.to_vec());
    }
    let entries: Vec<RawZipName<'_>> = raw_names
        .iter()
        .zip(name_is_utf8.iter())
        .map(|(raw, &utf8)| RawZipName::new(raw.as_slice(), utf8))
        .collect();
    let decoded_names = decode_zip_entry_names(&entries);

    let mut valid_entries = Vec::new();
    for (i, decoded_name) in decoded_names.into_iter().enumerate() {
        if decoded_name.ends_with('/') || decoded_name.ends_with('\\') {
            continue;
        }
        let is_ignored = decoded_name.split(['/', '\\']).any(is_system_hidden_dir);
        if is_ignored {
            continue;
        }
        let file_name = decoded_name
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(&decoded_name);
        if let Some(ext) = Path::new(file_name).extension().and_then(|s| s.to_str()) {
            if is_image_extension(ext) {
                valid_entries.push((i, decoded_name));
            }
        }
    }

    valid_entries.sort_by(|a, b| compare_natural_path_str(&a.1, &b.1));
    let entry_names: Vec<String> = valid_entries.iter().map(|(_, n)| n.clone()).collect();
    let sort_keys = compute_relative_sort_keys(&entry_names);
    let valid_with_keys: Vec<(usize, String, String)> = valid_entries
        .into_iter()
        .zip(sort_keys)
        .map(|((idx, name), key)| (idx, name, key))
        .collect();

    let total = valid_with_keys.len();
    let counter = std::sync::atomic::AtomicUsize::new(0);
    let archive_arc = std::sync::Arc::new(archive_path.to_path_buf());
    let mut entries = Vec::with_capacity(total);
    let mut errors = Vec::new();

    for chunk in valid_with_keys.chunks(32) {
        if is_cancelled(cancel_flag) {
            break;
        }
        let mut raw_chunk = Vec::with_capacity(chunk.len());
        for (index, entry_name, sort_key) in chunk {
            if is_cancelled(cancel_flag) {
                break;
            }
            let display_name = entry_name
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or(entry_name)
                .to_string();
            let read_res = archive.by_index(*index).and_then(|zip_file| {
                let declared = zip_file.size();
                read_zip_entry(zip_file, declared, entry_name)
                    .map_err(zip::result::ZipError::from)
            });
            let bytes = match read_res {
                Ok(b) => b,
                Err(e) => {
                    errors.push(format!("{}: 解压失败 ({})", entry_name, e));
                    counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    continue;
                }
            };
            let id = start_id;
            start_id += 1;
            raw_chunk.push((
                id,
                *index,
                display_name,
                sort_key.clone(),
                entry_name.clone(),
                bytes,
            ));
        }

        if is_cancelled(cancel_flag) {
            break;
        }

        let chunk_processed: Vec<Option<ImageEntry>> = raw_chunk
            .into_par_iter()
            .map(|(id, entry_index, display_name, sort_key, entry_name, bytes)| {
                if is_cancelled(cancel_flag) {
                    return None;
                }
                let mut entry = create_image_entry(
                    id,
                    display_name.clone(),
                    bytes,
                    None,
                    Some(ArchiveEntryRef {
                        archive_path: archive_arc.clone(),
                        entry_index,
                        decoded_path: entry_name,
                    }),
                );
                entry.sort_key = sort_key;
                let done = counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                if let Some(cb) = progress_cb {
                    cb(done, total, &display_name);
                }
                Some(entry)
            })
            .collect();

        if is_cancelled(cancel_flag) {
            break;
        }

        for entry in chunk_processed.into_iter().flatten() {
            if entry.thumb_rgb.is_none() {
                errors.push(format!("{}: 无法解码预览，图片可能已损坏", entry.name));
            }
            entries.push(entry);
        }
    }

    Ok((entries, errors))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_natural_path_sorting_respects_directory_boundaries() {
        let mut paths = vec![
            PathBuf::from(r"E:\Manga\第10话\01.jpg"),
            PathBuf::from(r"E:\Manga\第1话 番外\01.jpg"),
            PathBuf::from(r"E:\Manga\第1话\02.jpg"),
            PathBuf::from(r"E:\Manga\第1话\01.jpg"),
        ];
        paths.sort_by(|a, b| compare_natural_paths(a, b));
        assert_eq!(
            paths,
            vec![
                PathBuf::from(r"E:\Manga\第1话\01.jpg"),
                PathBuf::from(r"E:\Manga\第1话\02.jpg"),
                PathBuf::from(r"E:\Manga\第1话 番外\01.jpg"),
                PathBuf::from(r"E:\Manga\第10话\01.jpg"),
            ]
        );
    }

    #[test]
    fn test_relative_sort_keys_preserve_multi_chapter_hierarchy() {
        let multi = vec![
            "Manga/Ch01/001.jpg".to_string(),
            "Manga/Ch02/001.jpg".to_string(),
        ];
        assert_eq!(
            compute_relative_sort_keys(&multi),
            vec!["Ch01/001.jpg", "Ch02/001.jpg"]
        );

        let single_dir = vec!["Vol01/001.jpg".to_string(), "Vol01/002.jpg".to_string()];
        assert_eq!(
            compute_relative_sort_keys(&single_dir),
            vec!["001.jpg", "002.jpg"]
        );
    }

    #[test]
    fn test_multi_batch_append_natural_sort_keeps_chapters_ordered() {
        use crate::model::Sequence;
        use std::sync::Arc;

        let make_entry = |id: usize, path_str: &str| {
            let p = PathBuf::from(path_str);
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            ImageEntry {
                id,
                name: name.clone(),
                // 模拟分批导入时单批内部剥离目录后仅剩文件名
                sort_key: name,
                source_path: Some(p),
                archive_source: None,
                bytes: Arc::from(&[][..]),
                width: 100,
                height: 100,
                thumb_rgb: None,
                thumb_width: 0,
                thumb_height: 0,
                thumb_error: None,
            }
        };

        let mut seq = Sequence::new(1, "合集".to_string());
        seq.items.push(make_entry(1, r"E:\Manga\Ch02\001.jpg"));
        seq.items.push(make_entry(2, r"E:\Manga\Ch01\002.jpg"));
        seq.items.push(make_entry(3, r"E:\Manga\Ch02\002.jpg"));
        seq.items.push(make_entry(4, r"E:\Manga\Ch01\001.jpg"));

        seq.natural_sort();

        let sorted_keys: Vec<&str> = seq.items.iter().map(|i| i.sort_key.as_str()).collect();
        assert_eq!(
            sorted_keys,
            vec![
                "Ch01/001.jpg",
                "Ch01/002.jpg",
                "Ch02/001.jpg",
                "Ch02/002.jpg"
            ]
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn test_archive_import_decodes_shift_jis_0x5c_and_reads_by_index() {
        use std::io::Write;
        use zip::write::SimpleFileOptions;

        let mut png_bytes = Vec::new();
        let img = image::GrayImage::from_fn(8, 8, |x, _| image::Luma([x as u8 * 25]));
        img.write_to(
            &mut std::io::Cursor::new(&mut png_bytes),
            image::ImageFormat::Png,
        )
        .unwrap();

        // Shift-JIS 原始字节："第2話/001.png" (13B) 和 "第1話/表紙.png" (14B，含 0x5C '\' 的「表」[0x95, 0x5C])
        let sjis_ch2: &[u8] = &[
            0x91, 0xE6, b'2', 0x98, 0x62, b'/', b'0', b'0', b'1', b'.', b'p', b'n', b'g',
        ];
        let sjis_ch1: &[u8] = &[
            0x91, 0xE6, b'1', 0x98, 0x62, b'/', 0x95, 0x5C, 0x8E, 0x86, b'.', b'p', b'n', b'g',
        ];
        let ph_ch2 = b"xxxx2/001.png";
        let ph_ch1 = b"xxxx1/yyyy.png";
        assert_eq!(sjis_ch2.len(), ph_ch2.len());
        assert_eq!(sjis_ch1.len(), ph_ch1.len());

        let mut zip_buf = Vec::new();
        {
            let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut zip_buf));
            let opts = SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            writer
                .start_file(std::str::from_utf8(ph_ch2).unwrap(), opts)
                .unwrap();
            writer.write_all(&png_bytes).unwrap();
            writer
                .start_file(std::str::from_utf8(ph_ch1).unwrap(), opts)
                .unwrap();
            writer.write_all(&png_bytes).unwrap();
            writer.finish().unwrap();
        }

        // 将本地头与中央目录中的等长 ASCII 占位符替换为原始 Shift-JIS 字节（模拟未置 UTF-8 标志的日文 ZIP）
        for (ph, raw) in [(ph_ch2.as_slice(), sjis_ch2), (ph_ch1.as_slice(), sjis_ch1)] {
            let mut idx = 0;
            while idx + ph.len() <= zip_buf.len() {
                if &zip_buf[idx..idx + ph.len()] == ph {
                    zip_buf[idx..idx + ph.len()].copy_from_slice(raw);
                    idx += ph.len();
                } else {
                    idx += 1;
                }
            }
        }

        let zip_path = std::env::temp_dir().join(format!(
            "mangapdf_sjis_test_{}.cbz",
            std::process::id()
        ));
        fs::write(&zip_path, &zip_buf).unwrap();

        let (entries, errors) = load_entries_from_archive_with_progress(
            &zip_path,
            1,
            None::<&fn(usize, usize, &str)>,
            None,
        )
        .unwrap();

        assert!(errors.is_empty(), "不应有解码错误: {:?}", errors);
        assert_eq!(entries.len(), 2);
        // 应按自然排序排成 第1話 在前、第2話 在后
        assert_eq!(entries[0].name, "表紙.png");
        assert_eq!(entries[0].sort_key, "第1話/表紙.png");
        assert_eq!(entries[1].name, "001.png");
        assert_eq!(entries[1].sort_key, "第2話/001.png");
        // 按条目索引二次读取原图字节应完全一致
        assert_eq!(entries[0].get_bytes().unwrap(), png_bytes);
        assert_eq!(entries[1].get_bytes().unwrap(), png_bytes);

        let _ = fs::remove_file(&zip_path);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn test_gbk_archive_does_not_split_one_directory_into_two() {
        use std::io::Write;
        use zip::write::SimpleFileOptions;

        let mut png_bytes = Vec::new();
        let img = image::GrayImage::from_fn(8, 8, |x, _| image::Luma([x as u8 * 25]));
        img.write_to(
            &mut std::io::Cursor::new(&mut png_bytes),
            image::ImageFormat::Png,
        )
        .unwrap();

        // GBK 原始字节："目录/001.jpg"(12B，全部字节恰好是合法 UTF-8，会被读成 "Ŀ¼/001.jpg")
        // 与 "目录/封面.jpg"(13B，不是合法 UTF-8)。旧实现按「能 utf8 就 utf8」逐条短路，
        // 于是同一个「目录」被拆成 Ŀ¼ 与 目录 两个目录，排序键错乱、封面被排到最后一页。
        let gbk_index: &[u8] = &[
            0xC4, 0xBF, 0xC2, 0xBC, b'/', b'0', b'0', b'1', b'.', b'j', b'p', b'g',
        ];
        let gbk_cover: &[u8] = &[
            0xC4, 0xBF, 0xC2, 0xBC, b'/', 0xB7, 0xE2, 0xC3, 0xE6, b'.', b'j', b'p', b'g',
        ];
        let ph_index = b"dddd/001.jpg";
        let ph_cover = b"dddd/yyyy.jpg";
        assert_eq!(gbk_index.len(), ph_index.len());
        assert_eq!(gbk_cover.len(), ph_cover.len());
        assert_eq!(std::str::from_utf8(&gbk_index[..4]).unwrap(), "Ŀ¼");

        let mut zip_buf = Vec::new();
        {
            let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut zip_buf));
            let opts = SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            for ph in [ph_index.as_slice(), ph_cover.as_slice()] {
                writer.start_file(std::str::from_utf8(ph).unwrap(), opts).unwrap();
                writer.write_all(&png_bytes).unwrap();
            }
            writer.finish().unwrap();
        }
        // 把等长 ASCII 占位符换成 GBK 原始字节，模拟未置 UTF-8 标志的国产打包工具产物
        for (ph, raw) in [(ph_index.as_slice(), gbk_index), (ph_cover.as_slice(), gbk_cover)] {
            let mut idx = 0;
            while idx + ph.len() <= zip_buf.len() {
                if &zip_buf[idx..idx + ph.len()] == ph {
                    zip_buf[idx..idx + ph.len()].copy_from_slice(raw);
                    idx += ph.len();
                } else {
                    idx += 1;
                }
            }
        }

        let zip_path = std::env::temp_dir().join(format!(
            "mangapdf_gbk_dir_test_{}.cbz",
            std::process::id()
        ));
        fs::write(&zip_path, &zip_buf).unwrap();

        let (entries, errors) = load_entries_from_archive_with_progress(
            &zip_path,
            1,
            None::<&fn(usize, usize, &str)>,
            None,
        )
        .unwrap();

        assert!(errors.is_empty(), "不应有解码错误: {:?}", errors);
        assert_eq!(entries.len(), 2);
        // 两条名字必须落在同一个「目录」下：排序键剥掉公共祖先目录后只剩文件名
        assert_eq!(entries[0].sort_key, "001.jpg");
        assert_eq!(entries[1].sort_key, "封面.jpg");
        assert_eq!(entries[1].name, "封面.jpg");
        assert_eq!(entries[0].get_bytes().unwrap(), png_bytes);
        assert_eq!(entries[1].get_bytes().unwrap(), png_bytes);

        let _ = fs::remove_file(&zip_path);
    }

    #[test]
    fn test_collect_archive_files_natural_order_and_cancel() {
        let dir = std::env::temp_dir().join(format!("mangapdf_arch_scan_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("Vol10.cbz"), b"").unwrap();
        fs::write(dir.join("Vol2.cbz"), b"").unwrap();
        fs::write(dir.join("Vol1.zip"), b"").unwrap();
        fs::write(dir.join("note.txt"), b"").unwrap();

        let archives = collect_archive_files(&dir, false, None);
        let names: Vec<String> = archives
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["Vol1.zip", "Vol2.cbz", "Vol10.cbz"]);

        let cancel = AtomicBool::new(true);
        let cancelled = collect_archive_files(&dir, false, Some(&cancel));
        assert!(cancelled.is_empty());

        let _ = fs::remove_dir_all(&dir);
    }
}
