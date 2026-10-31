use crate::img::create_image_entry;
use crate::model::{compare_natural_path_str, ImageEntry, MAX_ZIP_PREALLOC_BYTES};
use anyhow::{Context, Result};
use rayon::prelude::*;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

pub const SUPPORTED_IMG_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "webp", "bmp", "jfif"];

pub const SUPPORTED_ARCHIVE_EXTENSIONS: &[&str] = &["zip", "cbz"];

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

/// 检查是否为应该忽略的系统隐藏目录
pub fn is_system_hidden_dir(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower == "__macosx"
        || lower.starts_with('.')
        || lower == "$recycle.bin"
        || lower == "system volume information"
}

/// 收集目录下的图片文件 (支持递归开关与智能过滤)
pub fn collect_image_files(dir: &Path, recursive: bool) -> Vec<PathBuf> {
    let mut files = Vec::new();

    if !recursive {
        // 非递归模式：仅收集当前目录直属的图片文件（彻底忽略所有子文件夹，包括蒙版和翻译缓存）
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
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
        // 递归模式：扫描所有子目录（完全忠实执行用户指令，仅跳过系统隐藏文件夹）
        let mut stack = vec![dir.to_path_buf()];
        while let Some(current_dir) = stack.pop() {
            if let Ok(entries) = fs::read_dir(&current_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
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

/// 提取一组路径剥离公共祖先目录后的相对排序键（如 `Ch01/001.jpg`；单层目录则简化为 `001.jpg`）
fn compute_relative_sort_keys(raw_paths: &[String]) -> Vec<String> {
    if raw_paths.is_empty() {
        return Vec::new();
    }
    let split_all: Vec<Vec<&str>> = raw_paths
        .iter()
        .map(|p| p.split(['/', '\\']).filter(|s| !s.is_empty()).collect())
        .collect();

    let first_dirs = &split_all[0][..split_all[0].len().saturating_sub(1)];
    let mut common_len = first_dirs.len();
    for segs in &split_all[1..] {
        let dirs = &segs[..segs.len().saturating_sub(1)];
        common_len = common_len.min(dirs.len());
        for i in 0..common_len {
            if !first_dirs[i].eq_ignore_ascii_case(dirs[i]) && first_dirs[i] != dirs[i] {
                common_len = i;
                break;
            }
        }
    }

    split_all
        .into_iter()
        .map(|segs| {
            if common_len < segs.len() {
                segs[common_len..].join("/")
            } else {
                segs.last().copied().unwrap_or_default().to_string()
            }
        })
        .collect()
}

/// 从文件路径列表中加载图片 (按自然语言自然排序，支持分批并发与进度回调)
///
/// 返回 (成功导入的条目, 问题列表)。读取失败的文件会被跳过；无法解码预览的图片仍会保留，
/// 但会记入问题列表提醒用户 (超大 JPEG 可能仍能直通写入 PDF)。
pub fn load_entries_from_paths_with_progress<F>(
    paths: &[PathBuf],
    mut start_id: usize,
    progress_cb: Option<&F>,
) -> (Vec<ImageEntry>, Vec<String>)
where
    F: Fn(usize, usize, &str) + Send + Sync,
{
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
        let chunk_entries: Vec<Result<ImageEntry, String>> = chunk
            .into_par_iter()
            .map(|(id, name, sort_key, path)| {
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
                entry
            })
            .collect();
        for res in chunk_entries {
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

/// 从文件夹中加载所有图片 (可指定是否递归扫描，支持进度回调)
pub fn load_entries_from_folder_with_progress<F>(
    dir: &Path,
    start_id: usize,
    recursive: bool,
    progress_cb: Option<&F>,
) -> Result<(Vec<ImageEntry>, Vec<String>)>
where
    F: Fn(usize, usize, &str) + Send + Sync,
{
    let file_paths = collect_image_files(dir, recursive);
    Ok(load_entries_from_paths_with_progress(
        &file_paths,
        start_id,
        progress_cb,
    ))
}

/// 快速探测目录树中是否存在至少一张有效图片（找到即短路返回，避免重复全量遍历排序）
fn dir_has_any_image(dir: &Path) -> bool {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current_dir) = stack.pop() {
        if let Ok(entries) = fs::read_dir(&current_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
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
pub fn get_direct_subfolders(dir: &Path) -> Vec<PathBuf> {
    let mut subfolders = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let dir_name = path.file_name().unwrap_or_default().to_string_lossy();
                if is_system_hidden_dir(&dir_name) {
                    continue;
                }
                // 检查子目录内是否有有效图片
                if dir_has_any_image(&path) {
                    subfolders.push(path);
                }
            }
        }
    }
    subfolders.sort_by(|a, b| compare_natural_paths(a, b));
    subfolders
}

/// 从 ZIP / CBZ 压缩包中分批读取图片生成缩略图（不常驻解压字节，严格控制峰值内存）
///
/// 归档本身无法打开/解析时返回 Err；单个条目读取失败则跳过并记入问题列表。
pub fn load_entries_from_archive_with_progress<F>(
    archive_path: &Path,
    mut start_id: usize,
    progress_cb: Option<&F>,
) -> Result<(Vec<ImageEntry>, Vec<String>)>
where
    F: Fn(usize, usize, &str) + Send + Sync,
{
    let file = File::open(archive_path)
        .with_context(|| format!("无法打开压缩文件: {}", archive_path.display()))?;
    let mut archive = zip::ZipArchive::new(file)
        .with_context(|| format!("无法解析 ZIP/CBZ 归档: {}", archive_path.display()))?;

    let mut valid_entries = Vec::new();
    for i in 0..archive.len() {
        let zip_file = archive.by_index_raw(i)?;
        let name = zip_file.name().to_string();
        if !zip_file.is_dir() {
            if let Some(ext) = Path::new(&name).extension().and_then(|s| s.to_str()) {
                if is_image_extension(ext) {
                    let is_ignored = name.split('/').any(is_system_hidden_dir)
                        || name.split('\\').any(is_system_hidden_dir);
                    if !is_ignored {
                        valid_entries.push((i, name));
                    }
                }
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
        let mut raw_chunk = Vec::with_capacity(chunk.len());
        for (index, entry_name, sort_key) in chunk {
            let display_name = Path::new(entry_name)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let read_res = archive.by_index(*index).and_then(|mut zip_file| {
                let cap = zip_file.size().min(MAX_ZIP_PREALLOC_BYTES) as usize;
                let mut bytes = Vec::with_capacity(cap);
                zip_file.read_to_end(&mut bytes)?;
                Ok(bytes)
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
                display_name,
                sort_key.clone(),
                entry_name.clone(),
                bytes,
            ));
        }

        let chunk_processed: Vec<ImageEntry> = raw_chunk
            .into_par_iter()
            .map(|(id, display_name, sort_key, entry_name, bytes)| {
                let mut entry = create_image_entry(
                    id,
                    display_name.clone(),
                    bytes,
                    None,
                    Some((archive_arc.clone(), entry_name)),
                );
                entry.sort_key = sort_key;
                let done = counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                if let Some(cb) = progress_cb {
                    cb(done, total, &display_name);
                }
                entry
            })
            .collect();

        for entry in &chunk_processed {
            if entry.thumb_rgb.is_none() {
                errors.push(format!("{}: 无法解码预览，图片可能已损坏", entry.name));
            }
        }
        entries.extend(chunk_processed);
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
}
