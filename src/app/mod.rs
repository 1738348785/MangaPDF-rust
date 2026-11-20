mod about;
mod grid;
mod panels;
mod preview;
mod tasks;

use crate::model::{ImageEntry, Sequence};
use crate::util::trim_process_memory;
use eframe::egui::{self, Color32, Rounding, Stroke, Vec2};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

pub struct MangaPdfApp {
    sequences: Vec<Sequence>,
    active_sequence_idx: usize,
    next_sequence_id: usize,
    next_image_id: usize,

    texture_cache: HashMap<usize, egui::TextureHandle>,

    is_dark_theme: bool,
    status_message: Option<(String, bool)>,
    /// 状态栏消息的详细问题列表 (跳过/失败的图片)，悬停状态栏时展示
    status_details: Vec<String>,
    is_generating: bool,
    is_importing: bool,
    is_cancelling: bool,
    cancel_flag: Arc<AtomicBool>,
    progress: f32,
    progress_text: String,

    task_receiver: Receiver<TaskMessage>,
    task_sender: Sender<TaskMessage>,

    show_about_dialog: bool,
    last_output_path: Option<PathBuf>,
    scan_subfolders: bool,
    app_logo_texture: Option<egui::TextureHandle>,
    github_logo_texture: Option<egui::TextureHandle>,
    frame_count: usize,
    preview_image_idx: Option<usize>,
    preview_texture: Option<(usize, Option<egui::TextureHandle>)>,
    preview_error: Option<(usize, String)>,
    preview_actual_size: bool,
    preview_loading_id: Option<usize>,
    preview_pending: Arc<Mutex<Option<(ImageEntry, u32)>>>,
    preview_worker_active: Arc<AtomicBool>,
    preview_receiver: Receiver<(usize, Result<egui::ColorImage, String>)>,
    preview_sender: Sender<(usize, Result<egui::ColorImage, String>)>,
}

struct PanelTheme {
    card_bg: Color32,
    border_stroke: Stroke,
    text_muted: Color32,
    primary_color: Color32,
    is_dark: bool,
}

impl PanelTheme {
    /// 按明暗主题构造一套面板配色（深色用 Gray-800/900 体系，浅色用 Gray-50/200 体系）
    fn new(is_dark: bool) -> Self {
        let card_bg = if is_dark {
            Color32::from_rgb(31, 41, 55)
        } else {
            Color32::WHITE
        };
        let border_stroke = Stroke::new(
            1.0_f32,
            if is_dark {
                Color32::from_rgb(55, 65, 81)
            } else {
                Color32::from_rgb(229, 231, 235)
            },
        );
        let text_muted = if is_dark {
            Color32::from_rgb(156, 163, 175)
        } else {
            Color32::from_rgb(107, 114, 128)
        };
        let primary_color = if is_dark {
            Color32::from_rgb(59, 130, 246)
        } else {
            Color32::from_rgb(37, 99, 235)
        };
        Self {
            card_bg,
            border_stroke,
            text_muted,
            primary_color,
            is_dark,
        }
    }
}

struct ImportedGroup {
    seq_name: Option<String>,
    out_dir: Option<PathBuf>,
    entries: Vec<ImageEntry>,
}

enum TaskMessage {
    Progress {
        current: usize,
        total: usize,
        desc: String,
    },
    ImportFinished {
        groups: Vec<ImportedGroup>,
        errors: Vec<String>,
        skipped_subfolders: Vec<String>,
        unsupported_exts: Vec<String>,
    },
    Finished {
        success_count: usize,
        skipped: Vec<String>,
        output_path: PathBuf,
    },
    BatchFinished {
        total_files: usize,
        failed_errors: Vec<String>,
        skipped: Vec<String>,
        last_output_path: Option<PathBuf>,
    },
    Cancelled {
        message: String,
        details: Vec<String>,
        last_output_path: Option<PathBuf>,
    },
    Error(String),
}

