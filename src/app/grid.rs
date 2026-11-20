use super::{display_sequence_name, truncate_with_ellipsis, MangaPdfApp, PanelTheme};
use crate::model::Sequence;
use eframe::egui::{self, Align, Color32, FontId, Layout, Rounding, Stroke, Vec2};
use std::collections::HashSet;
use std::sync::Arc;

impl MangaPdfApp {
    pub(super) fn render_central_panel(
        &mut self,
        ctx: &egui::Context,
        theme: &PanelTheme,
        is_busy: bool,
        is_hovering_files: bool,
        frame: Option<&eframe::Frame>,
    ) {
        egui::CentralPanel::default().show(ctx, |ui| {
            if is_busy {
                ui.disable();
            }
            // 分卷标签栏 (自适应自动折行，杜绝分卷过多超出边框被截断)
            egui::ScrollArea::vertical()
                .max_height(96.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(egui::RichText::new("分卷管理:").strong());

                        let mut to_switch = None;
                        let mut to_delete = None;
                        for (idx, seq) in self.sequences.iter().enumerate() {
                            let is_active = idx == self.active_sequence_idx;
                            let pill_bg = if is_active {
                                theme.primary_color
                            } else if theme.is_dark {
                                Color32::from_rgb(31, 41, 55)
                            } else {
                                Color32::from_rgb(243, 244, 246)
                            };
                            let text_color = if is_active {
                                Color32::WHITE
                            } else {
                                ui.visuals().text_color()
                            };

                            let display_seq_name = display_sequence_name(&seq.name);
                            let short_name = truncate_with_ellipsis(display_seq_name, 18);

                            let label_text = format!("{} ({}页)", short_name, seq.items.len());
                            let btn = egui::Button::new(
                                egui::RichText::new(label_text).color(text_color).strong(),
                            )
                            .fill(pill_bg)
                            .rounding(Rounding::same(14.0));

                            let hover_tip = if self.sequences.len() > 1 {
                                format!("{} (点击切换，右键可删除)", display_seq_name)
                            } else {
                                format!("{} (点击切换)", display_seq_name)
                            };
                            let resp = ui.add(btn).on_hover_text(hover_tip);
                            if resp.clicked() && self.active_sequence_idx != idx {
                                to_switch = Some(idx);
                            }
                            if self.sequences.len() > 1 {
                                resp.context_menu(|ui| {
                                    if ui.button("🗑 删除此分卷").clicked() {
                                        to_delete = Some(idx);
                                        ui.close_menu();
                                    }
                                });
                            }
                        }

                        if let Some(idx) = to_switch {
                            self.active_sequence_idx = idx;
                            self.clear_preview_state();
                        }

                        if ui.button("+ 新建分卷").clicked() {
                            let name = format!("第 {:02} 卷", self.next_sequence_id);
                            let new_seq = Sequence::new(self.next_sequence_id, name);
                            self.next_sequence_id += 1;
                            self.sequences.push(new_seq);
                            self.active_sequence_idx = self.sequences.len() - 1;
                            self.clear_preview_state();
                        }

                        if self.sequences.len() > 1 && ui.button("× 删除").clicked() {
                            to_delete = Some(self.active_sequence_idx);
                        }

                        if let Some(idx) = to_delete {
                            let prev_active_idx = self.active_sequence_idx;
                            self.sequences.remove(idx);
                            // 删除位于当前分卷之前的分卷时，当前索引需前移一位，否则会悄悄切换到相邻分卷
                            if idx < self.active_sequence_idx {
                                self.active_sequence_idx -= 1;
                            }
                            if self.active_sequence_idx >= self.sequences.len() {
                                self.active_sequence_idx = self.sequences.len() - 1;
                            }
                            if idx == prev_active_idx {
                                // 若删除的是当前正在预览/加载中的分卷，直接关闭预览，避免同页号串跳到相邻分卷
                                self.clear_preview_state();
                            }
                            self.cleanup_stale_textures();
                        }
                    });
                });

