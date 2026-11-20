#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod img;
mod model;
mod pdf;
mod source;
mod util;

use app::MangaPdfApp;
use eframe::egui::{self, FontData, FontDefinitions, FontFamily};
use std::fs;
use std::path::PathBuf;

fn load_app_icon() -> Option<egui::IconData> {
    let icon_bytes = include_bytes!("../assets/icon.png");
    let image = image::load_from_memory(icon_bytes).ok()?.to_rgba8();
    let (width, height) = image.dimensions();
    Some(egui::IconData {
        rgba: image.into_raw(),
        width,
        height,
    })
}

fn main() -> eframe::Result<()> {
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([1080.0, 720.0])
        .with_min_inner_size([860.0, 580.0])
        .with_title("MangaPDF")
        .with_drag_and_drop(true);

    if let Some(icon) = load_app_icon() {
        viewport = viewport.with_icon(icon);
    }

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "MangaPDF",
        options,
        Box::new(|cc| {
            setup_custom_fonts(&cc.egui_ctx);
            Ok(Box::new(MangaPdfApp::new(cc)))
        }),
    )
}

/// 配置 Windows 中文字体（优先使用微软雅黑 msyh.ttc），并追加韩文回退字体
fn setup_custom_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();

    let windows_dir = std::env::var_os("SystemRoot")
        .or_else(|| std::env::var_os("WINDIR"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("C:\\Windows"));
    let fonts_dir = windows_dir.join("Fonts");

    // 中文字体插到字体列表最前面，保证中文字形优先
    if let Some((key, data)) = first_readable_font(
        &fonts_dir,
        &["msyh.ttc", "msyh.ttf", "simhei.ttf", "simsun.ttc"],
        "chinese_font",
    ) {
        fonts.font_data.insert(key.clone(), FontData::from_owned(data));
        for family in [FontFamily::Proportional, FontFamily::Monospace] {
            fonts.families.entry(family).or_default().insert(0, key.clone());
        }
    }

    // 韩文回退：微软雅黑 / 黑体 / 宋体都不含谚文，导入 CP949 韩文归档后分卷名与文件名
    // 会整片显示成方块 □□。追加到字体列表**末尾**，只补缺失字形，完全不改动中文字形。
    if let Some((key, data)) = first_readable_font(
        &fonts_dir,
        &["malgun.ttf", "malgunbd.ttf", "batang.ttc", "gulim.ttc"],
        "korean_font",
    ) {
        fonts.font_data.insert(key.clone(), FontData::from_owned(data));
        for family in [FontFamily::Proportional, FontFamily::Monospace] {
            fonts.families.entry(family).or_default().push(key.clone());
        }
    }

    ctx.set_fonts(fonts);
}

/// 依次尝试候选字体文件名，返回第一个存在且可读的字体（附上注册用的字体键名）
fn first_readable_font(
    fonts_dir: &std::path::Path,
    candidates: &[&str],
    font_key: &str,
) -> Option<(String, Vec<u8>)> {
    candidates
        .iter()
        .find_map(|file_name| Some((font_key.to_owned(), fs::read(fonts_dir.join(file_name)).ok()?)))
}