/// 启动后台任务线程，并捕获其中的 panic (例如畸形图片导致解码器崩溃)。
/// 发生 panic 时发送 `TaskMessage::Error`，确保界面能复位 is_generating / is_importing，不会永久卡住。
fn spawn_guarded<F>(tx: Sender<TaskMessage>, task_label: &'static str, f: F)
where
    F: FnOnce() + Send + 'static,
{
    thread::spawn(move || {
        if let Err(payload) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
            let reason = payload
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "未知错误".to_string());
            let _ = tx.send(TaskMessage::Error(format!(
                "{}过程中发生内部错误，任务已中止：{}",
                task_label, reason
            )));
        }
    });
}

/// 返回供界面展示的分卷名称（空串或纯空白时统一回退为 `"未命名分卷"`）
pub(super) fn display_sequence_name(name: &str) -> &str {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        "未命名分卷"
    } else {
        trimmed
    }
}

/// 按字符数上限截断展示文本，超出时保留前 `max_chars - 2` 个字符并追加 `"..."`
pub(super) fn truncate_with_ellipsis(text: &str, max_chars: usize) -> String {
    if text.chars().count() > max_chars {
        let keep = max_chars.saturating_sub(2);
        format!("{}...", text.chars().take(keep).collect::<String>())
    } else {
        text.to_string()
    }
}

/// 主题全局控件圆角是 6.0，egui 的勾选框方框只有 `icon_width`(14px) 大小，
/// 套上 6.0 圆角会呈现"圆角小方块"，鼠标悬停时露出蓝色描边尤其明显。
/// 这里把勾选框局部改成**正圆**：inactive 半径 = 边长的一半；
/// hovered / active 状态下 egui 会把控件框外扩 1px（expansion = 1.0），半径同步 +1 才能保持正圆。
/// 用 `ui.scope` 包裹，样式修改只作用于勾选框本身，不影响按钮等其他控件。
fn circular_checkbox(
    ui: &mut egui::Ui,
    checked: &mut bool,
    text: impl Into<egui::WidgetText>,
) -> egui::Response {
    let radius = (ui.spacing().icon_width * 0.5).max(1.0);
    ui.scope(|ui| {
        let widgets = &mut ui.visuals_mut().widgets;
        widgets.inactive.rounding = Rounding::same(radius);
        widgets.hovered.rounding = Rounding::same(radius + 1.0);
        widgets.active.rounding = Rounding::same(radius + 1.0);
        ui.checkbox(checked, text)
    })
    .inner
}