            ui.add_space(8.0);

            // 紧凑、精致的拖拽上传卡片 (高 68px)
            let drop_zone_rect = ui.available_rect_before_wrap();
            let drop_height = 68.0;
            let (rect, response) = ui.allocate_exact_size(
                Vec2::new(drop_zone_rect.width(), drop_height),
                egui::Sense::click(),
            );

            let is_hovered = response.hovered();

            let drop_bg = if is_hovering_files {
                if theme.is_dark {
                    Color32::from_rgb(30, 58, 138)
                } else {
                    Color32::from_rgb(239, 246, 255)
                }
            } else if is_hovered {
                if theme.is_dark {
                    Color32::from_rgb(38, 48, 65)
                } else {
                    Color32::from_rgb(248, 250, 252)
                }
            } else if theme.is_dark {
                Color32::from_rgb(31, 41, 55)
            } else {
                Color32::WHITE
            };

            let drop_border = if is_hovering_files || is_hovered {
                theme.primary_color
            } else if theme.is_dark {
                Color32::from_rgb(55, 65, 81)
            } else {
                Color32::from_rgb(209, 213, 219)
            };

            let stroke_width = if is_hovering_files { 2.0_f32 } else { 1.0_f32 };
            ui.painter().rect_filled(rect, 6.0, drop_bg);
            ui.painter()
                .rect_stroke(rect, 6.0, Stroke::new(stroke_width, drop_border));

            if is_hovered && !is_hovering_files {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }

