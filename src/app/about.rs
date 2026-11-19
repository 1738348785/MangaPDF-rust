use super::{MangaPdfApp, PanelTheme};
use eframe::egui::{self, Color32, FontId, Rounding, Vec2};

impl MangaPdfApp {
    pub(super) fn render_about_dialog(&mut self, ctx: &egui::Context, theme: &PanelTheme) {
        if !self.show_about_dialog {
            return;
        }

        let app_logo = self.get_app_logo(ctx);
        let github_logo = self.get_github_logo(ctx);
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
                            .rounding(Rounding::same(10.0)),
                    );
                    ui.add_space(2.0);
                    ui.heading("MangaPDF 漫画与图片打包工具");
                    ui.add_space(2.0);
                    ui.label(
                        egui::RichText::new(concat!(
                            "版本 v",
                            env!("CARGO_PKG_VERSION"),
                            " · 现代轻量 GUI"
                        ))
                        .color(theme.text_muted),
                    );
                    ui.add_space(4.0);

                    // GitHub 小图标与小字转跳链接
                    let label_text = "GitHub ↗";
                    let font_id = FontId::proportional(12.0);
                    let icon_size = 15.0;
                    let gap = 5.0;
                    let text_w = ui.fonts(|f| {
                        f.layout_no_wrap(label_text.to_string(), font_id.clone(), theme.text_muted)
                            .size()
                            .x
                    });
                    let total_w = icon_size + gap + text_w;

                    let (rect, resp) =
                        ui.allocate_exact_size(Vec2::new(total_w, 20.0), egui::Sense::click());
                    let is_hovered = resp.hovered();
                    let is_clicked = resp.clicked();
                    resp.on_hover_text(
                        "在浏览器中打开 GitHub 开源仓库 ↗\nhttps://github.com/1738348785/MangaPDF-rust",
                    );

                    if is_clicked {
                        ctx.open_url(egui::OpenUrl::new_tab(github_url));
                    }
                    if is_hovered {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }

                    let color = if is_hovered {
                        theme.primary_color
                    } else {
                        theme.text_muted
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
                ui.add_space(8.0);

                // 精炼特性亮点
                let features = [
                    ("⚡ 原画无损直存", "JPEG 零拷贝与 PNG 预测无损"),
                    ("🚀 异步流畅体验", "后台多核并发，大图切页丝滑"),
                    ("📖 专为漫画优化", "智能自然排序与日漫右翻跨页"),
                ];

                for (title, desc) in features {
                    ui.horizontal(|ui| {
                        ui.add_space(6.0);
                        ui.label(egui::RichText::new(title).strong());
                        ui.label(egui::RichText::new(desc).color(theme.text_muted));
                    });
                    ui.add_space(2.0);
                }

                ui.add_space(14.0);
                ui.vertical_centered(|ui| {
                    let confirm_btn = egui::Button::new(
                        egui::RichText::new("确定").color(Color32::WHITE).strong(),
                    )
                    .fill(theme.primary_color);
                    if ui.add_sized([80.0, 28.0], confirm_btn).clicked() {
                        close_dialog = true;
                    }
                });
            });

        if close_dialog {
            self.show_about_dialog = false;
        }
    }
}