impl MangaPdfApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let (tx, rx) = channel();
        let (prev_tx, prev_rx) = channel();
        let initial_seq = Sequence::new(1, "第 01 卷".to_string());

        let mut app = Self {
            sequences: vec![initial_seq],
            active_sequence_idx: 0,
            next_sequence_id: 2,
            next_image_id: 1,
            texture_cache: HashMap::new(),
            is_dark_theme: false,
            status_message: None,
            status_details: Vec::new(),
            is_generating: false,
            is_importing: false,
            is_cancelling: false,
            cancel_flag: Arc::new(AtomicBool::new(false)),
            progress: 0.0,
            progress_text: String::new(),
            task_receiver: rx,
            task_sender: tx,
            show_about_dialog: false,
            last_output_path: None,
            scan_subfolders: false,
            app_logo_texture: None,
            github_logo_texture: None,
            frame_count: 0,
            preview_image_idx: None,
            preview_texture: None,
            preview_error: None,
            preview_actual_size: false,
            preview_loading_id: None,
            preview_pending: Arc::new(Mutex::new(None)),
            preview_worker_active: Arc::new(AtomicBool::new(false)),
            preview_receiver: prev_rx,
            preview_sender: prev_tx,
        };

        app.apply_modern_styling(&cc.egui_ctx);

        // 支持直接将漫画文件夹、图片或 .zip/.cbz 拖拽到 exe 图标启动
        let cli_paths: Vec<PathBuf> = std::env::args_os()
            .skip(1)
            .map(PathBuf::from)
            .filter(|p| p.exists())
            .collect();
        if !cli_paths.is_empty() {
            app.add_paths_to_current_sequence(cli_paths);
        }

        app
    }

    fn get_app_logo(&mut self, ctx: &egui::Context) -> egui::TextureHandle {
        if let Some(tex) = &self.app_logo_texture {
            return tex.clone();
        }
        let bytes = include_bytes!("../../assets/icon.png");
        let img = image::load_from_memory(bytes)
            .expect("Failed to load icon.png")
            .to_rgba8();
        let (w, h) = img.dimensions();
        let color_image =
            egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &img.into_raw());
        let tex = ctx.load_texture(
            "app_logo_texture",
            color_image,
            egui::TextureOptions::LINEAR,
        );
        self.app_logo_texture = Some(tex.clone());
        tex
    }

    fn get_github_logo(&mut self, ctx: &egui::Context) -> egui::TextureHandle {
        if let Some(tex) = &self.github_logo_texture {
            return tex.clone();
        }
        let bytes = include_bytes!("../../assets/github.png");
        let img = image::load_from_memory(bytes)
            .expect("Failed to load github.png")
            .to_rgba8();
        let (w, h) = img.dimensions();
        let color_image =
            egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &img.into_raw());
        let tex = ctx.load_texture(
            "github_logo_texture",
            color_image,
            egui::TextureOptions::LINEAR,
        );
        self.github_logo_texture = Some(tex.clone());
        tex
    }

    fn current_sequence_mut(&mut self) -> &mut Sequence {
        if self.active_sequence_idx >= self.sequences.len() {
            self.active_sequence_idx = 0;
        }
        &mut self.sequences[self.active_sequence_idx]
    }

    fn current_sequence(&self) -> &Sequence {
        if self.active_sequence_idx >= self.sequences.len() {
            &self.sequences[0]
        } else {
            &self.sequences[self.active_sequence_idx]
        }
    }

    pub(super) fn clear_preview_state(&mut self) {
        self.preview_image_idx = None;
        self.preview_texture = None;
        self.preview_error = None;
        self.preview_loading_id = None;
        *self
            .preview_pending
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// 当前预览页对应的图片 id（用于重排后重新定位同一张图）
    fn preview_item_id(&self) -> Option<usize> {
        self.preview_image_idx
            .and_then(|idx| self.current_sequence().items.get(idx))
            .map(|item| item.id)
    }

    /// 分卷内图片顺序整体变化（如自然排序）后，按 item id 重新定位预览页：
    /// 预览窗继续显示同一张图，而不是"第 N 页"在新顺序里变成另一张图。
    fn refocus_preview_by_id(&mut self, previous_id: Option<usize>) {
        let Some(id) = previous_id else {
            return;
        };
        let new_idx = self
            .current_sequence()
            .items
            .iter()
            .position(|item| item.id == id);
        match new_idx {
            Some(idx) => self.preview_image_idx = Some(idx),
            None => self.clear_preview_state(),
        }
    }

    /// 分卷内图片顺序发生变化（删除 / 交换 / 移动到第 N 位）后同步预览窗索引，
    /// 避免预览悄悄跳到相邻页；映射结果为 None 时关闭预览。
    fn sync_preview_after_reorder(&mut self, map: impl FnOnce(usize) -> Option<usize>) {
        if let Some(cur) = self.preview_image_idx {
            let mapped = map(cur);
            self.preview_image_idx = mapped;
            if mapped.is_none() {
                self.clear_preview_state();
            }
        }
    }

    fn cleanup_stale_textures(&mut self) {
        let valid_ids: HashSet<usize> = self
            .sequences
            .iter()
            .flat_map(|s| s.items.iter().map(|i| i.id))
            .collect();
        self.texture_cache.retain(|id, _| valid_ids.contains(id));
        let preview_tex_stale = self
            .preview_texture
            .as_ref()
            .is_some_and(|(id, _)| !valid_ids.contains(id));
        let preview_loading_stale = self
            .preview_loading_id
            .is_some_and(|id| !valid_ids.contains(&id));
        let preview_err_stale = self
            .preview_error
            .as_ref()
            .is_some_and(|(id, _)| !valid_ids.contains(id));
        if preview_tex_stale || preview_loading_stale || preview_err_stale {
            self.clear_preview_state();
        }
    }

    /// 设置状态栏消息及其详细问题列表 (详细列表在悬停状态栏时展示)
    fn set_status(&mut self, msg: String, is_err: bool, details: Vec<String>) {
        self.status_message = Some((msg, is_err));
        self.status_details = details;
    }

    /// 触发当前正在运行的后台导入/打包任务取消
    fn request_cancel(&mut self) {
        if (self.is_importing || self.is_generating) && !self.is_cancelling {
            self.cancel_flag.store(true, Ordering::SeqCst);
            self.is_cancelling = true;
            self.progress_text = if self.is_importing {
                "正在取消导入...".to_string()
            } else {
                "正在取消打包...".to_string()
            };
        }
    }

    /// 将当前分卷的输出配置（页面模式、无损/画质、边距、日漫右翻、输出目录）一键同步到全部分卷
    fn apply_current_config_to_all_sequences(&mut self) {
        if self.sequences.len() <= 1 {
            return;
        }
        let cur = self.current_sequence();
        let page_size_mode = cur.page_size_mode;
        let lossless_direct = cur.lossless_direct;
        let quality = cur.quality;
        let margin_mm = cur.margin_mm;
        let manga_rtl_spread = cur.manga_rtl_spread;
        let output_dir = cur.output_dir.clone();
        let count = self.sequences.len();

        for seq in &mut self.sequences {
            seq.page_size_mode = page_size_mode;
            seq.lossless_direct = lossless_direct;
            seq.quality = quality;
            seq.margin_mm = margin_mm;
            seq.manga_rtl_spread = manga_rtl_spread;
            seq.output_dir = output_dir.clone();
        }

        self.set_status(
            format!("已将当前分卷的输出配置应用到全部 {} 个分卷", count),
            false,
            Vec::new(),
        );
    }

    fn apply_modern_styling(&self, ctx: &egui::Context) {
        let target_theme = if self.is_dark_theme {
            egui::Theme::Dark
        } else {
            egui::Theme::Light
        };
        // 锁定 egui 主题偏好，防止 eframe 每帧根据系统浅色/深色设置重置应用主题
        ctx.set_theme(target_theme);

        let mut style = (*ctx.style()).clone();
        style.spacing.item_spacing = Vec2::new(8.0, 7.0);
        style.spacing.window_margin = egui::Margin::same(12.0);
        style.spacing.button_padding = Vec2::new(10.0, 5.0);
        style.spacing.slider_width = 120.0;
        style.interaction.selectable_labels = false;

        let mut visuals = if self.is_dark_theme {
            let mut dark = egui::Visuals::dark();
            dark.extreme_bg_color = Color32::from_rgb(17, 24, 39);
            dark.panel_fill = Color32::from_rgb(17, 24, 39); // Gray-900
            dark.window_fill = Color32::from_rgb(31, 41, 55); // Gray-800
            dark.widgets.noninteractive.bg_fill = Color32::from_rgb(31, 41, 55);
            dark.widgets.inactive.bg_fill = Color32::from_rgb(55, 65, 81);
            dark.widgets.hovered.bg_fill = Color32::from_rgb(75, 85, 99);
            dark.widgets.active.bg_fill = Color32::from_rgb(59, 130, 246);
            dark.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(75, 85, 99));
            dark.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(59, 130, 246));
            dark
        } else {
            let mut light = egui::Visuals::light();
            light.extreme_bg_color = Color32::from_rgb(243, 244, 246);
            light.panel_fill = Color32::from_rgb(249, 250, 251); // Gray-50
            light.window_fill = Color32::WHITE;
            light.widgets.noninteractive.bg_fill = Color32::from_rgb(243, 244, 246);
            light.widgets.inactive.bg_fill = Color32::from_rgb(229, 231, 235);
            light.widgets.hovered.bg_fill = Color32::from_rgb(209, 213, 219);
            light.widgets.active.bg_fill = Color32::from_rgb(37, 99, 235);
            light.widgets.inactive.bg_stroke =
                Stroke::new(1.0_f32, Color32::from_rgb(209, 213, 219));
            light.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(37, 99, 235));
            light
        };

        // 在构造完 dark / light Visuals 之后设置统一圆角，确保不会被默认值覆盖
        visuals.window_rounding = Rounding::same(8.0);
        visuals.menu_rounding = Rounding::same(6.0);
        visuals.widgets.noninteractive.rounding = Rounding::same(6.0);
        visuals.widgets.inactive.rounding = Rounding::same(6.0);
        visuals.widgets.hovered.rounding = Rounding::same(6.0);
        visuals.widgets.active.rounding = Rounding::same(6.0);
        style.visuals = visuals;

        ctx.set_style_of(target_theme, style);
    }

    fn handle_drag_and_drop(&mut self, ctx: &egui::Context) {
        let dropped_files = ctx.input(|i| i.raw.dropped_files.clone());
        if !dropped_files.is_empty() {
            let paths: Vec<PathBuf> = dropped_files.into_iter().filter_map(|f| f.path).collect();
            if !paths.is_empty() {
                if self.is_generating || self.is_importing {
                    self.set_status(
                        "后台任务正在进行中，请等待当前导入/打包完成后再拖入文件".to_string(),
                        true,
                        Vec::new(),
                    );
                } else {
                    self.add_paths_to_current_sequence(paths);
                }
            }
        }
    }
}

