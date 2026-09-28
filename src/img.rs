use crate::model::ImageEntry;
use anyhow::{Context, Result};
use std::io::Cursor;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorSpace {
    DeviceGray,
    DeviceRgb,
    DeviceCmyk,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum Filter {
    DctDecode,
    FlateDecode,
}

pub struct ProcessedImage {
    pub width: f32,
    pub height: f32,
    pub color_space: ColorSpace,
    pub bits_per_component: u8,
    pub filter: Filter,
    pub stream_data: Vec<u8>,
}

/// 创建带有缩略图的图片条目 (针对磁盘文件不常驻原始字节，仅保留缩略图以保证极低内存)
pub fn create_image_entry(
    id: usize,
    name: String,
    bytes: Vec<u8>,
    source_path: Option<std::path::PathBuf>,
) -> ImageEntry {
    let (mut width, mut height) = match imagesize::blob_size(&bytes) {
        Ok(size) => (size.width as u32, size.height as u32),
        Err(_) => (800, 1200),
    };

    let mut thumb_rgb = None;
    let mut thumb_w = 0;
    let mut thumb_h = 0;

    // 生成轻量 UI 缩略图 (最大 100x140 像素以维持极低内存占用)
    if let Ok(dyn_img) = image::load_from_memory(&bytes) {
        width = dyn_img.width();
        height = dyn_img.height();
        let thumb = dyn_img.thumbnail(100, 140);
        thumb_w = thumb.width() as usize;
        thumb_h = thumb.height() as usize;
        thumb_rgb = Some(thumb.into_rgb8().into_raw());
    }

    // 若图片来自本地磁盘文件，生成缩略图后立即释放原始大图字节，杜绝内存堆积
    let stored_bytes = if source_path.is_some() {
        Vec::new()
    } else {
        bytes
    };

    ImageEntry {
        id,
        name,
        source_path,
        bytes: stored_bytes,
        width,
        height,
        thumb_rgb,
        thumb_width: thumb_w,
        thumb_height: thumb_h,
    }
}

/// 解析并处理图片数据以供 PDF 组装 (支持原画无损直存：JPEG 零解码直通 + PNG Flate 无损；或按指定画质 10%~100% 压缩重编)
pub fn process_image_bytes(
    bytes: Vec<u8>,
    lossless_direct: bool,
    quality: u8,
) -> Result<ProcessedImage> {
    if lossless_direct {
        if is_jpeg(&bytes) {
            if let Ok(processed) = parse_jpeg_passthrough(bytes.clone()) {
                return Ok(processed);
            }
        }

        if is_png(&bytes) {
            if let Ok(processed) = decode_and_compress_flate(&bytes) {
                return Ok(processed);
            }
        }
    }

    decode_and_compress_with_quality(&bytes, quality)
}

pub fn is_png(data: &[u8]) -> bool {
    data.len() >= 8 && data.starts_with(b"\x89PNG\r\n\x1a\n")
}

fn is_jpeg(data: &[u8]) -> bool {
    data.len() >= 3 && data[0] == 0xFF && data[1] == 0xD8 && data[2] == 0xFF
}

fn parse_jpeg_passthrough(data: Vec<u8>) -> Result<ProcessedImage> {
    let mut cursor = 2;

    while cursor + 4 <= data.len() {
        if data[cursor] != 0xFF {
            cursor += 1;
            continue;
        }

        while cursor < data.len() && data[cursor] == 0xFF {
            cursor += 1;
        }

        if cursor >= data.len() {
            break;
        }

        let marker = data[cursor];
        cursor += 1;

        if marker == 0xD8 || marker == 0xD9 || (0xD0..=0xD7).contains(&marker) || marker == 0x00 || marker == 0x01 {
            continue;
        }

        if cursor + 2 > data.len() {
            break;
        }

        let length = u16::from_be_bytes([data[cursor], data[cursor + 1]]) as usize;
        if length < 2 || cursor + length > data.len() {
            break;
        }

        let is_sof = matches!(
            marker,
            0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF
        );

        if is_sof && length >= 8 {
            let seg = &data[cursor + 2..cursor + length];
            let precision = seg[0];
            let height = u16::from_be_bytes([seg[1], seg[2]]) as f32;
            let width = u16::from_be_bytes([seg[3], seg[4]]) as f32;
            let components = seg[5];

            let color_space = match components {
                1 => ColorSpace::DeviceGray,
                3 => ColorSpace::DeviceRgb,
                4 => ColorSpace::DeviceCmyk,
                _ => ColorSpace::DeviceRgb,
            };

            return Ok(ProcessedImage {
                width,
                height,
                color_space,
                bits_per_component: precision,
                filter: Filter::DctDecode,
                stream_data: data,
            });
        }

        cursor += length;
    }

    anyhow::bail!("无法在 JPEG 中解析到有效的 SOF Marker")
}

fn decode_and_compress_with_quality(bytes: &[u8], quality: u8) -> Result<ProcessedImage> {
    let reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .context("无法识别图片格式")?;
    let dynamic_img = reader.decode().context("图片解码失败")?;

    let width = dynamic_img.width() as f32;
    let height = dynamic_img.height() as f32;
    let q = quality.clamp(10, 100);
    let mut jpeg_buf = Vec::new();

    match dynamic_img.color() {
        image::ColorType::L8 => {
            let luma = dynamic_img.into_luma8();
            let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg_buf, q);
            encoder.encode(
                luma.as_raw(),
                width as u32,
                height as u32,
                image::ExtendedColorType::L8,
            )?;
            Ok(ProcessedImage {
                width,
                height,
                color_space: ColorSpace::DeviceGray,
                bits_per_component: 8,
                filter: Filter::DctDecode,
                stream_data: jpeg_buf,
            })
        }
        _ => {
            let rgb = dynamic_img.into_rgb8();
            let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg_buf, q);
            encoder.encode(
                rgb.as_raw(),
                width as u32,
                height as u32,
                image::ExtendedColorType::Rgb8,
            )?;
            Ok(ProcessedImage {
                width,
                height,
                color_space: ColorSpace::DeviceRgb,
                bits_per_component: 8,
                filter: Filter::DctDecode,
                stream_data: jpeg_buf,
            })
        }
    }
}

