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
        let processed_chunk: Vec<Result<crate::img::ProcessedImage>> = chunk
            .par_iter()
            .map(|item| {
                let raw_bytes = match item.get_bytes() {
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
            let (page_w, page_h, draw_w, draw_h, off_x, off_y) = match sequence.page_size_mode {
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
    if let Some(parent) = output_path.as_ref().parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut file = File::create(output_path)?;
    file.write_all(&pdf_data)?;

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
}
