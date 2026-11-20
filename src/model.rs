use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageSizeMode {
    Original,
    FitA4,
    FixedRatio,
}

impl PageSizeMode {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Original => "保持原图尺寸 (推荐)",
            Self::FitA4 => "适应 A4 页面",
            Self::FixedRatio => "统一宽度 (1200pt)",
        }
    }

    pub fn all() -> &'static [PageSizeMode] {
        &[Self::Original, Self::FitA4, Self::FixedRatio]
    }
}

/// ZIP 条目读取时的安全预分配容量上限 (64 MB)，防止损坏归档伪造超大头部触发进程级 OOM Abort
pub const MAX_ZIP_PREALLOC_BYTES: u64 = 64 * 1024 * 1024;

/// 单个 ZIP 条目解压后的读取硬上限 (256 MB)。
///
/// 预分配上限只能挡住「伪造头部」，真正的 ZIP Bomb 压缩比可以到 1000:1 以上，
/// `read_to_end` 会一路扩容直到进程 OOM Abort。所以读取阶段必须再用 `Take` 卡一个硬上限，
/// 超限即报错跳过该条目，而不是把整个进程拖死。
pub const MAX_ZIP_ENTRY_BYTES: u64 = 256 * 1024 * 1024;

fn mb(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

/// 带硬上限地读取一个 ZIP 条目：先按头部声明尺寸做受限预分配，再用 `Take` 限制实际读取字节数。
pub fn read_zip_entry<R: Read>(
    reader: R,
    declared_size: u64,
    label: &str,
) -> std::io::Result<Vec<u8>> {
    read_zip_entry_with_limit(reader, declared_size, label, MAX_ZIP_ENTRY_BYTES)
}

/// [`read_zip_entry`] 的可测试版本：上限由调用方指定，便于用小体积数据覆盖两条拦截分支。
fn read_zip_entry_with_limit<R: Read>(
    reader: R,
    declared_size: u64,
    label: &str,
    limit: u64,
) -> std::io::Result<Vec<u8>> {
    if declared_size > limit {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "{} 声明解压后 {:.1} MB，超过单条目安全上限 {:.0} MB，已跳过（疑似 ZIP Bomb 或归档损坏）",
                label,
                mb(declared_size),
                mb(limit)
            ),
        ));
    }

    let mut buf = Vec::with_capacity(declared_size.min(MAX_ZIP_PREALLOC_BYTES) as usize);
    reader.take(limit + 1).read_to_end(&mut buf)?;

    if buf.len() as u64 > limit {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "{} 实际解压字节数超过单条目安全上限 {:.0} MB，已跳过（疑似 ZIP Bomb）",
                label,
                mb(limit)
            ),
        ));
    }
    Ok(buf)
}

