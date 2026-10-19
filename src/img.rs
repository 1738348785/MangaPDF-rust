use crate::model::ImageEntry;
use anyhow::{Context, Result};
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::Arc;

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
    pub invert_cmyk: bool,
}

/// 创建带有缩略图的图片条目 (针对磁盘文件和归档条目不常驻原始字节，仅保留缩略图以保证极低内存)
pub fn create_image_entry(
    id: usize,
    name: String,
    bytes: Vec<u8>,
    source_path: Option<PathBuf>,
    archive_source: Option<(Arc<PathBuf>, String)>,
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
        thumb_rgb = Some(Arc::from(thumb.into_rgb8().into_raw()));
    }

    // 若图片来自本地磁盘文件或 ZIP/CBZ 归档，生成缩略图后立即释放原始大图字节，杜绝内存堆积
    let stored_bytes: Arc<[u8]> = if source_path.is_some() || archive_source.is_some() {
        Arc::from(&[][..])
    } else {
        Arc::from(bytes)
    };

    ImageEntry {
        id,
        name,
        source_path,
        archive_source,
        bytes: stored_bytes,
        width,
        height,
        thumb_rgb,
        thumb_width: thumb_w,
        thumb_height: thumb_h,
    }
}

/// 解析并处理图片数据以供 PDF 组装 (支持原画无损直存：JPEG 零解码零拷贝直通 + PNG Flate 无损；或按指定画质 10%~100% 压缩重编)
pub fn process_image_bytes(
    bytes: Vec<u8>,
    lossless_direct: bool,
    quality: u8,
) -> Result<ProcessedImage> {
    if lossless_direct {
        if is_jpeg(&bytes) {
            if let Ok((width, height, color_space, bits_per_component, invert_cmyk)) =
                parse_jpeg_header(&bytes)
            {
                return Ok(ProcessedImage {
                    width,
                    height,
                    color_space,
                    bits_per_component,
                    filter: Filter::DctDecode,
                    stream_data: bytes,
                    invert_cmyk,
                });
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

/// 检查 JPEG EXIF APP1 段是否包含非默认旋转方向 (Orientation != 1)
fn has_non_default_exif_orientation(exif_seg: &[u8]) -> bool {
    if exif_seg.len() < 14 || &exif_seg[0..6] != b"Exif\0\0" {
        return false;
    }
    let tiff = &exif_seg[6..];
    let is_le = match &tiff[0..2] {
        b"II" => true,
        b"MM" => false,
        _ => return false,
    };
    let read_u16 = |offset: usize| -> Option<u16> {
        let b = tiff.get(offset..offset + 2)?;
        Some(if is_le {
            u16::from_le_bytes([b[0], b[1]])
        } else {
            u16::from_be_bytes([b[0], b[1]])
        })
    };
    let read_u32 = |offset: usize| -> Option<u32> {
        let b = tiff.get(offset..offset + 4)?;
        Some(if is_le {
            u32::from_le_bytes([b[0], b[1], b[2], b[3]])
        } else {
            u32::from_be_bytes([b[0], b[1], b[2], b[3]])
        })
    };

    if read_u16(2) != Some(42) {
        return false;
    }
    let Some(ifd_offset) = read_u32(4).map(|v| v as usize) else {
        return false;
    };
    let Some(num_entries) = read_u16(ifd_offset).map(|v| v as usize) else {
        return false;
    };

    for i in 0..num_entries.min(64) {
        let entry_offset = ifd_offset + 2 + i * 12;
        if let Some(tag) = read_u16(entry_offset) {
            if tag == 0x0112 {
                if let Some(orientation) = read_u16(entry_offset + 8) {
                    return (2..=8).contains(&orientation);
                }
            }
        } else {
            break;
        }
    }
    false
}

/// 零拷贝解析 JPEG 头部信息：(width, height, color_space, bits_per_component, invert_cmyk)
fn parse_jpeg_header(data: &[u8]) -> Result<(f32, f32, ColorSpace, u8, bool)> {
    let mut cursor = 2;
    let mut has_adobe_app14 = false;

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

        let seg = &data[cursor + 2..cursor + length];

        // 检查 EXIF Orientation：若带有旋转/翻转标记，则回退至标准解码流程处理
        if marker == 0xE1 && has_non_default_exif_orientation(seg) {
            anyhow::bail!("JPEG 包含非默认 EXIF 方向信息");
        }

        // 检查 Adobe APP14 标记 (用于判断 CMYK JPEG 是否需要反相 Decode)
        if marker == 0xEE && seg.len() >= 12 && seg.starts_with(b"Adobe") {
            has_adobe_app14 = true;
        }

        let is_sof = matches!(
            marker,
            0xC0..=0xC2
        );

        if is_sof && seg.len() >= 6 {
            let precision = seg[0];
            if precision != 8 {
                anyhow::bail!("不支持非 8-bit 精度的 JPEG 直通");
            }
            let height = u16::from_be_bytes([seg[1], seg[2]]) as f32;
            let width = u16::from_be_bytes([seg[3], seg[4]]) as f32;
            let components = seg[5];

            if width <= 0.0 || height <= 0.0 {
                anyhow::bail!("无效的 JPEG 宽高尺寸");
            }

            let (color_space, invert_cmyk) = match components {
                1 => (ColorSpace::DeviceGray, false),
                3 => (ColorSpace::DeviceRgb, false),
                4 => (ColorSpace::DeviceCmyk, has_adobe_app14 || true),
                _ => anyhow::bail!("不支持的 JPEG 通道数: {}", components),
            };

            return Ok((width, height, color_space, precision, invert_cmyk));
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
                invert_cmyk: false,
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
                invert_cmyk: false,
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
        invert_cmyk: false,
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
