use crate::img::{ColorSpace, Filter};
use crate::model::{ImageEntry, PageSizeMode, Sequence};
use anyhow::Result;
use pdf_writer::{Content, Filter as PdfFilter, Finish, Name, Pdf, Rect, Ref};
use rayon::prelude::*;
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// 任务被用户主动取消时返回的哨兵错误类型
#[derive(Debug)]
pub struct CancelledError;

impl std::fmt::Display for CancelledError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "任务已取消")
    }
}

impl std::error::Error for CancelledError {}

#[inline]
fn is_cancelled(cancel_flag: Option<&AtomicBool>) -> bool {
    cancel_flag.is_some_and(|flag| flag.load(Ordering::Relaxed))
}

/// PDF 生成结果：成功写入的页数 + 被跳过的图片 (文件名: 原因)
pub struct BuildReport {
    pub pages: usize,
    pub skipped: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageLayoutBox {
    pub page_w: f32,
    pub page_h: f32,
    pub draw_w: f32,
    pub draw_h: f32,
    pub off_x: f32,
    pub off_y: f32,
}

/// 根据页面尺寸模式、方向校正后的图片尺寸与边距 (pt) 计算 PDF 页面尺寸与绘制区域。
/// 自动钳制极端边距防止坐标越出页面，并按 PDF 规范上限 (14400 pt) 等比缩放超长条漫。
pub fn compute_page_layout(
    mode: PageSizeMode,
    img_w: f32,
    img_h: f32,
    margin_pt: f32,
) -> PageLayoutBox {
    let img_w = img_w.max(1.0);
    let img_h = img_h.max(1.0);
    let margin_pt = margin_pt.max(0.0);

    let (mut page_w, mut page_h, mut draw_w, mut draw_h, mut off_x, mut off_y) = match mode {
        PageSizeMode::Original => {
            let pw = img_w + margin_pt * 2.0;
            let ph = img_h + margin_pt * 2.0;
            (pw, ph, img_w, img_h, margin_pt, margin_pt)
        }
        PageSizeMode::FitA4 => {
            let a4_w = 595.28_f32;
            let a4_h = 841.89_f32;
            let eff_margin = margin_pt.min((a4_w - 10.0) / 2.0).min((a4_h - 10.0) / 2.0);
            let avail_w = (a4_w - eff_margin * 2.0).max(10.0);
            let avail_h = (a4_h - eff_margin * 2.0).max(10.0);
            let scale = (avail_w / img_w).min(avail_h / img_h);
            let dw = img_w * scale;
            let dh = img_h * scale;
            let ox = eff_margin + (avail_w - dw) / 2.0;
            let oy = eff_margin + (avail_h - dh) / 2.0;
            (a4_w, a4_h, dw, dh, ox, oy)
        }
        PageSizeMode::FixedRatio => {
            let target_w = 1200.0_f32;
            let eff_margin = margin_pt.min((target_w - 10.0) / 2.0);
            let avail_w = (target_w - eff_margin * 2.0).max(10.0);
            let scale = avail_w / img_w;
            let dh = img_h * scale;
            let ph = dh + eff_margin * 2.0;
            (target_w, ph, avail_w, dh, eff_margin, eff_margin)
        }
    };

    // PDF 规范限制单个页面尺寸上限为 14400 pt (200 英寸)。
    // 针对超长条漫/韩漫，若长边超过 14400 pt，等比缩放页面与绘制坐标至规范范围内（底层原始图片像素与画质 100% 保持不变）
    const MAX_PDF_PAGE_PT: f32 = 14400.0;
    let max_dim = page_w.max(page_h);
    if max_dim > MAX_PDF_PAGE_PT {
        let downscale = MAX_PDF_PAGE_PT / max_dim;
        page_w *= downscale;
        page_h *= downscale;
        draw_w *= downscale;
        draw_h *= downscale;
        off_x *= downscale;
        off_y *= downscale;
    }

    PageLayoutBox {
        page_w,
        page_h,
        draw_w,
        draw_h,
        off_x,
        off_y,
    }
}

/// 计算将图片单位方块映射到页面显示区域 (off_x, off_y, draw_w, draw_h) 的变换矩阵，
/// 同时按 EXIF 方向 (1~8) 完成旋转/翻转。draw_w / draw_h 为校正方向后的显示尺寸。
///
/// PDF 图像空间中 (0,1) 对应原始图像左上角像素，(1,0) 对应右下角。
pub fn orientation_matrix(
    orientation: u8,
    draw_w: f32,
    draw_h: f32,
    off_x: f32,
    off_y: f32,
) -> [f32; 6] {
    let (w, h, x, y) = (draw_w, draw_h, off_x, off_y);
    match orientation {
        2 => [-w, 0.0, 0.0, h, x + w, y],      // 水平镜像
        3 => [-w, 0.0, 0.0, -h, x + w, y + h], // 旋转 180°
        4 => [w, 0.0, 0.0, -h, x, y + h],      // 垂直镜像
        5 => [0.0, -h, -w, 0.0, x + w, y + h], // 转置 (主对角线镜像)
        6 => [0.0, -h, w, 0.0, x, y + h],      // 顺时针旋转 90°
        7 => [0.0, h, w, 0.0, x, y],           // 反转置
        8 => [0.0, h, -w, 0.0, x + w, y],      // 逆时针旋转 90°
        _ => [w, 0.0, 0.0, h, x, y],           // 1 / 未知：保持原样
    }
}

/// 跨批次复用的 ZIP/CBZ 归档读取缓存，避免每 32 页重复打开文件并重新解析中央目录
struct ArchiveReaderCache {
    readers: HashMap<PathBuf, Result<zip::ZipArchive<File>, String>>,
}

impl ArchiveReaderCache {
    fn new() -> Self {
        Self {
            readers: HashMap::new(),
        }
    }

