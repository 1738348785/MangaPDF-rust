use crate::img::{ColorSpace, Filter};
use crate::model::{PageSizeMode, Sequence};
use anyhow::Result;
use pdf_writer::{Content, Filter as PdfFilter, Finish, Name, Pdf, Rect, Ref};
use rayon::prelude::*;
use std::fs::File;
use std::io::Write;
use std::path::Path;

pub fn build_pdf_from_sequence<P, F>(
    sequence: &Sequence,
    output_path: P,
    progress_callback: Option<F>,
) -> Result<usize>
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
    let total_items = sequence.items.len();

    let margin_pt = (sequence.margin_mm as f32) * (72.0 / 25.4); // mm 转 point (1 inch = 25.4mm = 72pt)

    // 分批多核并行转码 (每批 32 张，充分释放多核性能且将内存峰值严格控制在几十兆内)
    const CHUNK_SIZE: usize = 32;
    let processed_counter = std::sync::atomic::AtomicUsize::new(0);

    for chunk in sequence.items.chunks(CHUNK_SIZE) {
        let processed_chunk: Vec<Result<crate::img::ProcessedImage>> = chunk
            .par_iter()
            .map(|item| {
                let res = crate::img::process_image_bytes(
                    item.bytes.clone(),
                    sequence.lossless_direct,
                    sequence.quality,
                );
                let current_done = processed_counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
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
                    eprintln!("⚠️ 跳过损坏图片 [{}]: {:?}", item.name, e);
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
        match processed.filter {
            Filter::DctDecode => image_xobj.filter(PdfFilter::DctDecode),
            Filter::FlateDecode => image_xobj.filter(PdfFilter::FlateDecode),
        };
        image_xobj.finish();

        // 2. 根据页面尺寸模式计算页面宽高与图片变换矩阵
        let (page_w, page_h, draw_w, draw_h, off_x, off_y) = match sequence.page_size_mode {
            PageSizeMode::Original => {
                let pw = processed.width + margin_pt * 2.0;
                let ph = processed.height + margin_pt * 2.0;
                (pw, ph, processed.width, processed.height, margin_pt, margin_pt)
            }
            PageSizeMode::FitA4 => {
                let a4_w = 595.28_f32;
                let a4_h = 841.89_f32;
                let avail_w = (a4_w - margin_pt * 2.0).max(10.0);
                let avail_h = (a4_h - margin_pt * 2.0).max(10.0);
                let scale = (avail_w / processed.width).min(avail_h / processed.height);
                let dw = processed.width * scale;
                let dh = processed.height * scale;
                let ox = margin_pt + (avail_w - dw) / 2.0;
                let oy = margin_pt + (avail_h - dh) / 2.0;
                (a4_w, a4_h, dw, dh, ox, oy)
            }
            PageSizeMode::FixedRatio => {
                let target_w = 1200.0_f32;
                let scale = target_w / processed.width;
                let ph = processed.height * scale;
                (target_w, ph, target_w, ph, 0.0, 0.0)
            }
        };

        // 3. 写入 Content Stream
        let mut content = Content::new();
        content.save_state();
        content.transform([draw_w, 0.0, 0.0, draw_h, off_x, off_y]);
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
        anyhow::bail!("未能成功将任何有效图片写入 PDF");
    }

    // 5. 写入 Page Tree
    let mut page_tree = pdf.pages(page_tree_id);
    page_tree.count(page_ids.len() as i32);
    page_tree.kids(page_ids.iter().copied());
    page_tree.finish();

    // 6. 写入 Catalog
    pdf.catalog(catalog_id).pages(page_tree_id);

    let pdf_data = pdf.finish();
    if let Some(parent) = output_path.as_ref().parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut file = File::create(output_path)?;
    file.write_all(&pdf_data)?;

    Ok(success_count)
}