impl eframe::App for MangaPdfApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        let bg = if self.is_dark_theme {
            Color32::from_rgb(17, 24, 39)
        } else {
            Color32::from_rgb(249, 250, 251)
        };
        bg.to_normalized_gamma_f32()
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_async_messages(ctx);

        let is_busy = self.is_generating || self.is_importing;
        self.frame_count += 1;
        // 启动后第 3 帧主动修剪显卡预分配缓存；任务结束与预览关闭时也会调用
        if self.frame_count == 3 {
            trim_process_memory();
        }

        if is_busy {
            ctx.request_repaint();
        }

        self.handle_drag_and_drop(ctx);

        let is_hovering_files = ctx.input(|i| !i.raw.hovered_files.is_empty());
        let mut theme = PanelTheme::new(self.is_dark_theme);

        // 1. 顶部导航栏（可能在本帧切换主题）
        self.render_top_bar(ctx, &theme);
        // 顶部栏切换主题后立即重建配色，避免同一帧里其余面板仍按旧主题绘制
        if theme.is_dark != self.is_dark_theme {
            theme = PanelTheme::new(self.is_dark_theme);
        }

        // 2. 底部状态栏
        self.render_bottom_bar(ctx, &theme, is_busy);

        // 3. 左侧控制面板
        self.render_left_panel(ctx, &theme, is_busy);

        // 4. 中间主面板
        self.render_central_panel(ctx, &theme, is_busy, is_hovering_files);

        // 5. 关于对话框
        self.render_about_dialog(ctx, &theme);

        // 6. 大图画廊预览
        self.render_preview_window(ctx, &theme);

        // 7. 拖拽文件悬停窗口时的视觉高亮（仅在窗口边缘勾勒一圈精致的主题色描边）
        if is_hovering_files {
            let screen_rect = ctx.screen_rect();
            ctx.layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::new("window_drag_outline"),
            ))
            .rect_stroke(
                screen_rect.shrink(1.0),
                Rounding::ZERO,
                Stroke::new(2.5_f32, theme.primary_color),
            );
        }
    }
}
