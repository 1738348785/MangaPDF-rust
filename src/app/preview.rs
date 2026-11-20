use super::{MangaPdfApp, PanelTheme};
use crate::model::ImageEntry;
use crate::util::trim_process_memory;
use eframe::egui::{self, Align, Color32, FontId, Layout, Vec2};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread;

impl MangaPdfApp {
    /// 将最新预览解码请求推入单工作线程队列：
    /// 快速连续切页时自动覆盖旧请求，最多仅运行 1 个解码线程，且通过 catch_unwind 杜绝异常死锁转圈。
    fn enqueue_preview_decode(&mut self, ctx: &egui::Context, item: ImageEntry, max_dim: u32) {
        {
            let mut guard = self
                .preview_pending
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            *guard = Some((item, max_dim));
        }

        if self
            .preview_worker_active
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
        {
            let pending = Arc::clone(&self.preview_pending);
            let worker_active = Arc::clone(&self.preview_worker_active);
            let tx = self.preview_sender.clone();
            let ctx_clone = ctx.clone();

            thread::spawn(move || loop {
                let next_task = {
                    let mut guard = pending.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(task) = guard.take() {
                        task
                    } else {
                        worker_active.store(false, Ordering::Release);
                        break;
                    }
                };

                let (cur_item, limit_dim) = next_task;
                let item_id = cur_item.id;
                let res = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    cur_item
                        .get_bytes()
                        .and_then(|bytes| {
                            crate::img::decode_oriented(&bytes).map_err(|e| format!("{:#}", e))
                        })
                        .map(|dyn_img| {
                            let img_to_show =
                                if dyn_img.width() > limit_dim || dyn_img.height() > limit_dim {
                                    dyn_img.resize(
                                        limit_dim,
                                        limit_dim,
                                        image::imageops::FilterType::CatmullRom,
                                    )
                                } else {
                                    dyn_img
                                };
                            let rgba = img_to_show.to_rgba8();
                            egui::ColorImage::from_rgba_unmultiplied(
                                [rgba.width() as usize, rgba.height() as usize],
                                &rgba,
                            )
                        })
                })) {
                    Ok(r) => r,
                    Err(_) => Err("图片解码时发生内部异常".to_string()),
                };

                let _ = tx.send((item_id, res));
                ctx_clone.request_repaint();
            });
        }
    }

    pub(super) fn render_preview_window(&mut self, ctx: &egui::Context, theme: &PanelTheme) {
        let Some(cur_idx) = self.preview_image_idx else {
            return;
        };

        let current_seq = self.current_sequence();
        if cur_idx >= current_seq.items.len() {
            self.clear_preview_state();
            trim_process_memory();
            return;
        }

        let cur_item = current_seq.items[cur_idx].clone();
        let item_id = cur_item.id;
        let item_name = cur_item.name.clone();
        let item_width = cur_item.width;
        let item_height = cur_item.height;
        let total_pages = current_seq.items.len();

        let cached_id = self.preview_texture.as_ref().map(|(id, _)| *id);
        if cached_id != Some(item_id) {
            self.preview_texture = None;
            self.preview_error = None;
            if self.preview_loading_id != Some(item_id) {
                self.preview_loading_id = Some(item_id);
                let gpu_max = ctx.input(|i| i.max_texture_side);
                let max_dim = (gpu_max as u32).clamp(1024, 8192);
                self.enqueue_preview_decode(ctx, cur_item, max_dim);
            }
        }

        let mut close_preview = false;
        let mut switch_idx = None;
        let mut is_open = true;

        let window_title = format!(
            "图片预览 (第 {}/{} 页) - {}",
            cur_idx + 1,
            total_pages,
            item_name
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
                // 快捷键支持：← 上一页，→ 下一页，Esc 关闭（文本框聚焦输入时不拦截方向键）
                let kb_free = !ctx.wants_keyboard_input();
                if kb_free && ui.input(|i| i.key_pressed(egui::Key::ArrowLeft)) && cur_idx > 0 {
                    switch_idx = Some(cur_idx.saturating_sub(1));
                }
                if kb_free
                    && ui.input(|i| i.key_pressed(egui::Key::ArrowRight))
                    && cur_idx + 1 < total_pages
                {
                    switch_idx = Some(cur_idx + 1);
                }
                if !self.show_about_dialog && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    close_preview = true;
                }

                // 预览控制栏 (极简整洁，无重复关闭按钮)
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(cur_idx > 0, egui::Button::new("◀ 上一页 (←)"))
                        .clicked()
                    {
                        switch_idx = Some(cur_idx.saturating_sub(1));
                    }
                    if ui
                        .add_enabled(cur_idx + 1 < total_pages, egui::Button::new("下一页 (→) ▶"))
                        .clicked()
                    {
                        switch_idx = Some(cur_idx + 1);
                    }

                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new(format!("第 {} / {} 页", cur_idx + 1, total_pages))
                            .strong(),
                    );

                    ui.add_space(10.0);
                    ui.separator();
                    ui.add_space(10.0);

                    ui.selectable_value(&mut self.preview_actual_size, false, "适应窗口");
                    ui.selectable_value(&mut self.preview_actual_size, true, "100% 原始尺寸");

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let dim_text = if item_width == 0 || item_height == 0 {
                            "尺寸未知 · 按 Esc 退出".to_string()
                        } else {
                            format!("{} × {} px · 按 Esc 退出", item_width, item_height)
                        };
                        ui.label(egui::RichText::new(dim_text).color(theme.text_muted));
                    });
                });

                ui.separator();

                // 图片展示区：平滑居中自适应缩放或 1:1 查看（按 item_id 区分滚动容器，切页时滚动条自动归零）
                let avail_size = ui.available_size();
                if let Some((_, Some(tex))) = &self.preview_texture {
                    if self.preview_actual_size {
                        egui::ScrollArea::both()
                            .id_salt(("preview_scroll", item_id))
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                ui.image((tex.id(), tex.size_vec2()));
                            });
                    } else if avail_size.x > 10.0 && avail_size.y > 10.0 {
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
                } else if self.preview_loading_id == Some(item_id) {
                    ui.centered_and_justified(|ui| {
                        ui.vertical_centered(|ui| {
                            ui.add_space(40.0);
                            ui.spinner();
                            ui.add_space(10.0);
                            ui.label(
                                egui::RichText::new("正在后台极速解码高分辨率原图...")
                                    .color(theme.text_muted)
                                    .font(FontId::proportional(13.0)),
                            );
                        });
                    });
                } else {
                    let err_detail = self
                        .preview_error
                        .as_ref()
                        .filter(|(id, _)| *id == item_id)
                        .map(|(_, msg)| msg.as_str());
                    ui.centered_and_justified(|ui| {
                        ui.vertical_centered(|ui| {
                            ui.add_space(40.0);
                            ui.label(
                                egui::RichText::new("无法加载该图片的高清原图")
                                    .color(Color32::from_rgb(239, 68, 68))
                                    .font(FontId::proportional(14.0))
                                    .strong(),
                            );
                            if let Some(reason) = err_detail {
                                ui.add_space(6.0);
                                ui.label(
                                    egui::RichText::new(format!("原因：{}", reason))
                                        .color(theme.text_muted)
                                        .font(FontId::proportional(12.0)),
                                );
                            }
                        });
                    });
                }
            });

        if let Some(new_idx) = switch_idx {
            self.preview_image_idx = Some(new_idx);
            self.preview_texture = None;
            self.preview_error = None;
            self.preview_loading_id = None;
        }

        if !is_open || close_preview {
            self.clear_preview_state();
            trim_process_memory();
        }
    }
}
