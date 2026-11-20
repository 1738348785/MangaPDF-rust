use super::{
    circular_checkbox, display_sequence_name, truncate_with_ellipsis, MangaPdfApp, PanelTheme,
};
use crate::model::PageSizeMode;
use crate::util::open_in_file_explorer;
use eframe::egui::{self, Align, Color32, FontId, Layout, Rounding};
use std::path::PathBuf;

impl MangaPdfApp {
    pub(super) fn render_top_bar(&mut self, ctx: &egui::Context, theme: &PanelTheme) {
        egui::TopBottomPanel::top("top_bar")
            .frame(
                egui::Frame::none()
                    .fill(theme.card_bg)
                    .stroke(theme.border_stroke)
                    .inner_margin(egui::Margin::symmetric(16.0, 10.0)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let logo_bg = theme.primary_color;
                    egui::Frame::none()
                        .fill(logo_bg)
                        .rounding(Rounding::same(5.0))
                        .inner_margin(egui::Margin::symmetric(7.0, 3.0))
                        .show(ui, |ui| {
                            ui.label(
                                egui::RichText::new("MangaPDF")
                                    .color(Color32::WHITE)
                                    .strong()
                                    .font(FontId::proportional(14.0)),
                            );
                        });

                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new("漫画与图片转 PDF 工具")
                            .font(FontId::proportional(14.0))
                            .strong(),
                    );

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button("关于").clicked() {
                            self.show_about_dialog = true;
                        }

                        ui.separator();

                        let theme_btn = if theme.is_dark {
                            "浅色模式"
                        } else {
                            "深色模式"
                        };
                        if ui.button(theme_btn).clicked() {
                            self.is_dark_theme = !self.is_dark_theme;
                            self.apply_modern_styling(ctx);
                        }
                    });
                });
            });
    }

    pub(super) fn render_bottom_bar(
        &mut self,
        ctx: &egui::Context,
        theme: &PanelTheme,
        is_busy: bool,
    ) {
        egui::TopBottomPanel::bottom("bottom_bar")
            .frame(
                egui::Frame::none()
                    .fill(theme.card_bg)
                    .stroke(theme.border_stroke)
                    .inner_margin(egui::Margin::symmetric(16.0, 7.0)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if is_busy {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let cancel_label = if self.is_cancelling {
                                "正在取消..."
                            } else {
                                "取消"
                            };
                            let cancel_btn = egui::Button::new(
                                egui::RichText::new(cancel_label)
                                    .font(FontId::proportional(12.0))
                                    .color(if self.is_cancelling {
                                        theme.text_muted
                                    } else {
                                        Color32::from_rgb(239, 68, 68)
                                    })
                                    .strong(),
                            );
                            if ui
                                .add_enabled(!self.is_cancelling, cancel_btn)
                                .on_hover_text("中止当前导入或打包任务")
                                .clicked()
                            {
                                self.request_cancel();
                            }
                            ui.add_space(8.0);
                            ui.add(
                                egui::ProgressBar::new(self.progress)
                                    .desired_width(180.0)
                                    .show_percentage(),
                            );
                            ui.add_space(10.0);
                            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                ui.spinner();
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(&self.progress_text)
                                            .color(theme.primary_color)
                                            .strong(),
                                    )
                                    .truncate(),
                                );
                            });
                        });
                    } else {
                        // 先按 right_to_left 占住右侧空间放「打开所在目录」按钮，再让状态文本挤进剩余宽度。
                        // 顺序反过来的话（旧实现就是这样），批量生成失败时拼出来的超长错误信息会把整条横栏
                        // 撑爆，最右侧的按钮直接被顶出可视区域、完全点不到。
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if let Some(out_path) = &self.last_output_path {
                                let open_btn = egui::Button::new(
                                    egui::RichText::new("📂 打开所在目录")
                                        .font(FontId::proportional(12.0))
                                        .color(theme.primary_color)
                                        .strong(),
                                );
                                if ui
                                    .add(open_btn)
                                    .on_hover_text("在资源管理器中高亮定位该文件")
                                    .clicked()
                                {
                                    open_in_file_explorer(out_path);
                                }
                                ui.add_space(10.0);
                            }

                            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                if let Some((msg, is_err)) = &self.status_message {
                                    let dot_color = if *is_err {
                                        Color32::from_rgb(239, 68, 68)
                                    } else {
                                        Color32::from_rgb(16, 185, 129)
                                    };
                                    ui.label(
                                        egui::RichText::new("•")
                                            .color(dot_color)
                                            .font(FontId::proportional(18.0)),
                                    );
                                    let resp = ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(msg).color(dot_color).strong(),
                                        )
                                        .truncate(),
                                    );
                                    // 文本被截断时，完整内容放进悬停提示；
                                    // 有详细问题列表时则以列表为主，避免提示过长反而看不清
                                    if self.status_details.is_empty() {
                                        resp.on_hover_text(msg);
                                    } else {
                                        const MAX_LINES: usize = 30;
                                        let mut tip = format!("{}\n\n", msg);
                                        tip.push_str(
                                            &self
                                                .status_details
                                                .iter()
                                                .take(MAX_LINES)
                                                .cloned()
                                                .collect::<Vec<_>>()
                                                .join("\n"),
                                        );
                                        if self.status_details.len() > MAX_LINES {
                                            tip.push_str(&format!(
                                                "\n…… 另有 {} 条",
                                                self.status_details.len() - MAX_LINES
                                            ));
                                        }
                                        resp.on_hover_text(tip);
                                    }
                                } else {
                                    ui.label(
                                        egui::RichText::new("•")
                                            .color(Color32::from_rgb(16, 185, 129))
                                            .font(FontId::proportional(18.0)),
                                    );
                                    ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(
                                                "就绪 · 可直接将漫画文件夹或压缩包拖入窗口",
                                            )
                                            .color(theme.text_muted),
                                        )
                                        .truncate(),
                                    );
                                }
                            });
                        });
                    }
                });
            });
    }

    pub(super) fn render_left_panel(
        &mut self,
        ctx: &egui::Context,
        theme: &PanelTheme,
        is_busy: bool,
        frame: Option<&eframe::Frame>,
    ) {
        egui::SidePanel::left("left_control_panel")
            .exact_width(270.0)
            .resizable(false)
            .show_separator_line(false)
            .frame(
                egui::Frame::none()
                    .fill(theme.card_bg)
                    .stroke(theme.border_stroke)
                    .inner_margin(egui::Margin::same(14.0)),
            )
            .show(ctx, |ui| {
                if is_busy {
                    ui.disable();
                }
                ui.set_max_width(242.0);

                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.set_max_width(242.0);
                        let total_pages = self.current_sequence().items.len();
                        let seq_name = self.current_sequence().name.clone();

                        // 头部统计卡片
                        egui::Frame::none()
                            .fill(if theme.is_dark {
                                Color32::from_rgb(17, 24, 39)
                            } else {
                                Color32::from_rgb(243, 244, 246)
                            })
                            .rounding(Rounding::same(6.0))
                            .inner_margin(egui::Margin::symmetric(12.0, 10.0))
                            .show(ui, |ui| {
                                ui.set_min_width(218.0);
                                ui.set_max_width(218.0);
                                ui.label(
                                    egui::RichText::new(format!("共 {} 页", total_pages))
                                        .font(FontId::proportional(22.0))
                                        .strong()
                                        .color(theme.primary_color),
                                );

                                let display_seq_name = display_sequence_name(&seq_name);
                                let short_name = truncate_with_ellipsis(display_seq_name, 18);
                                ui.label(
                                    egui::RichText::new(format!("分卷: {}", short_name))
                                        .font(FontId::proportional(11.0))
                                        .color(theme.text_muted),
                                );
                            });

                        ui.add_space(8.0);

                        // 快捷导入操作（紧凑 2 按钮排布）
                        ui.horizontal(|ui| {
                            if ui
                                .add_sized([117.0, 30.0], egui::Button::new("导入文件夹"))
                                .clicked()
                            {
                                if let Some(folder) = super::file_dialog(frame).pick_folder() {
                                    self.add_paths_to_current_sequence(vec![folder]);
                                }
                            }
                            if ui
                                .add_sized([117.0, 30.0], egui::Button::new("选择图片文件"))
                                .clicked()
                            {
                                if let Some(files) = super::file_dialog(frame)
                                    .add_filter(
                                        "图片或漫画归档",
                                        &[
                                            "jpg", "jpeg", "png", "webp", "bmp", "jfif", "zip",
                                            "cbz",
                                        ],
                                    )
                                    .pick_files()
                                {
                                    self.add_paths_to_current_sequence(files);
                                }
                            }
                        });

                        ui.add_space(2.0);
                        ui.horizontal(|ui| {
                            circular_checkbox(ui, &mut self.scan_subfolders, "包含子文件夹图片");
                        })
                        .response
                        .on_hover_text(
                            "开启：将所选文件夹及其所有子文件夹内的图片合并导入到同一分卷\n关闭：优先仅导入根目录一级图片（忽略蒙版/翻译缓存等子目录）；若根目录无图片且含多个子目录，则自动按子目录拆分为多个分卷",
                        );

                        ui.add_space(8.0);
                        ui.separator();
                        ui.add_space(6.0);

                        // 输出设置
                        ui.label(
                            egui::RichText::new("输出配置")
                                .font(FontId::proportional(13.0))
                                .strong(),
                        );
                        ui.add_space(4.0);

                        let active_seq = self.current_sequence_mut();

                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("页面模式:").color(theme.text_muted));
                            egui::ComboBox::from_id_salt("page_mode_combo")
                                .selected_text(active_seq.page_size_mode.label())
                                .width(160.0)
                                .show_ui(ui, |ui| {
                                    for mode in PageSizeMode::all() {
                                        ui.selectable_value(
                                            &mut active_seq.page_size_mode,
                                            *mode,
                                            mode.label(),
                                        );
                                    }
                                });
                        });

                        ui.add_space(2.0);
                        circular_checkbox(
                            ui,
                            &mut active_seq.lossless_direct,
                            "保持原画无损直存 (100%原画)",
                        )
                        .on_hover_text("保留图片原始画质，不进行任何画质压缩，100% 还原原图");

                        ui.add_space(3.0);
                        ui.add_enabled_ui(!active_seq.lossless_direct, |ui| {
                            ui.spacing_mut().slider_width = 112.0;
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new("压缩画质:").color(theme.text_muted));
                                ui.add(
                                    egui::Slider::new(&mut active_seq.quality, 10..=100)
                                        .suffix("%"),
                                );
                            });
                        });
                        if active_seq.lossless_direct {
                            ui.label(
                                egui::RichText::new("💡 已启用原画无损直存，保持 100% 原始画质")
                                    .font(FontId::proportional(10.0))
                                    .color(theme.text_muted),
                            );
                        }

                        ui.add_space(3.0);
                        // 纯键盘输入框（不使用 DragValue / Slider，它们都靠拖动改值）。
                        // 编辑期间把文本存在 egui 临时内存里，避免每帧从 margin_mm 重建内容：
                        // 否则「删掉 0 后下一帧又被写回 0、光标跳回开头，再输入就变成 30」。
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("页面边距:").color(theme.text_muted));
                            let draft_id = egui::Id::new(("margin_mm_draft", active_seq.id));
                            let mut buf = ui
                                .memory(|m| m.data.get_temp::<String>(draft_id))
                                .unwrap_or_else(|| active_seq.margin_mm.to_string());
                            let resp = ui.add_sized(
                                [40.0, 22.0],
                                egui::TextEdit::singleline(&mut buf)
                                    .id_salt(("margin_mm_edit", active_seq.id))
                                    .char_limit(2),
                            );
                            if resp.changed() {
                                // 编辑中主要更新草稿，但**同时**把能解析出的合法值同步进真值：
                                // 否则改完边距没先失焦就直接拖文件进来（此时整块面板被 ui.disable()），
                                // `!resp.has_focus()` 分支会把还没提交的草稿丢掉，用户的修改静默消失。
                                // 保留草稿本身是为了防止「删光后想重输」时被立刻写回 0 导致光标乱跳。
                                ui.memory_mut(|m| m.data.insert_temp(draft_id, buf.clone()));
                                if let Ok(v) = buf.trim().parse::<u32>() {
                                    active_seq.margin_mm = v.min(50);
                                }
                            }
                            if resp.lost_focus() {
                                // 失焦或回车时才提交：空串视为 0，非法输入保持原值，超范围夹到 50
                                let trimmed = buf.trim();
                                active_seq.margin_mm = if trimmed.is_empty() {
                                    0
                                } else {
                                    trimmed
                                        .parse::<u32>()
                                        .map_or(active_seq.margin_mm, |v| v.min(50))
                                };
                            }
                            if !resp.has_focus() {
                                // 不在编辑状态时丢弃草稿，保证输入框始终反映真实值（含「同步到全部分卷」的改动）
                                ui.memory_mut(|m| m.data.remove::<String>(draft_id));
                            }
                            ui.label(
                                egui::RichText::new("mm（0 为无边框）")
                                    .font(FontId::proportional(11.0))
                                    .color(theme.text_muted),
                            );
                        });

                        ui.add_space(2.0);
                        circular_checkbox(
                            ui,
                            &mut active_seq.manga_rtl_spread,
                            "日漫右翻跨页模式 (从右向左)",
                        )
                        .on_hover_text(
                            "开启后写入 PDF 右翻跨页标记 (TwoPageRight + R2L)\n在 Acrobat / SumatraPDF 等专业阅读器中默认以日漫双页右翻排版打开",
                        );

                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new("分卷 / 输出文件名:").color(theme.text_muted),
                        );
                        if ui
                            .add_sized(
                                [242.0, 22.0],
                                egui::TextEdit::singleline(&mut active_seq.output_filename)
                                    // 按分卷 id 绑定控件 id，切换分卷时不会串用光标位置、
                                    // 水平滚动偏移与 Ctrl+Z 撤销历史
                                    .id_salt(("output_filename_edit", active_seq.id)),
                            )
                            .changed()
                        {
                            active_seq.name = active_seq.output_filename.clone();
                        }

                        ui.add_space(4.0);
                        ui.label(egui::RichText::new("输出保存目录:").color(theme.text_muted));
                        ui.horizontal(|ui| {
                            let mut dir_str = active_seq
                                .output_dir
                                .as_ref()
                                .map(|p| p.to_string_lossy().to_string())
                                .unwrap_or_default();
                            let dir_resp = ui.add_sized(
                                [189.0, 22.0],
                                egui::TextEdit::singleline(&mut dir_str)
                                    // 同上：按分卷 id 绑定，避免切换分卷时串用编辑状态
                                    .id_salt(("output_dir_edit", active_seq.id))
                                    .hint_text("留空则默认保存至桌面"),
                            );
                            // 编辑过程中原样写回、不做 trim：否则输入首尾空格会被立刻吞掉并导致光标回跳
                            if dir_resp.changed() {
                                active_seq.output_dir = if dir_str.is_empty() {
                                    None
                                } else {
                                    Some(PathBuf::from(&dir_str))
                                };
                            }
                            // 结束编辑（失焦/回车）时统一规范化：去首尾空白，空串视为"未设置"
                            if dir_resp.lost_focus() {
                                let trimmed = dir_str.trim();
                                active_seq.output_dir = if trimmed.is_empty() {
                                    None
                                } else {
                                    Some(PathBuf::from(trimmed))
                                };
                            }
                            if ui.add_sized([45.0, 22.0], egui::Button::new("浏览")).clicked() {
                                if let Some(folder) = super::file_dialog(frame).pick_folder() {
                                    active_seq.output_dir = Some(folder);
                                }
                            }
                        });

                        if self.sequences.len() > 1 {
                            ui.add_space(6.0);
                            let sync_btn = egui::Button::new(
                                egui::RichText::new("将当前配置应用到全部分卷")
                                    .font(FontId::proportional(12.0)),
                            );
                            if ui
                                .add_sized([242.0, 26.0], sync_btn)
                                .on_hover_text(format!(
                                    "将当前分卷的页面模式、无损/压缩画质、页面边距、日漫右翻与输出目录同步到全部 {} 个分卷（保留各分卷原有名称）",
                                    self.sequences.len()
                                ))
                                .clicked()
                            {
                                self.apply_current_config_to_all_sequences();
                            }
                        }

                        ui.add_space(14.0);
                        ui.separator();
                        ui.add_space(8.0);

                        // 主打包按钮
                        let (btn_text, btn_fill) = if self.is_generating {
                            ("⏳ 正在打包中...", Color32::from_rgb(156, 163, 175))
                        } else if self.is_importing {
                            ("⏳ 正在导入中...", Color32::from_rgb(156, 163, 175))
                        } else {
                            ("开始打包为 PDF", theme.primary_color)
                        };

                        let primary_btn = egui::Button::new(
                            egui::RichText::new(btn_text)
                                .font(FontId::proportional(15.0))
                                .strong()
                                .color(Color32::WHITE),
                        )
                        .fill(btn_fill);

                        if ui.add_sized([242.0, 38.0], primary_btn).clicked() && !is_busy {
                            self.trigger_generate_current(frame);
                        }

                        ui.add_space(4.0);

                        if self.sequences.len() > 1 {
                            let batch_btn = egui::Button::new(
                                egui::RichText::new(format!(
                                    "批量生成全部分卷 ({} 个)",
                                    self.sequences.len()
                                ))
                                .font(FontId::proportional(12.0)),
                            );

                            if ui.add_sized([242.0, 30.0], batch_btn).clicked() && !is_busy {
                                self.trigger_generate_all();
                            }
                        }
                    });
            });
    }
}
