#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod img;
mod model;
mod pdf;
mod source;

use app::MangaPdfApp;
use eframe::egui::{self, FontData, FontDefinitions, FontFamily};
use std::fs;
use std::path::Path;

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
        .with_title("MangaPDF - 漫画与图片打包工具")
        .with_drag_and_drop(true);

    if let Some(icon) = load_app_icon() {
        viewport = viewport.with_icon(icon);
    }

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "MangaPDF - 漫画与图片打包工具",
        options,
        Box::new(|cc| {
            setup_custom_fonts(&cc.egui_ctx);
            Ok(Box::new(MangaPdfApp::new(cc)))
        }),
    )
}

/// 配置 Windows 中文字体（优先使用微软雅黑 msyh.ttc）
fn setup_custom_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();

    let font_candidates = [
        "C:\\Windows\\Fonts\\msyh.ttc",
        "C:\\Windows\\Fonts\\msyh.ttf",
        "C:\\Windows\\Fonts\\simhei.ttf",
        "C:\\Windows\\Fonts\\simsun.ttc",
    ];

    for path_str in font_candidates {
        let path = Path::new(path_str);
        if path.exists() {
            if let Ok(font_data) = fs::read(path) {
                fonts.font_data.insert(
                    "chinese_font".to_owned(),
                    FontData::from_owned(font_data),
                );

                fonts
                    .families
                    .entry(FontFamily::Proportional)
                    .or_default()
                    .insert(0, "chinese_font".to_owned());

                fonts
                    .families
                    .entry(FontFamily::Monospace)
                    .or_default()
                    .insert(0, "chinese_font".to_owned());

                break;
            }
        }
    }

    ctx.set_fonts(fonts);
}
