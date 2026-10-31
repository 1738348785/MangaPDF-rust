use crate::model::ImageEntry;
use anyhow::{Context, Result};
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::Arc;

#[allow(clippy::enum_variant_names)]
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
    /// EXIF 方向 (1~8)。仅 JPEG 无损直通时可能 != 1，由 PDF 变换矩阵完成旋转/翻转，
    /// 解码重编路径已在像素层面校正，恒为 1
    pub orientation: u8,
}

impl ProcessedImage {
    /// 按 EXIF 方向校正后的显示尺寸 (方向 5~8 需交换宽高)
    pub fn display_size(&self) -> (f32, f32) {
        if (5..=8).contains(&self.orientation) {
            (self.height, self.width)
        } else {
            (self.width, self.height)
        }
    }
}

/// JPEG 头部解析结果
struct JpegInfo {
    width: f32,
    height: f32,
    color_space: ColorSpace,
    bits_per_component: u8,
    invert_cmyk: bool,
    orientation: u8,
}

/// 解码图片并按 EXIF 方向自动旋转/翻转 (image 0.25 的 `decode()` 不会自动应用方向信息)
pub fn decode_oriented(bytes: &[u8]) -> Result<image::DynamicImage> {
    use image::ImageDecoder;
    let reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .context("无法识别图片格式")?;
    let mut decoder = reader.into_decoder().context("图片解码失败")?;
    let orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut img = image::DynamicImage::from_decoder(decoder).context("图片解码失败")?;
    img.apply_orientation(orientation);
    Ok(img)
}

