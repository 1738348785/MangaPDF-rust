use std::path::PathBuf;

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
            Self::FixedRatio => "统一宽度 (1200px)",
        }
    }

    pub fn all() -> &'static [PageSizeMode] {
        &[Self::Original, Self::FitA4, Self::FixedRatio]
    }
}

#[derive(Clone)]
pub struct ImageEntry {
    pub id: usize,
    pub name: String,
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub thumb_rgb: Option<Vec<u8>>,
    pub thumb_width: usize,
    pub thumb_height: usize,
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
    pub jpeg_passthrough: bool,
    pub lossless_png: bool,
    pub margin_mm: u32,
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
            quality: 100, // 默认 100% 满画质
            jpeg_passthrough: true,
            lossless_png: true, // 默认启用 PNG 无损
            margin_mm: 0,
        }
    }

    pub fn natural_sort(&mut self) {
        self.items.sort_by(|a, b| natord::compare_ignore_case(&a.name, &b.name));
    }

    #[allow(dead_code)]
    pub fn move_up(&mut self, idx: usize) {
        if idx > 0 && idx < self.items.len() {
            self.items.swap(idx, idx - 1);
        }
    }

    #[allow(dead_code)]
    pub fn move_down(&mut self, idx: usize) {
        if idx + 1 < self.items.len() {
            self.items.swap(idx, idx + 1);
        }
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
