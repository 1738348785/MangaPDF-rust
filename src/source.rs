use crate::img::create_image_entry;
use crate::model::ImageEntry;
use anyhow::{Context, Result};
use rayon::prelude::*;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

pub const SUPPORTED_IMG_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "png", "webp", "bmp", "jfif", "tif", "tiff",
];

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
    lower == "__macosx" || lower.starts_with('.') || lower == "$recycle.bin" || lower == "system volume information"
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

    files.sort_by(|a, b| {
        let a_str = a.to_string_lossy();
        let b_str = b.to_string_lossy();
        natord::compare_ignore_case(&a_str, &b_str)
    });

    files
}

/// 从文件路径列表中加载图片 (按自然语言自然排序)
pub fn load_entries_from_paths(paths: &[PathBuf], mut start_id: usize) -> Vec<ImageEntry> {
    let mut sorted_paths = paths.to_vec();
    sorted_paths.sort_by(|a, b| {
        let a_str = a.to_string_lossy();
        let b_str = b.to_string_lossy();
        natord::compare_ignore_case(&a_str, &b_str)
    });

    let raw_files: Vec<(usize, String, Vec<u8>)> = sorted_paths
        .into_iter()
        .filter_map(|p| {
            if let Some(ext) = p.extension().and_then(|s| s.to_str()) {
                if is_image_extension(ext) {
                    if let Ok(bytes) = fs::read(&p) {
                        let name = p.file_name().unwrap_or_default().to_string_lossy().to_string();
                        let id = start_id;
                        start_id += 1;
                        return Some((id, name, bytes));
                    }
                }
            }
            None
        })
        .collect();

    raw_files
        .into_par_iter()
        .map(|(id, name, bytes)| create_image_entry(id, name, bytes))
        .collect()
}

/// 从文件夹中加载所有图片 (可指定是否递归扫描)
pub fn load_entries_from_folder(dir: &Path, start_id: usize, recursive: bool) -> Result<Vec<ImageEntry>> {
    let file_paths = collect_image_files(dir, recursive);
    Ok(load_entries_from_paths(&file_paths, start_id))
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
                if !collect_image_files(&path, true).is_empty() {
                    subfolders.push(path);
                }
            }
        }
    }
    subfolders.sort_by(|a, b| {
        let a_str = a.to_string_lossy();
        let b_str = b.to_string_lossy();
        natord::compare_ignore_case(&a_str, &b_str)
    });
    subfolders
}

/// 从 ZIP / CBZ 压缩包中读取图片
pub fn load_entries_from_archive(archive_path: &Path, mut start_id: usize) -> Result<Vec<ImageEntry>> {
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
                    let is_ignored = name.split('/').any(is_system_hidden_dir) || name.split('\\').any(is_system_hidden_dir);
                    if !is_ignored {
                        valid_entries.push((i, name));
                    }
                }
            }
        }
    }

    valid_entries.sort_by(|a, b| natord::compare_ignore_case(&a.1, &b.1));

    let mut raw_items = Vec::with_capacity(valid_entries.len());
    for (index, name) in valid_entries {
        let mut zip_file = archive.by_index(index)?;
        let mut bytes = Vec::with_capacity(zip_file.size() as usize);
        zip_file.read_to_end(&mut bytes)?;
        let display_name = Path::new(&name)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let id = start_id;
        start_id += 1;
        raw_items.push((id, display_name, bytes));
    }

    let entries = raw_items
        .into_par_iter()
        .map(|(id, name, bytes)| create_image_entry(id, name, bytes))
        .collect();

    Ok(entries)
}