/// 创建带有缩略图的图片条目 (针对磁盘文件和归档条目不常驻原始字节，仅保留缩略图以保证极低内存)
pub fn create_image_entry(
    id: usize,
    name: String,
    bytes: Vec<u8>,
    source_path: Option<PathBuf>,
    archive_source: Option<(Arc<PathBuf>, String)>,
) -> ImageEntry {
    let mut width = 800;
    let mut height = 1200;
    let mut thumb_rgb = None;
    let mut thumb_w = 0;
    let mut thumb_h = 0;

    // 生成轻量 UI 缩略图 (最大 100x140 像素以维持极低内存占用；按 EXIF 方向校正)
    // 优先完整解码以直接提取真实方向与尺寸；仅在解码失败时才回退至快速头解析 blob_size
    if let Ok(dyn_img) = decode_oriented(&bytes) {
        width = dyn_img.width();
        height = dyn_img.height();
        let thumb = dyn_img.thumbnail(100, 140);
        thumb_w = thumb.width() as usize;
        thumb_h = thumb.height() as usize;
        thumb_rgb = Some(Arc::from(thumb.into_rgb8().into_raw()));
    } else if let Ok(size) = imagesize::blob_size(&bytes) {
        width = size.width as u32;
        height = size.height as u32;
    }

    // 若图片来自本地磁盘文件或 ZIP/CBZ 归档，生成缩略图后立即释放原始大图字节，杜绝内存堆积
    let stored_bytes: Arc<[u8]> = if source_path.is_some() || archive_source.is_some() {
        Arc::from(&[][..])
    } else {
        Arc::from(bytes)
    };

    let sort_key = name.clone();
    ImageEntry {
        id,
        name,
        sort_key,
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

/// 解析并处理图片数据以供 PDF 组装 (支持原画无损直存：JPEG 零解码零拷贝直通 + 其余格式 Flate 像素无损；或按指定画质 10%~100% 压缩重编)
pub fn process_image_bytes(
    bytes: Vec<u8>,
    lossless_direct: bool,
    quality: u8,
) -> Result<ProcessedImage> {
    if lossless_direct {
        if is_jpeg(&bytes) {
            // EXIF 方向不再回退重编码：保持 DCT 数据原样直通，旋转/翻转交给 PDF 变换矩阵
            if let Ok(info) = parse_jpeg_header(&bytes) {
                return Ok(ProcessedImage {
                    width: info.width,
                    height: info.height,
                    color_space: info.color_space,
                    bits_per_component: info.bits_per_component,
                    filter: Filter::DctDecode,
                    stream_data: bytes,
                    invert_cmyk: info.invert_cmyk,
                    orientation: info.orientation,
                });
            }
        }

        return decode_and_compress_flate(&bytes);
    }

    decode_and_compress_with_quality(&bytes, quality)
}

#[allow(dead_code)]
pub fn is_png(data: &[u8]) -> bool {
    data.len() >= 8 && data.starts_with(b"\x89PNG\r\n\x1a\n")
}

fn is_jpeg(data: &[u8]) -> bool {
    data.len() >= 3 && data[0] == 0xFF && data[1] == 0xD8 && data[2] == 0xFF
}

/// 读取 JPEG EXIF APP1 段中的方向值 (Orientation, 1~8)；无效或缺失时返回 None
fn exif_orientation(exif_seg: &[u8]) -> Option<u8> {
    if exif_seg.len() < 14 || &exif_seg[0..6] != b"Exif\0\0" {
        return None;
    }
    let tiff = &exif_seg[6..];
    let is_le = match &tiff[0..2] {
        b"II" => true,
        b"MM" => false,
        _ => return None,
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
        return None;
    }
    let ifd_offset = read_u32(4)? as usize;
    let num_entries = read_u16(ifd_offset)? as usize;

    for i in 0..num_entries.min(64) {
        let Some(entry_offset) = ifd_offset.checked_add(2 + i * 12) else {
            break;
        };
        let tag = read_u16(entry_offset)?;
        if tag == 0x0112 {
            let field_type = read_u16(entry_offset + 2)?;
            let orientation = match field_type {
                3 => read_u16(entry_offset + 8)? as u32, // SHORT (u16)
                4 => read_u32(entry_offset + 8)?,        // LONG (u32)
                _ => read_u16(entry_offset + 8)? as u32,
            };
            return (1..=8).contains(&orientation).then_some(orientation as u8);
        }
    }
    None
}

/// 零拷贝解析 JPEG 头部信息 (尺寸、色彩空间、位深、CMYK 反相、EXIF 方向)
fn parse_jpeg_header(data: &[u8]) -> Result<JpegInfo> {
    let mut cursor = 2;
    let mut has_adobe_app14 = false;
    let mut orientation = 1u8;
    let mut sof_info: Option<(f32, f32, u8, u8)> = None;

    while cursor + 2 <= data.len() {
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

        // 遇到 SOS (开始扫描熵数据) 或 EOI (主图结束) 立即停止：
        // 既避免扫描正文压缩数据或误读尾部 MPF/UltraHDR 增益图的副图 SOF，又能完整捕获 SOS 前的所有 APP1/APP14 段
        if marker == 0xDA || marker == 0xD9 {
            break;
        }

        if marker == 0xD8 || (0xD0..=0xD7).contains(&marker) || marker == 0x00 || marker == 0x01 {
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

        // 记录 EXIF Orientation，交由 PDF 变换矩阵完成旋转/翻转 (保持 DCT 数据无损直通)
        if marker == 0xE1 {
            if let Some(o) = exif_orientation(seg) {
                orientation = o;
            }
        }

        // 检查 Adobe APP14 标记 (用于判断 CMYK JPEG 是否需要反相 Decode)
        if marker == 0xEE && seg.len() >= 12 && seg.starts_with(b"Adobe") {
            has_adobe_app14 = true;
        }

        // 非标准 DCT (无损/差分/算术编码 SOF) 不支持 PDF DCTDecode 直通，回退解码重封装
        if sof_info.is_none() && matches!(marker, 0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF) {
            anyhow::bail!("不支持非常规编码 JPEG 的无损直通");
        }

        let is_sof = matches!(marker, 0xC0..=0xC2);

        if is_sof && sof_info.is_none() && seg.len() >= 6 {
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
            sof_info = Some((width, height, precision, components));
        }

        cursor += length;
    }

    if let Some((width, height, precision, components)) = sof_info {
        // 仅 Adobe (Photoshop) 写出的 CMYK JPEG 以反相形式存储，需要 Decode [1 0 ...] 还原；
        // 不带 APP14 标记的 CMYK JPEG 按常规值存储，不能反相
        let (color_space, invert_cmyk) = match components {
            1 => (ColorSpace::DeviceGray, false),
            3 => (ColorSpace::DeviceRgb, false),
            4 => (ColorSpace::DeviceCmyk, has_adobe_app14),
            _ => anyhow::bail!("不支持的 JPEG 通道数: {}", components),
        };

        return Ok(JpegInfo {
            width,
            height,
            color_space,
            bits_per_component: precision,
            invert_cmyk,
            orientation,
        });
    }

    anyhow::bail!("无法在 JPEG 中解析到有效的 SOF Marker")
}

/// 将解码后的图像统一转换为无透明通道的 8-bit 灰度或 RGB 像素缓冲 (透明像素与纯白背景 255 混合)。
/// 若 RGB/RGBA 图像的所有像素均满足 R == G == B（常见于被存为 3 通道的黑白漫画），自动无损降为单通道 DeviceGray，体积缩减约 2/3。
fn flatten_to_white(dynamic_img: image::DynamicImage) -> (ColorSpace, Vec<u8>) {
    let width = dynamic_img.width() as usize;
    let height = dynamic_img.height() as usize;
    match dynamic_img.color() {
        image::ColorType::L8 | image::ColorType::L16 => {
            (ColorSpace::DeviceGray, dynamic_img.into_luma8().into_raw())
        }
        image::ColorType::La8 | image::ColorType::La16 => {
            let luma_a = dynamic_img.into_luma_alpha8();
            let mut luma = Vec::with_capacity(width * height);
            for pixel in luma_a.pixels() {
                let l = pixel[0] as u32;
                let a = pixel[1] as u32;
                let val = (l * a + 255 * (255 - a)) / 255;
                luma.push(val as u8);
            }
            (ColorSpace::DeviceGray, luma)
        }
        image::ColorType::Rgba8 | image::ColorType::Rgba16 => {
            let rgba = dynamic_img.into_rgba8();
            let mut rgb = Vec::with_capacity(width * height * 3);
            let mut all_gray = true;
            for pixel in rgba.pixels() {
                let a = pixel[3] as u32;
                let (r, g, b) = if a == 255 {
                    (pixel[0], pixel[1], pixel[2])
                } else if a == 0 {
                    (255, 255, 255)
                } else {
                    let r = ((pixel[0] as u32 * a + 255 * (255 - a)) / 255) as u8;
                    let g = ((pixel[1] as u32 * a + 255 * (255 - a)) / 255) as u8;
                    let b = ((pixel[2] as u32 * a + 255 * (255 - a)) / 255) as u8;
                    (r, g, b)
                };
                if all_gray && (r != g || r != b) {
                    all_gray = false;
                }
                rgb.push(r);
                rgb.push(g);
                rgb.push(b);
            }
            if all_gray {
                let gray: Vec<u8> = rgb.into_iter().step_by(3).collect();
                (ColorSpace::DeviceGray, gray)
            } else {
                (ColorSpace::DeviceRgb, rgb)
            }
        }
        _ => {
            let rgb = dynamic_img.into_rgb8().into_raw();
            let all_gray = rgb
                .as_chunks::<3>()
                .0
                .iter()
                .all(|&[r, g, b]| r == g && r == b);
            if all_gray {
                let gray: Vec<u8> = rgb.into_iter().step_by(3).collect();
                (ColorSpace::DeviceGray, gray)
            } else {
                (ColorSpace::DeviceRgb, rgb)
            }
        }
    }
}

fn decode_and_compress_with_quality(bytes: &[u8], quality: u8) -> Result<ProcessedImage> {
    let dynamic_img = decode_oriented(bytes)?;

    let width = dynamic_img.width() as f32;
    let height = dynamic_img.height() as f32;
    let q = quality.clamp(10, 100);
    let mut jpeg_buf = Vec::new();

    let (color_space, raw_pixels) = flatten_to_white(dynamic_img);
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg_buf, q);
    let ext_color = match color_space {
        ColorSpace::DeviceGray => image::ExtendedColorType::L8,
        _ => image::ExtendedColorType::Rgb8,
    };
    encoder.encode(&raw_pixels, width as u32, height as u32, ext_color)?;

    Ok(ProcessedImage {
        width,
        height,
        color_space,
        bits_per_component: 8,
        filter: Filter::DctDecode,
        stream_data: jpeg_buf,
        invert_cmyk: false,
        orientation: 1,
    })
}

pub fn decode_and_compress_flate(bytes: &[u8]) -> Result<ProcessedImage> {
    let dynamic_img = decode_oriented(bytes)?;

    let width = dynamic_img.width() as f32;
    let height = dynamic_img.height() as f32;
    let (color_space, raw_pixels) = flatten_to_white(dynamic_img);

    let stream_data = miniz_oxide::deflate::compress_to_vec_zlib(&raw_pixels, 6);

    Ok(ProcessedImage {
        width,
        height,
        color_space,
        bits_per_component: 8,
        filter: Filter::FlateDecode,
        stream_data,
        invert_cmyk: false,
        orientation: 1,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_png_detection_and_flate_compression() {
        let mut png_bytes = Vec::new();
        let img = image::RgbImage::from_fn(10, 10, |x, _| image::Rgb([x as u8 * 20, 100, 200]));
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

    #[test]
    fn test_all_gray_rgb_losslessly_compacts_to_device_gray() {
        let mut png_bytes = Vec::new();
        let img = image::RgbImage::from_fn(12, 12, |x, y| {
            let v = ((x + y) * 10) as u8;
            image::Rgb([v, v, v])
        });
        img.write_to(
            &mut std::io::Cursor::new(&mut png_bytes),
            image::ImageFormat::Png,
        )
        .unwrap();

        let processed = decode_and_compress_flate(&png_bytes).unwrap();
        assert_eq!(processed.color_space, ColorSpace::DeviceGray);
    }

    /// 生成 w×h 的 RGB JPEG，并可选在 SOI 之后插入带 Orientation 标签的 EXIF APP1 段
    fn make_jpeg(w: u32, h: u32, orientation: Option<u16>) -> Vec<u8> {
        let mut jpeg = Vec::new();
        let img =
            image::RgbImage::from_fn(w, h, |x, y| image::Rgb([(x * 5) as u8, (y * 5) as u8, 128]));
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 90)
            .encode(img.as_raw(), w, h, image::ExtendedColorType::Rgb8)
            .unwrap();

        let Some(o) = orientation else { return jpeg };
        let mut payload = Vec::new();
        payload.extend_from_slice(b"Exif\0\0");
        payload.extend_from_slice(b"MM\0\x2a\0\0\0\x08"); // 大端 TIFF 头，IFD0 偏移 8
        payload.extend_from_slice(&1u16.to_be_bytes()); // 1 个条目
        payload.extend_from_slice(&0x0112u16.to_be_bytes()); // Orientation
        payload.extend_from_slice(&3u16.to_be_bytes()); // SHORT
        payload.extend_from_slice(&1u32.to_be_bytes()); // count = 1
        payload.extend_from_slice(&o.to_be_bytes());
        payload.extend_from_slice(&[0, 0]);
        payload.extend_from_slice(&0u32.to_be_bytes()); // 无下一个 IFD

        let mut out = jpeg[..2].to_vec();
        out.extend_from_slice(&[0xFF, 0xE1]);
        out.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
        out.extend_from_slice(&payload);
        out.extend_from_slice(&jpeg[2..]);
        out
    }

    #[test]
    fn test_jpeg_passthrough_keeps_exif_orientation() {
        let bytes = make_jpeg(40, 20, Some(6));
        let processed = process_image_bytes(bytes.clone(), true, 100).unwrap();
        assert_eq!(processed.filter, Filter::DctDecode);
        assert_eq!(
            processed.stream_data, bytes,
            "无损模式必须原样直通 DCT 数据"
        );
        assert_eq!(processed.orientation, 6);
        assert_eq!((processed.width, processed.height), (40.0, 20.0));
        assert_eq!(processed.display_size(), (20.0, 40.0));
    }

    #[test]
    fn test_decode_applies_exif_orientation() {
        let bytes = make_jpeg(40, 20, Some(6));
        let img = decode_oriented(&bytes).unwrap();
        assert_eq!((img.width(), img.height()), (20, 40));

        // 有损重编路径：像素已旋转，PDF 中无需再变换
        let processed = process_image_bytes(bytes, false, 80).unwrap();
        assert_eq!(processed.orientation, 1);
        assert_eq!((processed.width, processed.height), (20.0, 40.0));

        // 缩略图与记录的尺寸同样按方向校正
        let entry = create_image_entry(1, "a.jpg".into(), make_jpeg(40, 20, Some(8)), None, None);
        assert_eq!((entry.width, entry.height), (20, 40));
        assert!(entry.thumb_height > entry.thumb_width);
    }

    #[test]
    fn test_jpeg_without_exif_is_upright() {
        let processed = process_image_bytes(make_jpeg(40, 20, None), true, 100).unwrap();
        assert_eq!(processed.orientation, 1);
        assert_eq!(processed.display_size(), (40.0, 20.0));
    }

    /// 构造仅含 (可选 APP14) + 4 通道 SOF0 的最小 JPEG 头
    fn cmyk_header(with_adobe: bool) -> Vec<u8> {
        let mut d = vec![0xFF, 0xD8];
        if with_adobe {
            d.extend_from_slice(&[0xFF, 0xEE, 0x00, 0x0E]);
            d.extend_from_slice(b"Adobe");
            d.extend_from_slice(&[0, 100, 0, 0, 0, 0, 2]);
        }
        d.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x14, 8, 0, 16, 0, 16, 4]);
        for id in 1..=4u8 {
            d.extend_from_slice(&[id, 0x11, 0]);
        }
        d.extend_from_slice(&[0xFF, 0xD9]);
        d
    }

    #[test]
    fn test_cmyk_invert_only_with_adobe_marker() {
        let plain = parse_jpeg_header(&cmyk_header(false)).unwrap();
        assert_eq!(plain.color_space, ColorSpace::DeviceCmyk);
        assert!(!plain.invert_cmyk, "无 Adobe APP14 的 CMYK 不应反相");

        let adobe = parse_jpeg_header(&cmyk_header(true)).unwrap();
        assert_eq!(adobe.color_space, ColorSpace::DeviceCmyk);
        assert!(adobe.invert_cmyk, "Adobe CMYK JPEG 需要反相 Decode");
    }
}