            ui.allocate_new_ui(egui::UiBuilder::new().max_rect(rect), |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(13.0);
                    let title = if is_hovering_files {
                        "📥 释放鼠标立即导入漫画与图片"
                    } else {
                        "点击或将图片 / 文件夹 / CBZ 压缩包拖拽到此处"
                    };
                    let title_color = if is_hovering_files || is_hovered {
                        theme.primary_color
                    } else {
                        ui.visuals().text_color()
                    };
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(title)
                                .font(FontId::proportional(14.0))
                                .strong()
                                .color(title_color),
                        )
                        .selectable(false)
                        .sense(egui::Sense::hover()),
                    );
                    ui.add_space(3.0);
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(
                                "支持 JPG · PNG · WebP · BMP · ZIP · CBZ · 自动按自然顺序整理",
                            )
                            .font(FontId::proportional(11.0))
                            .color(theme.text_muted),
                        )
                        .selectable(false)
                        .sense(egui::Sense::hover()),
                    );
                });
            });

            if response.clicked() && !is_busy {
                if let Some(files) = super::file_dialog(frame)
                    .add_filter(
                        "图片或漫画归档",
                        &["jpg", "jpeg", "png", "webp", "bmp", "jfif", "zip", "cbz"],
                    )
                    .pick_files()
                {
                    self.add_paths_to_current_sequence(files);
                }
            }

            ui.add_space(14.0);

            // 预览列表工具栏
            let total_items = self.current_sequence().items.len();
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!("预览与排序 (共 {} 页)", total_items))
                        .font(FontId::proportional(14.0))
                        .strong(),
                );

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .add_enabled(!is_busy, egui::Button::new("清空全部"))
                        .clicked()
                    {
                        self.current_sequence_mut().clear();
                        self.clear_preview_state();
                        self.cleanup_stale_textures();
                    }

                    if ui
                        .add_enabled(!is_busy, egui::Button::new("自然排序 (1->2->10)"))
                        .clicked()
                    {
                        // 先记住当前预览的是哪张图，重排后按 id 找回它，避免预览跳到另一页
                        let preview_id = self.preview_item_id();
                        self.current_sequence_mut().natural_sort();
                        self.refocus_preview_by_id(preview_id);
                    }
                });
            });

            ui.add_space(10.0);

            // 工具栏里的「清空全部」可能就在同一帧把条目清空，
            // 这里必须重新读取实时长度：若继续用上面那次读取的 total_items，
            // 就会拿旧长度去切片一个已经变空的 Vec，触发 range end index out of range 的 panic（表现为点一下窗口就没了）。
            let total_items = self.current_sequence().items.len();

            // 虚拟滚动缩略图网格 或 空状态占位（仅渲染可视行并按需懒加载 GPU 纹理，数千页仍保持流畅）
            if total_items == 0 {
                ui.add_space(60.0);
                ui.vertical_centered(|ui| {
                    ui.label(
                        egui::RichText::new("暂无图片，请点击上方导入或直接拖入漫画文件")
                            .font(FontId::proportional(13.0))
                            .color(theme.text_muted),
                    );
                });
            } else {
                const CARD_INNER_W: f32 = 122.0;
                const CARD_INNER_H: f32 = 242.0;
                const CARD_MARGIN: f32 = 8.0;
                const CARD_OUTER_W: f32 = CARD_INNER_W + CARD_MARGIN * 2.0; // 138.0
                const CARD_OUTER_H: f32 = CARD_INNER_H + CARD_MARGIN * 2.0; // 258.0
                const GAP_X: f32 = 12.0;
                const GAP_Y: f32 = 16.0;
                const PREFETCH_BUFFER_ROWS: usize = 2;
                const KEEP_BUFFER_ROWS: usize = 8;
                const MAX_CACHED_TEXTURES: usize = 256;

                let avail_w = (ui.available_width() - 24.0).max(CARD_OUTER_W);
                let cols = ((avail_w + GAP_X) / (CARD_OUTER_W + GAP_X))
                    .floor()
                    .max(1.0) as usize;
                let total_rows = total_items.div_ceil(cols);

                let mut swap_action = None;
                let mut move_to_action = None;
                let mut remove_action = None;
                let mut preview_action = None;

                // 滚动位置按分卷隔离：所有分卷共用一个固定 id 时，切到页数少的分卷会沿用上一个分卷的
                // 偏移量而停在半空，切回来又丢了原来的位置
                let scroll_seq_id = self
                    .sequences
                    .get(self.active_sequence_idx)
                    .map_or(0, |seq| seq.id);

                ui.scope(|ui| {
                    ui.spacing_mut().item_spacing.y = GAP_Y;
                    egui::ScrollArea::vertical()
                        .id_salt(("image_cards_scroll", scroll_seq_id))
                        .auto_shrink([false, false])
                        .show_rows(ui, CARD_OUTER_H, total_rows, |ui, row_range| {
                            // 每帧按实时长度裁剪区间，杜绝任何"本帧中途条目被清空"导致的切片越界
                            let item_count = self.current_sequence().items.len();

                            // 1. 仅按当前可见区间 + 上下缓冲行按需上传缩略图纹理到 GPU
                            let prefetch_start = (row_range.start.saturating_sub(PREFETCH_BUFFER_ROWS)
                                * cols)
                                .min(item_count);
                            let prefetch_end = ((row_range.end + PREFETCH_BUFFER_ROWS) * cols)
                                .min(item_count);

                            if prefetch_start < prefetch_end {
                                let needed_textures: Vec<(usize, usize, usize, Arc<[u8]>)> = self
                                    .current_sequence()
                                    .items[prefetch_start..prefetch_end]
                                    .iter()
                                    .filter(|item| !self.texture_cache.contains_key(&item.id))
                                    .filter_map(|item| {
                                        item.thumb_rgb.as_ref().map(|rgba| {
                                            (
                                                item.id,
                                                item.thumb_width,
                                                item.thumb_height,
                                                Arc::clone(rgba),
                                            )
                                        })
                                    })
                                    .collect();

                                for (id, w, h, rgba) in needed_textures {
                                    let color_image =
                                        egui::ColorImage::from_rgba_unmultiplied([w, h], &rgba);
                                    let texture = ctx.load_texture(
                                        format!("thumb_{}", id),
                                        color_image,
                                        egui::TextureOptions::LINEAR,
                                    );
                                    self.texture_cache.insert(id, texture);
                                }
                            }

                            // 2. 当显存纹理缓存超过上限时，淘汰远离当前视口的纹理，严控超大单行本显存峰值
                            if self.texture_cache.len() > MAX_CACHED_TEXTURES {
                                let keep_start = (row_range.start.saturating_sub(KEEP_BUFFER_ROWS)
                                    * cols)
                                    .min(item_count);
                                let keep_end = ((row_range.end + KEEP_BUFFER_ROWS) * cols)
                                    .min(item_count);
                                let keep_ids: HashSet<usize> = self.current_sequence().items
                                    [keep_start..keep_end]
                                    .iter()
                                    .map(|item| item.id)
                                    .collect();
                                self.texture_cache.retain(|id, _| keep_ids.contains(id));
                            }

                            // 3. 仅构建当前视口内的卡片行
                            for row in row_range {
                                let start_idx = row * cols;
                                let end_idx = (start_idx + cols).min(item_count);
                                if start_idx >= end_idx {
                                    break;
                                }

                                ui.allocate_ui_with_layout(
                                    Vec2::new(ui.available_width(), CARD_OUTER_H),
                                    Layout::left_to_right(Align::TOP),
                                    |ui| {
                                        ui.spacing_mut().item_spacing = Vec2::new(GAP_X, 7.0);
                                        for idx in start_idx..end_idx {
                                            let item = &self.current_sequence().items[idx];
                                            ui.push_id(item.id, |ui| {
                                                egui::Frame::none()
                                                    .fill(theme.card_bg)
                                                    .stroke(theme.border_stroke)
                                                    .rounding(Rounding::same(6.0))
                                                    .inner_margin(egui::Margin::same(CARD_MARGIN))
                                                    .show(ui, |ui| {
                                                        ui.set_width(CARD_INNER_W);
                                                        ui.set_min_height(CARD_INNER_H);
                                                        ui.set_max_height(CARD_INNER_H);
                                                        ui.vertical_centered(|ui| {
                                                            let box_size =
                                                                Vec2::new(CARD_INNER_W, 154.0);
                                                            let (img_rect, img_resp) = ui
                                                                .allocate_exact_size(
                                                                    box_size,
                                                                    egui::Sense::click(),
                                                                );
                                                            if img_resp.hovered() {
                                                                ui.ctx().set_cursor_icon(
                                                                    egui::CursorIcon::PointingHand,
                                                                );
                                                            }
                                                            if let Some(texture) =
                                                                self.texture_cache.get(&item.id)
                                                            {
                                                                let tex_size = texture.size_vec2();
                                                                let scale = (box_size.x
                                                                    / tex_size.x.max(1.0))
                                                                .min(
                                                                    box_size.y
                                                                        / tex_size.y.max(1.0),
                                                                );
                                                                let draw_size = tex_size * scale;
                                                                let draw_rect =
                                                                    egui::Rect::from_center_size(
                                                                        img_rect.center(),
                                                                        draw_size,
                                                                    );
                                                                ui.painter().image(
                                                                    texture.id(),
                                                                    draw_rect,
                                                                    egui::Rect::from_min_max(
                                                                        egui::pos2(0.0, 0.0),
                                                                        egui::pos2(1.0, 1.0),
                                                                    ),
                                                                    Color32::WHITE,
                                                                );
                                                                if img_resp.hovered() {
                                                                    ui.painter().rect_stroke(
                                                                        draw_rect.expand(1.0),
                                                                        Rounding::same(3.0),
                                                                        Stroke::new(
                                                                            1.5_f32,
                                                                            theme.primary_color,
                                                                        ),
                                                                    );
                                                                }
                                                            } else {
                                                                let placeholder_bg =
                                                                    if theme.is_dark {
                                                                        Color32::from_rgb(
                                                                            17, 24, 39,
                                                                        )
                                                                    } else {
                                                                        Color32::from_rgb(
                                                                            243, 244, 246,
                                                                        )
                                                                    };
                                                                ui.painter().rect_filled(
                                                                    img_rect,
                                                                    Rounding::same(4.0),
                                                                    placeholder_bg,
                                                                );
                                                                let border_color =
                                                                    if img_resp.hovered() {
                                                                        theme.primary_color
                                                                    } else {
                                                                        theme.border_stroke.color
                                                                    };
                                                                ui.painter().rect_stroke(
                                                                    img_rect,
                                                                    Rounding::same(4.0),
                                                                    Stroke::new(
                                                                        1.0_f32,
                                                                        border_color,
                                                                    ),
                                                                );
                                                                ui.painter().text(
                                                                    img_rect.center(),
                                                                    egui::Align2::CENTER_CENTER,
                                                                    "⚠ 无缩略图",
                                                                    FontId::proportional(11.0),
                                                                    theme.text_muted,
                                                                );
                                                            }

                                                            let mut hover_tip = if item.sort_key
                                                                != item.name
                                                            {
                                                                format!(
                                                                    "{}\n🔍 左键点击放大预览 · 右键打开快捷菜单",
                                                                    item.sort_key
                                                                )
                                                            } else {
                                                                "🔍 左键点击放大预览 · 右键打开快捷菜单"
                                                                    .to_string()
                                                            };
                                                            // 缩略图生成失败时直接说明真实原因，而不是只显示一个占位符
                                                            if let Some(err) = &item.thumb_error {
                                                                hover_tip
                                                                    .push_str("\n⚠ 缩略图生成失败：");
                                                                hover_tip.push_str(err);
                                                            }
                                                            let img_resp =
                                                                img_resp.on_hover_text(hover_tip);
                                                            if img_resp.clicked() {
                                                                preview_action = Some(idx);
                                                            }
                                                            img_resp.context_menu(|ui| {
                                                                if ui
                                                                    .add_enabled(
                                                                        idx > 0,
                                                                        egui::Button::new(
                                                                            "⏫ 移至第一页 (设为封面)",
                                                                        ),
                                                                    )
                                                                    .clicked()
                                                                {
                                                                    move_to_action = Some((idx, 0));
                                                                    ui.close_menu();
                                                                }
                                                                if ui
                                                                    .add_enabled(
                                                                        idx + 1 < total_items,
                                                                        egui::Button::new(
                                                                            "⏬ 移至最后一页",
                                                                        ),
                                                                    )
                                                                    .clicked()
                                                                {
                                                                    move_to_action = Some((
                                                                        idx,
                                                                        total_items
                                                                            .saturating_sub(1),
                                                                    ));
                                                                    ui.close_menu();
                                                                }
                                                                ui.separator();
                                                                if ui.button("🔍 放大预览").clicked()
                                                                {
                                                                    preview_action = Some(idx);
                                                                    ui.close_menu();
                                                                }
                                                                if ui.button("🗑 移除此页").clicked()
                                                                {
                                                                    remove_action = Some(idx);
                                                                    ui.close_menu();
                                                                }
                                                            });

                                                            ui.add_space(4.0);

                                                            let display_name =
                                                                truncate_with_ellipsis(
                                                                    &item.name,
                                                                    14,
                                                                );

                                                            ui.add(
                                                                egui::Label::new(
                                                                    egui::RichText::new(format!(
                                                                        "#{}: {}",
                                                                        idx + 1,
                                                                        display_name
                                                                    ))
                                                                    .font(FontId::proportional(
                                                                        11.0,
                                                                    ))
                                                                    .strong(),
                                                                )
                                                                .truncate(),
                                                            );

                                                            // 解码与头解析都失败的图片不再写死 800×1200 假装正常，
                                                            // 宽高为 0 时明确显示「尺寸未知」
                                                            let size_label = if item.width == 0
                                                                || item.height == 0
                                                            {
                                                                "尺寸未知".to_string()
                                                            } else {
                                                                format!(
                                                                    "{} × {}",
                                                                    item.width, item.height
                                                                )
                                                            };

                                                            ui.add(
                                                                egui::Label::new(
                                                                    egui::RichText::new(size_label)
                                                                        .font(
                                                                            FontId::proportional(
                                                                                10.0,
                                                                            ),
                                                                        )
                                                                        .color(theme.text_muted),
                                                                )
                                                                .truncate(),
                                                            );

                                                            ui.add_space(4.0);

                                                            ui.horizontal(|ui| {
                                                                ui.spacing_mut().item_spacing =
                                                                    Vec2::new(4.0, 0.0);
                                                                ui.spacing_mut().button_padding =
                                                                    Vec2::new(4.0, 3.0);

                                                                if ui
                                                                    .add_enabled(
                                                                        idx > 0,
                                                                        egui::Button::new("◀")
                                                                            .min_size(Vec2::new(
                                                                                26.0, 22.0,
                                                                            )),
                                                                    )
                                                                    .on_hover_text("前移")
                                                                    .clicked()
                                                                {
                                                                    swap_action = Some((
                                                                        idx,
                                                                        idx.saturating_sub(1),
                                                                    ));
                                                                }
                                                                if ui
                                                                    .add_enabled(
                                                                        idx + 1 < total_items,
                                                                        egui::Button::new("▶")
                                                                            .min_size(Vec2::new(
                                                                                26.0, 22.0,
                                                                            )),
                                                                    )
                                                                    .on_hover_text("后移")
                                                                    .clicked()
                                                                {
                                                                    swap_action =
                                                                        Some((idx, idx + 1));
                                                                }
                                                                if ui
                                                                    .add_sized(
                                                                        [26.0, 22.0],
                                                                        egui::Button::new("🔍"),
                                                                    )
                                                                    .on_hover_text("放大预览")
                                                                    .clicked()
                                                                {
                                                                    preview_action = Some(idx);
                                                                }
                                                                if ui
                                                                    .add_sized(
                                                                        [26.0, 22.0],
                                                                        egui::Button::new("×"),
                                                                    )
                                                                    .on_hover_text("移除")
                                                                    .clicked()
                                                                {
                                                                    remove_action = Some(idx);
                                                                }
                                                            });
                                                        });
                                                    });
                                            });
                                        }
                                    },
                                );
                            }
                        });
                });

                if let Some((from, to)) = swap_action {
                    let swapped = {
                        let seq = self.current_sequence_mut();
                        if to < seq.items.len() && from != to {
                            seq.items.swap(from, to);
                            true
                        } else {
                            false
                        }
                    };
                    if swapped {
                        self.sync_preview_after_reorder(|cur| {
                            crate::util::preview_index_after_swap(cur, from, to)
                        });
                    }
                }

                if let Some((from, to)) = move_to_action {
                    let moved = {
                        let seq = self.current_sequence_mut();
                        if from < seq.items.len() && to < seq.items.len() && from != to {
                            let item = seq.items.remove(from);
                            seq.items.insert(to, item);
                            true
                        } else {
                            false
                        }
                    };
                    if moved {
                        self.sync_preview_after_reorder(|cur| {
                            crate::util::preview_index_after_move(cur, from, to)
                        });
                    }
                }

                if let Some(idx) = remove_action {
                    let removed_id = {
                        let seq = self.current_sequence_mut();
                        if idx < seq.items.len() {
                            let removed_id = seq.items[idx].id;
                            seq.remove_at(idx);
                            Some(removed_id)
                        } else {
                            None
                        }
                    };
                    if let Some(removed_id) = removed_id {
                        self.texture_cache.remove(&removed_id);
                        // 删掉正在预览的页 → 关闭预览；删掉前面的页 → 预览索引前移，避免错位一页
                        self.sync_preview_after_reorder(|cur| {
                            crate::util::preview_index_after_remove(cur, idx)
                        });
                    }
                }

                if let Some(idx) = preview_action {
                    self.preview_image_idx = Some(idx);
                }
            }
        });
    }
}