/// 提取一组路径剥离公共祖先目录后的相对排序键（如 `Ch01/001.jpg`；单层目录则简化为 `001.jpg`）
pub fn compute_relative_sort_keys(raw_paths: &[String]) -> Vec<String> {
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
            if !first_dirs[i].eq_ignore_ascii_case(dirs[i]) {
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

/// 按路径分段进行自然语义比较（解决直接拼接路径比较时 `\` 与空格/连字符 ASCII 顺序反转，以及多章节同名图片交错问题）
pub fn compare_natural_path_str(a: &str, b: &str) -> std::cmp::Ordering {
    let mut a_iter = a.split(['/', '\\']).filter(|s| !s.is_empty());
    let mut b_iter = b.split(['/', '\\']).filter(|s| !s.is_empty());
    loop {
        match (a_iter.next(), b_iter.next()) {
            (Some(sa), Some(sb)) => {
                let cmp = natord::compare_ignore_case(sa, sb);
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

/// ZIP/CBZ 归档内的条目引用：通过精确的条目索引读取字节（彻底规避非 UTF-8 文件名匹配问题），
/// 同时保留已按正确编码解码的逻辑路径用于自然排序与界面展示。
#[derive(Clone)]
pub struct ArchiveEntryRef {
    pub archive_path: Arc<PathBuf>,
    pub entry_index: usize,
    pub decoded_path: String,
}

#[derive(Clone)]
pub struct ImageEntry {
    pub id: usize,
    pub name: String,
    /// 自然排序键（保留子目录相对路径，如 `Ch01/001.jpg`，确保跨章节重排不乱序）
    pub sort_key: String,
    pub source_path: Option<PathBuf>,
    pub archive_source: Option<ArchiveEntryRef>,
    pub bytes: Arc<[u8]>,
    pub width: u32,
    pub height: u32,
    pub thumb_rgb: Option<Arc<[u8]>>,
    pub thumb_width: usize,
    pub thumb_height: usize,
    /// 缩略图生成失败的原因；用于在网格占位符的提示里直接说明，而不是只显示"无缩略图"
    pub thumb_error: Option<String>,
}

impl ImageEntry {
    pub fn get_bytes(&self) -> Result<Vec<u8>, String> {
        if let Some(path) = &self.source_path {
            std::fs::read(path).map_err(|e| format!("无法读取图片文件 {}: {}", path.display(), e))
        } else if let Some(arch) = &self.archive_source {
            let file = std::fs::File::open(arch.archive_path.as_ref())
                .map_err(|e| format!("无法打开归档文件 {}: {}", arch.archive_path.display(), e))?;
            let mut archive = zip::ZipArchive::new(file)
                .map_err(|e| format!("无法解析归档 {}: {}", arch.archive_path.display(), e))?;
            let zip_file = archive
                .by_index(arch.entry_index)
                .map_err(|e| format!("归档中找不到图片 {}: {}", arch.decoded_path, e))?;
            let declared = zip_file.size();
            read_zip_entry(zip_file, declared, &arch.decoded_path)
                .map_err(|e| format!("读取归档内图片 {} 失败: {}", arch.decoded_path, e))
        } else {
            Ok(self.bytes.to_vec())
        }
    }
}

#[derive(Clone)]
pub struct Sequence {
    #[allow(dead_code)]
    pub id: usize,
    pub name: String,
    pub items: Vec<ImageEntry>,
    pub output_dir: Option<PathBuf>,
    pub output_filename: String,
    pub page_size_mode: PageSizeMode,
    pub quality: u8,
    pub lossless_direct: bool,
    pub margin_mm: u32,
    pub manga_rtl_spread: bool,
}

pub fn default_desktop_dir() -> Option<PathBuf> {
    if let Ok(userprofile) = std::env::var("USERPROFILE") {
        let desk = PathBuf::from(userprofile).join("Desktop");
        if desk.exists() {
            return Some(desk);
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        let desk = PathBuf::from(home).join("Desktop");
        if desk.exists() {
            return Some(desk);
        }
    }
    std::env::current_dir().ok()
}

/// 解析最终导出目录：当 `output_dir` 为 `None` 或空白路径时，优先回退到桌面目录（兜底当前目录），
/// 避免用户清空「输出保存目录」后把 PDF 写到拖拽启动时的 exe 目录或系统工作目录。
pub fn resolve_output_dir(output_dir: Option<&std::path::Path>) -> PathBuf {
    if let Some(dir) = output_dir {
        let raw = dir.to_string_lossy();
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    default_desktop_dir().unwrap_or_else(|| PathBuf::from("."))
}

impl Sequence {
    pub fn new(id: usize, name: String) -> Self {
        let output_filename = name.clone();
        Self {
            id,
            name,
            items: Vec::new(),
            output_dir: default_desktop_dir(),
            output_filename,
            page_size_mode: PageSizeMode::Original,
            quality: 100,          // 默认 100% 满画质
            lossless_direct: true, // 保持原画无损直存 (JPEG + PNG 全无损)
            margin_mm: 0,
            manga_rtl_spread: false, // 默认关闭日漫右翻跨页模式
        }
    }

    /// 基于分卷内所有条目的完整源路径重新计算相对 `sort_key`（解决分批次追加不同章节散图时目录层级被单批剥离的问题）
    pub fn refresh_sort_keys(&mut self) {
        if self.items.is_empty() {
            return;
        }
        let raw_paths: Vec<String> = self
            .items
            .iter()
            .map(|item| {
                if let Some(p) = &item.source_path {
                    p.to_string_lossy().into_owned()
                } else if let Some(arch) = &item.archive_source {
                    format!("{}/{}", arch.archive_path.to_string_lossy(), arch.decoded_path)
                } else {
                    item.sort_key.clone()
                }
            })
            .collect();
        let keys = compute_relative_sort_keys(&raw_paths);
        for (item, key) in self.items.iter_mut().zip(keys) {
            item.sort_key = key;
        }
    }

    pub fn natural_sort(&mut self) {
        self.refresh_sort_keys();
        self.items
            .sort_by(|a, b| compare_natural_path_str(&a.sort_key, &b.sort_key));
    }

    pub fn remove_at(&mut self, idx: usize) {
        if idx < self.items.len() {
            self.items.remove(idx);
        }
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_resolve_output_dir_falls_back_to_desktop_when_empty() {
        let fallback = default_desktop_dir().unwrap_or_else(|| PathBuf::from("."));
        assert_eq!(resolve_output_dir(None), fallback);
        assert_eq!(resolve_output_dir(Some(Path::new(""))), fallback);
        assert_eq!(resolve_output_dir(Some(Path::new("   \t  "))), fallback);
        assert_eq!(
            resolve_output_dir(Some(Path::new(" E:/Manga/Out "))),
            PathBuf::from("E:/Manga/Out")
        );
    }

    #[test]
    fn test_read_zip_entry_enforces_hard_read_limit() {
        use std::io::Cursor;

        // 1. 头部声明的解压后尺寸就超过上限：直接拒绝，不进入读取阶段
        let err = read_zip_entry_with_limit(Cursor::new(vec![0u8; 8]), 4096, "bomb.png", 1024)
            .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(
            err.to_string().contains("声明解压后"),
            "应命中「声明尺寸超限」分支，实际：{err}"
        );

        // 2. 头部声明很小、实际字节远超上限 —— 这正是预分配保护完全挡不住的 ZIP Bomb 形态，
        //    必须由读取阶段的 Take 硬上限拦下来，而不是让 Vec 一路扩容到进程 OOM Abort
        let err = read_zip_entry_with_limit(Cursor::new(vec![0u8; 4096]), 16, "bomb.png", 1024)
            .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(
            err.to_string().contains("实际解压字节数"),
            "应命中「实际读取超限」分支，实际：{err}"
        );

        // 3. 正常范围内的条目原样读出
        let data =
            read_zip_entry_with_limit(Cursor::new(b"%PDF-1.7 ok".to_vec()), 11, "ok.pdf", 1024)
                .unwrap();
        assert_eq!(data, b"%PDF-1.7 ok");

        // 4. 恰好等于上限也要放行（边界不能误伤合法大图）
        let exact = read_zip_entry_with_limit(Cursor::new(vec![7u8; 1024]), 1024, "edge", 1024)
            .expect("恰好等于上限的条目必须被接受");
        assert_eq!(exact.len(), 1024);
    }
}