    fn read_entry(
        &mut self,
        archive_path: &Path,
        entry_index: usize,
        decoded_path: &str,
    ) -> Result<Vec<u8>, String> {
        let entry = self
            .readers
            .entry(archive_path.to_path_buf())
            .or_insert_with(|| {
                let file = File::open(archive_path).map_err(|e| {
                    format!("无法打开归档文件 {}: {}", archive_path.display(), e)
                })?;
                zip::ZipArchive::new(file)
                    .map_err(|e| format!("无法解析归档 {}: {}", archive_path.display(), e))
            });

        let archive = match entry {
            Ok(arch) => arch,
            Err(err_msg) => return Err(err_msg.clone()),
        };

        let mut zip_file = archive
            .by_index(entry_index)
            .map_err(|e| format!("归档中找不到图片 {}: {}", decoded_path, e))?;
        let cap = zip_file.size().min(crate::model::MAX_ZIP_PREALLOC_BYTES) as usize;
        let mut buf = Vec::with_capacity(cap);
        zip_file
            .read_to_end(&mut buf)
            .map_err(|e| format!("读取归档内图片 {} 失败: {}", decoded_path, e))?;
        Ok(buf)
    }
}

type ChunkArchiveBytes = Vec<Option<Result<Vec<u8>, String>>>;

/// 为当前批次预读归档条目字节（复用已打开的 ZipArchive 句柄）；
/// 本地磁盘文件留给 Rayon 线程池并行读取，既发挥并发 I/O 又能进一步减少批次缓冲占用。
fn prefetch_chunk_archive_bytes(
    chunk: &[ImageEntry],
    archive_cache: &mut ArchiveReaderCache,
    cancel_flag: Option<&AtomicBool>,
) -> Result<ChunkArchiveBytes> {
    let mut out = Vec::with_capacity(chunk.len());
    for item in chunk {
        if is_cancelled(cancel_flag) {
            return Err(CancelledError.into());
        }
        let fetched = item.archive_source.as_ref().map(|arch| {
            archive_cache.read_entry(
                arch.archive_path.as_ref(),
                arch.entry_index,
                &arch.decoded_path,
            )
        });
        out.push(fetched);
    }
    Ok(out)
}

#[allow(dead_code)]
pub fn build_pdf_from_sequence<P, F>(
    sequence: &Sequence,
    output_path: P,
    progress_callback: Option<F>,
) -> Result<BuildReport>
where
    P: AsRef<Path>,
    F: Fn(usize, usize, &str) + Send + Sync,
{
    build_pdf_from_sequence_with_cancel(sequence, output_path, progress_callback, None)
}

pub fn build_pdf_from_sequence_with_cancel<P, F>(
    sequence: &Sequence,
    output_path: P,
    progress_callback: Option<F>,
    cancel_flag: Option<&AtomicBool>,
) -> Result<BuildReport>
where
    P: AsRef<Path>,
    F: Fn(usize, usize, &str) + Send + Sync,
{
    if sequence.items.is_empty() {
        anyhow::bail!("序列中没有任何图片");
    }
    if is_cancelled(cancel_flag) {
        return Err(CancelledError.into());
    }

    let mut pdf = Pdf::new();
    let catalog_id = Ref::new(1);
    let page_tree_id = Ref::new(2);

    let mut next_id = 3;
    let mut page_ids = Vec::with_capacity(sequence.items.len());
    let mut success_count = 0;
    let mut skipped: Vec<String> = Vec::new();
    let total_items = sequence.items.len();

    let margin_pt = (sequence.margin_mm as f32) * (72.0 / 25.4); // mm 转 point (1 inch = 25.4mm = 72pt)

    // 分批多核并行转码，整卷复用 ZIP/CBZ 归档句柄避免每批重复解析中央目录
    const CHUNK_SIZE: usize = 32;
    let processed_counter = std::sync::atomic::AtomicUsize::new(0);
    let mut archive_cache = ArchiveReaderCache::new();

    for chunk in sequence.items.chunks(CHUNK_SIZE) {
        if is_cancelled(cancel_flag) {
            return Err(CancelledError.into());
        }
        let prefetched_archives =
            prefetch_chunk_archive_bytes(chunk, &mut archive_cache, cancel_flag)?;
        let processed_chunk: Vec<Result<crate::img::ProcessedImage>> = chunk
            .par_iter()
            .zip(prefetched_archives)
            .map(|(item, archive_bytes_opt)| {
                if is_cancelled(cancel_flag) {
                    return Err(CancelledError.into());
                }
                let raw_bytes_res = match archive_bytes_opt {
                    Some(res) => res,
                    None => {
                        if let Some(path) = &item.source_path {
                            std::fs::read(path).map_err(|e| {
                                format!("无法读取图片文件 {}: {}", path.display(), e)
                            })
                        } else {
                            Ok(item.bytes.to_vec())
                        }
                    }
                };
                let raw_bytes = match raw_bytes_res {
                    Ok(b) => b,
                    Err(e) => return Err(anyhow::anyhow!(e)),
                };
                if is_cancelled(cancel_flag) {
                    return Err(CancelledError.into());
                }
                let res = crate::img::process_image_bytes(
                    raw_bytes,
                    sequence.lossless_direct,
                    sequence.quality,
                );
                let current_done =
                    processed_counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                if let Some(cb) = &progress_callback {
                    cb(current_done, total_items, &item.name);
                }
                res
            })
            .collect();

        if is_cancelled(cancel_flag) {
            return Err(CancelledError.into());
        }

        for (item, res) in chunk.iter().zip(processed_chunk) {
            // 逐页检查取消：此前只在每个批次前后检查，点「取消」后最多还要再写完 32 页才有反应
            if is_cancelled(cancel_flag) {
                return Err(CancelledError.into());
            }
            let processed = match res {
                Ok(img) => img,
                Err(e) => {
                    // 窗口程序无控制台，eprintln 不可见：记录下来交给界面展示
                    skipped.push(format!("{}: {:#}", item.name, e));
                    continue;
                }
            };

            let page_id = Ref::new(next_id);
            next_id += 1;
            let image_id = Ref::new(next_id);
            next_id += 1;
            let content_id = Ref::new(next_id);
            next_id += 1;

            page_ids.push(page_id);

            // 1. 写入 Image XObject
            let mut image_xobj = pdf.image_xobject(image_id, &processed.stream_data);
            image_xobj.width(processed.width as i32);
            image_xobj.height(processed.height as i32);
            match processed.color_space {
                ColorSpace::DeviceGray => image_xobj.color_space().device_gray(),
                ColorSpace::DeviceRgb => image_xobj.color_space().device_rgb(),
                ColorSpace::DeviceCmyk => image_xobj.color_space().device_cmyk(),
            };
            image_xobj.bits_per_component(processed.bits_per_component as i32);
            if processed.invert_cmyk {
                image_xobj.decode([1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0, 0.0]);
            }
            match processed.filter {
                Filter::DctDecode => {
                    image_xobj.filter(PdfFilter::DctDecode);
                    if let Some(ct) = processed.dct_color_transform {
                        image_xobj.decode_parms().color_transform(ct);
                    }
                }
                Filter::FlateDecode => {
                    image_xobj.filter(PdfFilter::FlateDecode);
                    if processed.flate_predictor {
                        let colors = match processed.color_space {
                            ColorSpace::DeviceGray => 1,
                            ColorSpace::DeviceRgb => 3,
                            ColorSpace::DeviceCmyk => 4,
                        };
                        image_xobj
                            .decode_parms()
                            .predictor(pdf_writer::types::Predictor::PngOptimum)
                            .colors(colors)
                            .bits_per_component(processed.bits_per_component as i32)
                            .columns(processed.width as i32);
                    }
                }
            }
            image_xobj.finish();

            // 2. 根据页面尺寸模式计算页面宽高与图片变换矩阵 (使用按 EXIF 方向校正后的显示尺寸)
            let (img_w, img_h) = processed.display_size();
            let layout = compute_page_layout(sequence.page_size_mode, img_w, img_h, margin_pt);

            // 3. 写入 Content Stream
            let mut content = Content::new();
            content.save_state();
            content.transform(orientation_matrix(
                processed.orientation,
                layout.draw_w,
                layout.draw_h,
                layout.off_x,
                layout.off_y,
            ));
            content.x_object(Name(b"Im0"));
            content.restore_state();
            let content_bytes = content.finish();
            pdf.stream(content_id, &content_bytes);

            // 4. 写入 Page 对象
            let mut page = pdf.page(page_id);
            page.parent(page_tree_id);
            page.media_box(Rect::new(0.0, 0.0, layout.page_w, layout.page_h));
            let mut resources = page.resources();
            resources.x_objects().pair(Name(b"Im0"), image_id);
            resources.finish();
            page.contents(content_id);
            page.finish();

            success_count += 1;
        }
    }

    if page_ids.is_empty() {
        let preview: Vec<&str> = skipped.iter().take(3).map(String::as_str).collect();
        anyhow::bail!(
            "未能成功将任何有效图片写入 PDF（{} 张全部处理失败：{}{}）",
            skipped.len(),
            preview.join("；"),
            if skipped.len() > 3 { " 等" } else { "" }
        );
    }

    // 5. 写入 Page Tree
    let mut page_tree = pdf.pages(page_tree_id);
    page_tree.count(page_ids.len() as i32);
    page_tree.kids(page_ids.iter().copied());
    page_tree.finish();

    // 6. 写入 Catalog
    let mut catalog = pdf.catalog(catalog_id);
    catalog.pages(page_tree_id);
    if sequence.manga_rtl_spread {
        catalog.page_layout(pdf_writer::types::PageLayout::TwoPageRight);
        catalog
            .viewer_preferences()
            .direction(pdf_writer::types::Direction::R2L);
    }
    catalog.finish();

    if is_cancelled(cancel_flag) {
        return Err(CancelledError.into());
    }

    let pdf_data = pdf.finish();
    write_pdf_atomically(output_path.as_ref(), &pdf_data)?;

    Ok(BuildReport {
        pages: success_count,
        skipped,
    })
}

/// 原子化写入 PDF 文件：先写入同目录下的临时文件，成功刷盘并关闭句柄后再原子替换目标文件，
/// 防止覆盖已有 PDF 时因中途磁盘空间不足或写入异常将原文件截断损坏。
fn write_pdf_atomically(out_path: &Path, pdf_data: &[u8]) -> Result<()> {
    if out_path.is_dir() {
        anyhow::bail!(
            "无法写入 \"{}\"：（目标路径是一个已存在的文件夹）",
            out_path.display()
        );
    }

    let parent = out_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if !parent.exists() {
        std::fs::create_dir_all(parent)
            .map_err(|e| anyhow::anyhow!("无法创建输出目录 \"{}\"：{}", parent.display(), e))?;
    }

    static TMP_COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let seq = TMP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    // 使用简短固定前缀的临时文件名，避免原文件名达 160 字符时在深层目录下超出 Windows MAX_PATH (260)
    let tmp_path = parent.join(format!(".mangapdf_{}_{}.tmp", std::process::id(), seq));

    let write_res = (|| -> std::io::Result<()> {
        let mut tmp_file = File::create(&tmp_path)?;
        tmp_file.write_all(pdf_data)?;
        tmp_file.flush()?;
        Ok(())
    })();

    if let Err(e) = write_res {
        let _ = std::fs::remove_file(&tmp_path);
        anyhow::bail!("写入 PDF 数据失败 \"{}\"：{}", out_path.display(), e);
    }

    if let Err(e) = std::fs::rename(&tmp_path, out_path) {
        let _ = std::fs::remove_file(&tmp_path);
        let hint = if matches!(e.raw_os_error(), Some(5) | Some(32)) {
            "（请检查同名 PDF 是否正被阅读器打开占用，或输出目录是否有写入权限）"
        } else {
            ""
        };
        anyhow::bail!("无法写入 \"{}\"：{}{}", out_path.display(), e, hint);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 将图像单位方块坐标 (u, v) 经矩阵映射到页面坐标
    fn apply(m: [f32; 6], u: f32, v: f32) -> (f32, f32) {
        (m[0] * u + m[2] * v + m[4], m[1] * u + m[3] * v + m[5])
    }

    /// 参考实现：EXIF 方向定义的「原图像素 (xs, ys) → 显示位置 (X, Y)」，坐标均为左上角起算的 0~1 比例
    fn exif_reference(o: u8, xs: f32, ys: f32) -> (f32, f32) {
        match o {
            2 => (1.0 - xs, ys),
            3 => (1.0 - xs, 1.0 - ys),
            4 => (xs, 1.0 - ys),
            5 => (ys, xs),
            6 => (1.0 - ys, xs),
            7 => (1.0 - ys, 1.0 - xs),
            8 => (ys, 1.0 - xs),
            _ => (xs, ys),
        }
    }

    #[test]
    fn test_orientation_matrix_matches_exif_definition() {
        let (dw, dh, ox, oy) = (300.0_f32, 500.0_f32, 10.0_f32, 20.0_f32);
        for o in 1..=8u8 {
            let m = orientation_matrix(o, dw, dh, ox, oy);
            for &(xs, ys) in &[(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0), (0.25, 0.75)] {
                // 图像空间：u = xs，v = 1 - ys (PDF 图像空间 v 轴向上)
                let (px, py) = apply(m, xs, 1.0 - ys);
                let (dx, dy) = exif_reference(o, xs, ys);
                // 显示坐标 → 页面坐标 (页面 y 轴向上)
                let (ex, ey) = (ox + dx * dw, oy + (1.0 - dy) * dh);
                assert!(
                    (px - ex).abs() < 1e-3 && (py - ey).abs() < 1e-3,
                    "orientation {o}: ({xs},{ys}) -> ({px},{py}), expected ({ex},{ey})"
                );
            }
        }
    }

    #[test]
    fn test_webtoon_dimension_cap_under_14400_pt() {
        let layout = compute_page_layout(PageSizeMode::Original, 800.0, 28800.0, 0.0);
        assert_eq!(layout.page_h, 14400.0);
        assert_eq!(layout.page_w, 400.0);
        assert_eq!(layout.draw_h, 14400.0);
        assert_eq!(layout.draw_w, 400.0);
        assert_eq!(layout.off_x, 0.0);
        assert_eq!(layout.off_y, 0.0);
    }

    #[test]
    fn test_fit_a4_clamps_excessive_margin_within_page_bounds() {
        let layout = compute_page_layout(PageSizeMode::FitA4, 800.0, 1200.0, 1000.0);
        assert!(layout.off_x >= 0.0);
        assert!(layout.off_y >= 0.0);
        assert!(layout.off_x + layout.draw_w <= layout.page_w + 1e-2);
        assert!(layout.off_y + layout.draw_h <= layout.page_h + 1e-2);
    }

    #[test]
    fn test_pdf_flate_image_writes_png_predictor_decode_parms() {
        let mut png_bytes = Vec::new();
        let img = image::RgbImage::from_fn(24, 16, |x, y| {
            image::Rgb([(x * 8) as u8, (y * 12) as u8, 120])
        });
        img.write_to(
            &mut std::io::Cursor::new(&mut png_bytes),
            image::ImageFormat::Png,
        )
        .unwrap();

        let entry = crate::img::create_image_entry(1, "page1.png".into(), png_bytes, None, None);
        let mut seq = Sequence::new(1, "predictor_test".into());
        seq.items.push(entry);

        let out_path = std::env::temp_dir().join(format!(
            "mangapdf_predictor_test_{}.pdf",
            std::process::id()
        ));
        let report =
            build_pdf_from_sequence(&seq, &out_path, None::<fn(usize, usize, &str)>).unwrap();
        assert_eq!(report.pages, 1);
        assert!(report.skipped.is_empty());

        let pdf_bytes = std::fs::read(&out_path).unwrap();
        let _ = std::fs::remove_file(&out_path);
        let pdf_text = String::from_utf8_lossy(&pdf_bytes);
        assert!(pdf_text.contains("/Filter /FlateDecode"));
        assert!(pdf_text.contains("/DecodeParms"));
        assert!(pdf_text.contains("/Predictor 15"));
        assert!(pdf_text.contains("/Colors 3"));
        assert!(pdf_text.contains("/Columns 24"));
    }

    #[test]
    fn test_pdf_multi_chunk_zip_archive_reuses_reader() {
        use std::sync::Arc;
        use zip::write::SimpleFileOptions;

        let mut png_bytes = Vec::new();
        let img = image::GrayImage::from_fn(12, 12, |x, y| image::Luma([(x + y) as u8 * 8]));
        img.write_to(
            &mut std::io::Cursor::new(&mut png_bytes),
            image::ImageFormat::Png,
        )
        .unwrap();

        let zip_path = std::env::temp_dir().join(format!(
            "mangapdf_multichunk_test_{}.cbz",
            std::process::id()
        ));
        {
            let file = File::create(&zip_path).unwrap();
            let mut zip_writer = zip::ZipWriter::new(file);
            let opts = SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            // 40 页 > CHUNK_SIZE (32)，验证跨批次复用归档句柄
            for i in 1..=40usize {
                zip_writer
                    .start_file(format!("{:03}.png", i), opts)
                    .unwrap();
                zip_writer.write_all(&png_bytes).unwrap();
            }
            zip_writer.finish().unwrap();
        }

        let arch_arc = Arc::new(zip_path.clone());
        let mut seq = Sequence::new(1, "cbz_test".into());
        for i in 1..=40usize {
            let name = format!("{:03}.png", i);
            seq.items.push(ImageEntry {
                id: i,
                name: name.clone(),
                sort_key: name.clone(),
                source_path: None,
                archive_source: Some(crate::model::ArchiveEntryRef {
                    archive_path: Arc::clone(&arch_arc),
                    entry_index: i - 1,
                    decoded_path: name,
                }),
                bytes: Arc::from(&[][..]),
                width: 12,
                height: 12,
                thumb_rgb: None,
                thumb_width: 0,
                thumb_height: 0,
                thumb_error: None,
            });
        }

        let out_path = std::env::temp_dir().join(format!(
            "mangapdf_multichunk_out_{}.pdf",
            std::process::id()
        ));
        let report =
            build_pdf_from_sequence(&seq, &out_path, None::<fn(usize, usize, &str)>).unwrap();
        let _ = std::fs::remove_file(&zip_path);
        let _ = std::fs::remove_file(&out_path);

        assert_eq!(report.pages, 40);
        assert!(report.skipped.is_empty());
    }

    #[test]
    fn test_build_pdf_respects_cancel_flag() {
        let mut png_bytes = Vec::new();
        let img = image::GrayImage::from_fn(8, 8, |x, _| image::Luma([x as u8 * 20]));
        img.write_to(
            &mut std::io::Cursor::new(&mut png_bytes),
            image::ImageFormat::Png,
        )
        .unwrap();

        let entry = crate::img::create_image_entry(1, "p1.png".into(), png_bytes, None, None);
        let mut seq = Sequence::new(1, "cancel_test".into());
        seq.items.push(entry);

        let out_path = std::env::temp_dir().join(format!(
            "mangapdf_cancel_test_{}.pdf",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&out_path);

        let cancel = AtomicBool::new(true);
        let res = build_pdf_from_sequence_with_cancel(
            &seq,
            &out_path,
            None::<fn(usize, usize, &str)>,
            Some(&cancel),
        );
        assert!(res.is_err());
        assert!(res.err().unwrap().is::<CancelledError>());
        assert!(!out_path.exists(), "取消后不应向磁盘写出任何 PDF 文件");
    }

    #[test]
    fn test_pdf_writes_16bit_grayscale_png_without_bit_depth_loss() {
        let img =
            image::ImageBuffer::<image::Luma<u16>, Vec<u16>>::from_fn(18, 12, |x, y| {
                image::Luma([(x as u16 * 1234).wrapping_add(y as u16 * 77)])
            });
        let mut png_bytes = Vec::new();
        img.write_to(
            &mut std::io::Cursor::new(&mut png_bytes),
            image::ImageFormat::Png,
        )
        .unwrap();

        let entry = crate::img::create_image_entry(1, "page16.png".into(), png_bytes, None, None);
        let mut seq = Sequence::new(1, "p16".into());
        seq.items.push(entry);

        let out_path = std::env::temp_dir().join(format!(
            "mangapdf_p16_test_{}.pdf",
            std::process::id()
        ));
        let report =
            build_pdf_from_sequence(&seq, &out_path, None::<fn(usize, usize, &str)>).unwrap();
        assert_eq!(report.pages, 1);
        assert!(report.skipped.is_empty());

        let pdf_bytes = std::fs::read(&out_path).unwrap();
        let _ = std::fs::remove_file(&out_path);
        let pdf_text = String::from_utf8_lossy(&pdf_bytes);
        assert!(
            pdf_text.contains("/BitsPerComponent 16"),
            "16-bit 灰度 PNG 必须以 16-bit 写入 PDF"
        );
        assert!(pdf_text.contains("/Colors 1"));
        assert!(pdf_text.contains("/Predictor 15"));
    }
}
