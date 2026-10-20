use crate::img::{ColorSpace, Filter};
use crate::model::{PageSizeMode, Sequence};
use anyhow::Result;
use pdf_writer::{Content, Filter as PdfFilter, Finish, Name, Pdf, Rect, Ref};
use rayon::prelude::*;
use std::fs::File;
use std::io::Write;
use std::path::Path;

/// PDF 生成结果：成功写入的页数 + 被跳过的图片 (文件名: 原因)
pub struct BuildReport {
    pub pages: usize,
    pub skipped: Vec<String>,
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

/// 为当前批次批量提取原始图片字节。
/// 若批次中存在来自同一 ZIP/CBZ 归档的多张图片，一次性打开归档并连续读取，避免多线程重复解析中央目录。
fn fetch_chunk_bytes(chunk: &[crate::model::ImageEntry]) -> Vec<Result<Vec<u8>, String>> {
    let mut results: Vec<Option<Result<Vec<u8>, String>>> =
        (0..chunk.len()).map(|_| None).collect();
    use std::collections::BTreeMap;
    let mut archive_groups: BTreeMap<std::path::PathBuf, Vec<(usize, String)>> = BTreeMap::new();

    for (idx, item) in chunk.iter().enumerate() {
        if let Some(path) = &item.source_path {
            results[idx] = Some(
                std::fs::read(path)
                    .map_err(|e| format!("无法读取图片文件 {}: {}", path.display(), e)),
            );
        } else if let Some((archive_path, entry_name)) = &item.archive_source {
            archive_groups
                .entry((**archive_path).clone())
                .or_default()
                .push((idx, entry_name.clone()));
        } else {
            results[idx] = Some(Ok(item.bytes.to_vec()));
        }
    }

    for (archive_path, entries) in archive_groups {
        match std::fs::File::open(&archive_path) {
            Ok(file) => match zip::ZipArchive::new(file) {
                Ok(mut archive) => {
                    for (idx, entry_name) in entries {
                        match archive.by_name(&entry_name) {
                            Ok(mut zip_file) => {
                                let mut buf = Vec::with_capacity(zip_file.size() as usize);
                                use std::io::Read;
                                match zip_file.read_to_end(&mut buf) {
                                    Ok(_) => results[idx] = Some(Ok(buf)),
                                    Err(e) => {
                                        results[idx] = Some(Err(format!(
                                            "读取归档内图片 {} 失败: {}",
                                            entry_name, e
                                        )))
                                    }
                                }
                            }
                            Err(e) => {
                                results[idx] =
                                    Some(Err(format!("归档中找不到图片 {}: {}", entry_name, e)))
                            }
                        }
                    }
                }
                Err(e) => {
                    let err_msg = format!("无法解析归档 {}: {}", archive_path.display(), e);
                    for (idx, _) in entries {
                        results[idx] = Some(Err(err_msg.clone()));
                    }
                }
            },
            Err(e) => {
                let err_msg = format!("无法打开归档文件 {}: {}", archive_path.display(), e);
                for (idx, _) in entries {
                    results[idx] = Some(Err(err_msg.clone()));
                }
            }
        }
    }

    results
        .into_iter()
        .map(|opt| opt.unwrap_or_else(|| Err("未知读取错误".to_string())))
        .collect()
}

pub fn build_pdf_from_sequence<P, F>(
    sequence: &Sequence,
    output_path: P,
    progress_callback: Option<F>,
) -> Result<BuildReport>
where
    P: AsRef<Path>,
    F: Fn(usize, usize, &str) + Send + Sync,
{
    if sequence.items.is_empty() {
        anyhow::bail!("序列中没有任何图片");
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

    // 分批多核并行转码 (每批 32 张，充分释放多核性能且将内存峰值严格控制在几十兆内)
    const CHUNK_SIZE: usize = 32;
    let processed_counter = std::sync::atomic::AtomicUsize::new(0);

    for chunk in sequence.items.chunks(CHUNK_SIZE) {
        let chunk_bytes = fetch_chunk_bytes(chunk);
        let processed_chunk: Vec<Result<crate::img::ProcessedImage>> = chunk
            .par_iter()
            .zip(chunk_bytes)
            .map(|(item, raw_bytes_res)| {
                let raw_bytes = match raw_bytes_res {
                    Ok(b) => b,
                    Err(e) => return Err(anyhow::anyhow!(e)),
                };
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

        for (item, res) in chunk.iter().zip(processed_chunk) {
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
                Filter::DctDecode => image_xobj.filter(PdfFilter::DctDecode),
                Filter::FlateDecode => image_xobj.filter(PdfFilter::FlateDecode),
            };
            image_xobj.finish();

            // 2. 根据页面尺寸模式计算页面宽高与图片变换矩阵 (使用按 EXIF 方向校正后的显示尺寸)
            let (img_w, img_h) = processed.display_size();
            let (mut page_w, mut page_h, mut draw_w, mut draw_h, mut off_x, mut off_y) =
                match sequence.page_size_mode {
                    PageSizeMode::Original => {
                        let pw = img_w + margin_pt * 2.0;
                        let ph = img_h + margin_pt * 2.0;
                        (pw, ph, img_w, img_h, margin_pt, margin_pt)
                    }
                    PageSizeMode::FitA4 => {
                        let a4_w = 595.28_f32;
                        let a4_h = 841.89_f32;
                        let avail_w = (a4_w - margin_pt * 2.0).max(10.0);
                        let avail_h = (a4_h - margin_pt * 2.0).max(10.0);
                        let scale = (avail_w / img_w).min(avail_h / img_h);
                        let dw = img_w * scale;
                        let dh = img_h * scale;
                        let ox = margin_pt + (avail_w - dw) / 2.0;
                        let oy = margin_pt + (avail_h - dh) / 2.0;
                        (a4_w, a4_h, dw, dh, ox, oy)
                    }
                    PageSizeMode::FixedRatio => {
                        let target_w = 1200.0_f32;
                        let avail_w = (target_w - margin_pt * 2.0).max(10.0);
                        let scale = avail_w / img_w.max(1.0);
                        let dh = img_h * scale;
                        let ph = dh + margin_pt * 2.0;
                        (target_w, ph, avail_w, dh, margin_pt, margin_pt)
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

            // 3. 写入 Content Stream
            let mut content = Content::new();
            content.save_state();
            content.transform(orientation_matrix(
                processed.orientation,
                draw_w,
                draw_h,
                off_x,
                off_y,
            ));
            content.x_object(Name(b"Im0"));
            content.restore_state();
            let content_bytes = content.finish();
            pdf.stream(content_id, &content_bytes);

            // 4. 写入 Page 对象
            let mut page = pdf.page(page_id);
            page.parent(page_tree_id);
            page.media_box(Rect::new(0.0, 0.0, page_w, page_h));
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

    let pdf_data = pdf.finish();
    let out_path = output_path.as_ref();
    if let Some(parent) = out_path.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            std::fs::create_dir_all(parent).map_err(|e| {
                anyhow::anyhow!("无法创建输出目录 \"{}\"：{}", parent.display(), e)
            })?;
        }
    }

    let mut file = File::create(out_path).map_err(|e| {
        let hint = if out_path.is_dir() {
            "（目标路径是一个已存在的文件夹）"
        } else if matches!(e.raw_os_error(), Some(5) | Some(32)) {
            "（请检查同名 PDF 是否正被阅读器打开占用，或输出目录是否有写入权限）"
        } else {
            ""
        };
        anyhow::anyhow!("无法写入 \"{}\"：{}{}", out_path.display(), e, hint)
    })?;
    file.write_all(&pdf_data)
        .map_err(|e| anyhow::anyhow!("写入 PDF 数据失败 \"{}\"：{}", out_path.display(), e))?;

    Ok(BuildReport {
        pages: success_count,
        skipped,
    })
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
        let max_dim = 28800.0_f32;
        let mut page_w = 800.0_f32;
        let mut page_h = max_dim;
        let mut draw_w = 800.0_f32;
        let mut draw_h = max_dim;
        let mut off_x = 0.0_f32;
        let mut off_y = 0.0_f32;

        const MAX_PDF_PAGE_PT: f32 = 14400.0;
        let dim = page_w.max(page_h);
        if dim > MAX_PDF_PAGE_PT {
            let downscale = MAX_PDF_PAGE_PT / dim;
            page_w *= downscale;
            page_h *= downscale;
            draw_w *= downscale;
            draw_h *= downscale;
            off_x *= downscale;
            off_y *= downscale;
        }

        assert_eq!(page_h, 14400.0);
        assert_eq!(page_w, 400.0);
        assert_eq!(draw_h, 14400.0);
        assert_eq!(draw_w, 400.0);
        assert_eq!(off_x, 0.0);
        assert_eq!(off_y, 0.0);
    }
}
