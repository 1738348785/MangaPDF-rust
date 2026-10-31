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

#[derive(Clone)]
pub struct ImageEntry {
    pub id: usize,
    pub name: String,
    /// 自然排序键（保留子目录相对路径，如 `Ch01/001.jpg`，确保跨章节重排不乱序）
    pub sort_key: String,
    pub source_path: Option<PathBuf>,
    pub archive_source: Option<(Arc<PathBuf>, String)>,
    pub bytes: Arc<[u8]>,
    pub width: u32,
    pub height: u32,
    pub thumb_rgb: Option<Arc<[u8]>>,
    pub thumb_width: usize,
    pub thumb_height: usize,
}

impl ImageEntry {
    pub fn get_bytes(&self) -> Result<Vec<u8>, String> {
        if let Some(path) = &self.source_path {
            std::fs::read(path).map_err(|e| format!("无法读取图片文件 {}: {}", path.display(), e))
        } else if let Some((archive_path, entry_name)) = &self.archive_source {
            let file = std::fs::File::open(archive_path.as_ref())
                .map_err(|e| format!("无法打开归档文件 {}: {}", archive_path.display(), e))?;
            let mut archive = zip::ZipArchive::new(file)
                .map_err(|e| format!("无法解析归档 {}: {}", archive_path.display(), e))?;
            let mut zip_file = archive
                .by_name(entry_name)
                .map_err(|e| format!("归档中找不到图片 {}: {}", entry_name, e))?;
            let cap = zip_file.size().min(MAX_ZIP_PREALLOC_BYTES) as usize;
            let mut buf = Vec::with_capacity(cap);
            zip_file
                .read_to_end(&mut buf)
                .map_err(|e| format!("读取归档内图片 {} 失败: {}", entry_name, e))?;
            Ok(buf)
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

    pub fn natural_sort(&mut self) {
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