pub fn decode_and_compress_flate(bytes: &[u8]) -> Result<ProcessedImage> {
    let reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .context("无法识别图片格式")?;
    let dynamic_img = reader.decode().context("图片解码失败")?;

    let width = dynamic_img.width() as f32;
    let height = dynamic_img.height() as f32;

    let (color_space, raw_pixels) = match dynamic_img.color() {
        image::ColorType::L8 => {
            (ColorSpace::DeviceGray, dynamic_img.into_luma8().into_raw())
        }
        image::ColorType::La8 => {
            let luma_a = dynamic_img.into_luma_alpha8();
            let mut luma = Vec::with_capacity((width * height) as usize);
            for pixel in luma_a.pixels() {
                let l = pixel[0] as u32;
                let a = pixel[1] as u32;
                let val = (l * a + 255 * (255 - a)) / 255;
                luma.push(val as u8);
            }
            (ColorSpace::DeviceGray, luma)
        }
        image::ColorType::Rgba8 => {
            let rgba = dynamic_img.into_rgba8();
            let mut rgb = Vec::with_capacity((width * height * 3.0) as usize);
            for pixel in rgba.pixels() {
                let a = pixel[3] as u32;
                if a == 255 {
                    rgb.push(pixel[0]);
                    rgb.push(pixel[1]);
                    rgb.push(pixel[2]);
                } else if a == 0 {
                    rgb.push(255);
                    rgb.push(255);
                    rgb.push(255);
                } else {
                    let r = (pixel[0] as u32 * a + 255 * (255 - a)) / 255;
                    let g = (pixel[1] as u32 * a + 255 * (255 - a)) / 255;
                    let b = (pixel[2] as u32 * a + 255 * (255 - a)) / 255;
                    rgb.push(r as u8);
                    rgb.push(g as u8);
                    rgb.push(b as u8);
                }
            }
            (ColorSpace::DeviceRgb, rgb)
        }
        _ => {
            (ColorSpace::DeviceRgb, dynamic_img.into_rgb8().into_raw())
        }
    };

    let stream_data = miniz_oxide::deflate::compress_to_vec_zlib(&raw_pixels, 6);

    Ok(ProcessedImage {
        width,
        height,
        color_space,
        bits_per_component: 8,
        filter: Filter::FlateDecode,
        stream_data,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_png_detection_and_flate_compression() {
        let mut png_bytes = Vec::new();
        let img = image::RgbImage::new(10, 10);
        img.write_to(
            &mut std::io::Cursor::new(&mut png_bytes),
            image::ImageFormat::Png,
        )
        .unwrap();

        assert!(is_png(&png_bytes));
        let processed = decode_and_compress_flate(&png_bytes).unwrap();
        assert_eq!(processed.width, 10.0);
        assert_eq!(processed.height, 10.0);
        assert_eq!(processed.filter, Filter::FlateDecode);
        assert_eq!(processed.color_space, ColorSpace::DeviceRgb);
        assert!(!processed.stream_data.is_empty());
    }
}
