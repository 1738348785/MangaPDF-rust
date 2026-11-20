use crate::model::{ArchiveEntryRef, ImageEntry};
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
    /// FlateDecode 流是否采用了 PNG 行预测滤波 (/DecodeParms << /Predictor 15 ... >>)
    pub flate_predictor: bool,
    /// DCTDecode 可选颜色变换提示 (/DecodeParms << /ColorTransform 0|1 >>)，用于兼容原始 RGB / YCCK JPEG
    pub dct_color_transform: Option<bool>,
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
    dct_color_transform: Option<bool>,
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
    archive_source: Option<ArchiveEntryRef>,
) -> ImageEntry {
    // 默认 0×0 表示「尺寸未知」：解码与头解析双双失败时，写死一个 800×1200 会骗用户以为图片正常，
    // 卡片上再显示「尺寸未知」也自相矛盾。真正能头解析的图片仍会拿到真实宽高。
    let mut width = 0;
    let mut height = 0;
    let mut thumb_rgb = None;
    let mut thumb_w = 0;
    let mut thumb_h = 0;
    let mut thumb_error: Option<String> = None;

    // 生成轻量 UI 缩略图 (最大 100x140 像素以维持极低内存占用；按 EXIF 方向校正，保留 RGBA 透明通道)
    // 优先完整解码以直接提取真实方向与尺寸；仅在解码失败时才回退至快速头解析 blob_size
    match decode_oriented(&bytes) {
        Ok(dyn_img) => {
            width = dyn_img.width();
            height = dyn_img.height();
            let thumb = dyn_img.thumbnail(100, 140);
            thumb_w = thumb.width() as usize;
            thumb_h = thumb.height() as usize;
            thumb_rgb = Some(Arc::from(thumb.into_rgba8().into_raw()));
        }
        Err(e) => {
            // 记录真实失败原因，供网格占位符的悬停提示展示；
            // 同时尽量用头解析补回真实宽高（此类图片往往仍能无损直通写入 PDF）
            if let Ok(size) = imagesize::blob_size(&bytes) {
                width = size.width as u32;
                height = size.height as u32;
            }
            thumb_error = Some(format!("{:#}", e));
        }
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
        thumb_error,
    }
}

/// 解析并处理图片数据以供 PDF 组装 (支持原画无损直存：JPEG / 原生灰度 PNG 零解码零拷贝直通 + 其余格式 PNG 自适应预测 Flate 无损；或按指定画质 10%~100% 压缩重编)
pub fn process_image_bytes(
    bytes: Vec<u8>,
    lossless_direct: bool,
    quality: u8,
) -> Result<ProcessedImage> {
    // JPEG 结构校验对「无损直通」与「有损重编」两种模式都生效：
    // 截断/残缺的 JPEG 解码器往往仍会宽容补齐缺失行，若不在此拦截，
    // 残缺图就会以 Flate 形式静默写入 PDF，产出白条或半张图。
    if is_jpeg(&bytes) {
        match parse_jpeg_header(&bytes) {
            Ok(info) => {
                if lossless_direct {
                    // EXIF 方向不再回退重编码：保持 DCT 数据原样直通，旋转/翻转交给 PDF 变换矩阵
                    return Ok(ProcessedImage {
                        width: info.width,
                        height: info.height,
                        color_space: info.color_space,
                        bits_per_component: info.bits_per_component,
                        filter: Filter::DctDecode,
                        flate_predictor: false,
                        dct_color_transform: info.dct_color_transform,
                        stream_data: bytes,
                        invert_cmyk: info.invert_cmyk,
                        orientation: info.orientation,
                    });
                }
            }
            Err(JpegHeaderError::Corrupt(reason)) => {
                anyhow::bail!("JPEG 文件不完整或已损坏，已跳过该页：{}", reason);
            }
            // 结构完整、仅编码方式无法直通：保持原有行为，回退到解码重编
            Err(JpegHeaderError::Unsupported(_)) => {}
        }
    }

    if lossless_direct {
        if is_png(&bytes) {
            // 原生非隔行灰度 PNG (1/2/4/8/16-bit，无透明键 tRNS 与 EXIF 旋转) 直接复用原始 IDAT 压缩流，
            // 配合 PDF FlateDecode + PNG Optimum Predictor (15) 实现零解码零重压缩直通
            if let Some(processed) = try_passthrough_grayscale_png(&bytes) {
                return Ok(processed);
            }
        }

        return decode_and_compress_flate(&bytes);
    }

    decode_and_compress_with_quality(&bytes, quality)
}

pub fn is_png(data: &[u8]) -> bool {
    data.len() >= 8 && data.starts_with(b"\x89PNG\r\n\x1a\n")
}

/// 尝试对原生非隔行灰度 PNG（color_type == 0，位深 1/2/4/8/16，无 tRNS 透明色键、无 eXIf 方向旋转）
/// 直接提取拼接原始 IDAT zlib 数据流，避免解码再重压缩的开销并完整保留原始位深与压缩率。
fn try_passthrough_grayscale_png(data: &[u8]) -> Option<ProcessedImage> {
    if !is_png(data) {
        return None;
    }

    let mut cursor = 8usize;
    let mut ihdr: Option<(u32, u32, u8)> = None;
    let mut idat_buf = Vec::new();
    let mut seen_idat = false;
    let mut idat_closed = false;
    let mut saw_iend = false;

    while cursor + 12 <= data.len() {
        let len = u32::from_be_bytes([
            data[cursor],
            data[cursor + 1],
            data[cursor + 2],
            data[cursor + 3],
        ]) as usize;
        let chunk_type = &data[cursor + 4..cursor + 8];
        let data_start = cursor + 8;
        let data_end = data_start.checked_add(len)?;
        let next_cursor = data_end.checked_add(4)?; // 跳过 4 字节 CRC
        if next_cursor > data.len() {
            return None;
        }
        let chunk_data = &data[data_start..data_end];

        // 逐块校验 CRC32（覆盖 chunk 类型 + 数据）：IDAT 单比特损坏或部分写入残缺时
        // 必须在这里拦下，否则坏 PNG 会带着 Predictor 原样落进 PDF，阅读器解出花屏/白页。
        let stored_crc = u32::from_be_bytes([
            data[data_end],
            data[data_end + 1],
            data[data_end + 2],
            data[data_end + 3],
        ]);
        let mut hasher = crc32fast::Hasher::new();
        hasher.update(chunk_type);
        hasher.update(chunk_data);
        if hasher.finalize() != stored_crc {
            return None;
        }

        if ihdr.is_none() {
            if chunk_type != b"IHDR" || len != 13 {
                return None;
            }
            let width = u32::from_be_bytes([
                chunk_data[0],
                chunk_data[1],
                chunk_data[2],
                chunk_data[3],
            ]);
            let height = u32::from_be_bytes([
                chunk_data[4],
                chunk_data[5],
                chunk_data[6],
                chunk_data[7],
            ]);
            let bit_depth = chunk_data[8];
            let color_type = chunk_data[9];
            let compression = chunk_data[10];
            let filter_method = chunk_data[11];
            let interlace = chunk_data[12];

            if width == 0
                || height == 0
                || width > i32::MAX as u32
                || height > i32::MAX as u32
                || color_type != 0
                // 16-bit 灰度同样可直通：PDF 支持 /BitsPerComponent 16，
                // 且 PNG 与 PDF 的多字节样本均为大端，滤波器 bpp 亦同为 2 字节
                || !matches!(bit_depth, 1 | 2 | 4 | 8 | 16)
                || compression != 0
                || filter_method != 0
                || interlace != 0
            {
                return None;
            }
            ihdr = Some((width, height, bit_depth));
        } else {
            match chunk_type {
                // 含透明色键或 EXIF 方向信息时回退至解码路径（自动白底混合/方向校正）
                b"tRNS" | b"eXIf" => return None,
                b"IDAT" => {
                    if idat_closed {
                        return None;
                    }
                    seen_idat = true;
                    idat_buf.extend_from_slice(chunk_data);
                }
                b"IEND" => {
                    if len != 0 {
                        return None;
                    }
                    saw_iend = true;
                    break;
                }
                _ => {
                    if seen_idat {
                        idat_closed = true;
                    }
                }
            }
        }

        cursor = next_cursor;
    }

    let (width, height, bit_depth) = ihdr?;
    if !saw_iend || idat_buf.len() < 6 {
        return None;
    }

    // 校验 zlib 头部（Deflate 方法、窗口 <= 32KB、FCHECK 校验通过、无预设字典 FDICT）
    let cmf = idat_buf[0];
    let flg = idat_buf[1];
    if (cmf & 0x0F) != 8
        || (cmf >> 4) > 7
        || !((cmf as u16) << 8 | flg as u16).is_multiple_of(31)
        || (flg & 0x20) != 0
    {
        return None;
    }

    Some(ProcessedImage {
        width: width as f32,
        height: height as f32,
        color_space: ColorSpace::DeviceGray,
        bits_per_component: bit_depth,
        filter: Filter::FlateDecode,
        flate_predictor: true,
        dct_color_transform: None,
        stream_data: idat_buf,
        invert_cmyk: false,
        orientation: 1,
    })
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
        let end = offset.checked_add(2)?;
        let b = tiff.get(offset..end)?;
        Some(if is_le {
            u16::from_le_bytes([b[0], b[1]])
        } else {
            u16::from_be_bytes([b[0], b[1]])
        })
    };
    let read_u32 = |offset: usize| -> Option<u32> {
        let end = offset.checked_add(4)?;
        let b = tiff.get(offset..end)?;
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
            let field_type = read_u16(entry_offset.checked_add(2)?)?;
            let val_offset = entry_offset.checked_add(8)?;
            let orientation = match field_type {
                3 => read_u16(val_offset)? as u32, // SHORT (u16)
                4 => read_u32(val_offset)?,        // LONG (u32)
                _ => read_u16(val_offset)? as u32,
            };
            return (1..=8).contains(&orientation).then_some(orientation as u8);
        }
    }
    None
}

/// JPEG 头解析失败的两种性质，决定调用方是「回退解码重编」还是「直接拒绝」：
/// - `Unsupported`：结构完整，仅编码方式无法无损直通（可安全回退解码重编）；
/// - `Corrupt`：结构残缺/截断。解码器往往仍会「宽容」补齐缺失行，必须拒绝，
///   否则残缺图会以 Flate 形式静默写入 PDF，产出白条或半张图。
#[derive(Debug, Clone, Copy)]
enum JpegHeaderError {
    Unsupported(&'static str),
    Corrupt(&'static str),
}

impl std::fmt::Display for JpegHeaderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported(msg) | Self::Corrupt(msg) => f.write_str(msg),
        }
    }
}

/// 零拷贝解析 JPEG 头部信息并校验结构完整性 (尺寸、色彩空间、位深、CMYK 反相、ColorTransform、EXIF 方向、SOS/EOI 完整性)
fn parse_jpeg_header(data: &[u8]) -> std::result::Result<JpegInfo, JpegHeaderError> {
    let mut cursor = 2;
    let mut adobe_transform: Option<u8> = None;
    let mut orientation = 1u8;
    let mut sof_info: Option<(f32, f32, u8, u8, bool)> = None;
    let mut sos_scan_start: Option<usize> = None;

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

        // 遇到 EOI (主图提前结束，缺少 SOS) 立即停止
        if marker == 0xD9 {
            break;
        }

        // 遇到 SOS (开始扫描熵数据) 校验其段长与正文负载后立即停止：
        // 既避免扫描正文压缩数据或误读尾部 MPF/UltraHDR 增益图的副图 SOF，又能完整捕获 SOS 前的所有 APP1/APP14 段
        if marker == 0xDA {
            if cursor + 2 <= data.len() {
                let sos_len = u16::from_be_bytes([data[cursor], data[cursor + 1]]) as usize;
                let scan_start = cursor + sos_len;
                if sos_len >= 6 && scan_start < data.len() {
                    sos_scan_start = Some(scan_start);
                }
            }
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

        // 检查 Adobe APP14 标记及 ColorTransform 字节 (0 = 原始 RGB/CMYK, 1 = YCbCr, 2 = YCCK)
        if marker == 0xEE && seg.len() >= 12 && seg.starts_with(b"Adobe") {
            adobe_transform = Some(seg[11]);
        }

        // 非标准 DCT (无损/差分/算术编码 SOF) 不支持 PDF DCTDecode 直通，回退解码重封装
        if sof_info.is_none() && matches!(marker, 0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF) {
            return Err(JpegHeaderError::Unsupported(
                "不支持非常规编码 JPEG 的无损直通",
            ));
        }

        let is_sof = matches!(marker, 0xC0..=0xC2);

        if is_sof && sof_info.is_none() && seg.len() >= 6 {
            let precision = seg[0];
            if precision != 8 {
                return Err(JpegHeaderError::Unsupported(
                    "不支持非 8-bit 精度的 JPEG 直通",
                ));
            }
            let height = u16::from_be_bytes([seg[1], seg[2]]) as f32;
            let width = u16::from_be_bytes([seg[3], seg[4]]) as f32;
            let components = seg[5];

            if width <= 0.0 || height <= 0.0 {
                return Err(JpegHeaderError::Unsupported("无效的 JPEG 宽高尺寸"));
            }
            let is_ascii_rgb =
                components == 3 && seg.len() >= 15 && [seg[6], seg[9], seg[12]] == *b"RGB";
            sof_info = Some((width, height, precision, components, is_ascii_rgb));
        }

        cursor += length;
    }

    let Some(scan_start) = sos_scan_start else {
        return Err(JpegHeaderError::Corrupt(
            "JPEG 缺少有效的扫描数据 (SOS)，文件结构不完整",
        ));
    };

    // 校验 SOS 扫描数据之后存在合法的 EOI (0xFF 0xD9) 结束标记，防止下载截断的残缺 JPG 静默写入 PDF；
    // 从尾部反向查找，正常 JPEG 在 1 次比较内完成，同时兼容尾部带零填充或 MPF/UltraHDR 附加数据的文件
    let has_eoi = data[scan_start..]
        .windows(2)
        .rev()
        .any(|w| w == [0xFF, 0xD9]);
    if !has_eoi {
        return Err(JpegHeaderError::Corrupt(
            "JPEG 缺少结束标记 (EOI)，文件可能已截断损坏",
        ));
    }

    if let Some((width, height, precision, components, is_ascii_rgb)) = sof_info {
        // 仅 Adobe (Photoshop) 写出的 CMYK/YCCK JPEG 以反相形式存储，需要 Decode [1 0 ...] 还原；
        // 不带 APP14 标记的 CMYK JPEG 按常规值存储，不能反相
        let (color_space, invert_cmyk, dct_color_transform) = match components {
            1 => (ColorSpace::DeviceGray, false, None),
            3 => {
                let ct = if adobe_transform == Some(0)
                    || (adobe_transform.is_none() && is_ascii_rgb)
                {
                    Some(false) // 原始 RGB JPEG，禁用 YCbCr 逆变换
                } else {
                    None
                };
                (ColorSpace::DeviceRgb, false, ct)
            }
            4 => {
                let ct = match adobe_transform {
                    Some(2) => Some(true),  // Adobe YCCK
                    Some(0) => Some(false), // Adobe CMYK
                    _ => None,
                };
                (ColorSpace::DeviceCmyk, adobe_transform.is_some(), ct)
            }
            _ => return Err(JpegHeaderError::Unsupported("不支持的 JPEG 通道数")),
        };

        return Ok(JpegInfo {
            width,
            height,
            color_space,
            bits_per_component: precision,
            invert_cmyk,
            dct_color_transform,
            orientation,
        });
    }

    Err(JpegHeaderError::Corrupt(
        "无法在 JPEG 中解析到有效的 SOF 标记，文件结构不完整",
    ))
}

/// 将解码后的图像统一转换为无透明通道的灰度或 RGB 像素缓冲 (透明像素与纯白背景混合)。
/// 若 RGB/RGBA 图像的所有像素均满足 R == G == B（常见于被存为 3 通道的黑白漫画），自动无损降为单通道 DeviceGray，体积缩减约 2/3。
///
/// 返回 `(色彩空间, 大端像素字节, 每分量位深)`。16-bit 源会保留原生位深（PDF 支持
/// `/BitsPerComponent 16`），避免「无损直存」被静默降为 8-bit。
fn flatten_to_white(dynamic_img: image::DynamicImage) -> (ColorSpace, Vec<u8>, u8) {
    let width = dynamic_img.width() as usize;
    let height = dynamic_img.height() as usize;

    if matches!(
        dynamic_img.color(),
        image::ColorType::L16 | image::ColorType::La16 | image::ColorType::Rgb16 | image::ColorType::Rgba16
    ) {
        return flatten_to_white_16(dynamic_img);
    }

    match dynamic_img.color() {
        image::ColorType::L8 => (
            ColorSpace::DeviceGray,
            dynamic_img.into_luma8().into_raw(),
            8,
        ),
        image::ColorType::La8 => {
            let luma_a = dynamic_img.into_luma_alpha8();
            let mut luma = Vec::with_capacity(width * height);
            for pixel in luma_a.pixels() {
                let l = pixel[0] as u32;
                let a = pixel[1] as u32;
                let val = (l * a + 255 * (255 - a)) / 255;
                luma.push(val as u8);
            }
            (ColorSpace::DeviceGray, luma, 8)
        }
        image::ColorType::Rgba8 => {
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
                (ColorSpace::DeviceGray, gray, 8)
            } else {
                (ColorSpace::DeviceRgb, rgb, 8)
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
                (ColorSpace::DeviceGray, gray, 8)
            } else {
                (ColorSpace::DeviceRgb, rgb, 8)
            }
        }
    }
}

/// 将 u16 样本序列按**大端**字节序展开（PNG 与 PDF 的多字节样本均要求大端）。
fn widen_u16_be(samples: &[u16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len() * 2);
    for &v in samples {
        out.extend_from_slice(&v.to_be_bytes());
    }
    out
}

/// 16-bit 源的统一转换：白底合成后保留 16-bit 精度，返回 (色彩空间, 大端字节, 16)。
fn flatten_to_white_16(dynamic_img: image::DynamicImage) -> (ColorSpace, Vec<u8>, u8) {
    let width = dynamic_img.width() as usize;
    let height = dynamic_img.height() as usize;

    match dynamic_img.color() {
        image::ColorType::L16 => (
            ColorSpace::DeviceGray,
            widen_u16_be(dynamic_img.into_luma16().as_raw()),
            16,
        ),
        image::ColorType::La16 => {
            let luma_a = dynamic_img.into_luma_alpha16();
            let mut luma = Vec::with_capacity(width * height);
            for pixel in luma_a.pixels() {
                let l = pixel.0[0] as u64;
                let a = pixel.0[1] as u64;
                luma.push(((l * a + 65535 * (65535 - a)) / 65535) as u16);
            }
            (ColorSpace::DeviceGray, widen_u16_be(&luma), 16)
        }
        image::ColorType::Rgb16 => {
            // 无透明通道的 16-bit RGB 直接读取，避免多分配一份 4 通道 Rgba16 缓冲
            let rgb = dynamic_img.into_rgb16().into_raw();
            let all_gray = rgb
                .as_chunks::<3>()
                .0
                .iter()
                .all(|&[r, g, b]| r == g && r == b);
            if all_gray {
                let gray: Vec<u16> = rgb.into_iter().step_by(3).collect();
                (ColorSpace::DeviceGray, widen_u16_be(&gray), 16)
            } else {
                (ColorSpace::DeviceRgb, widen_u16_be(&rgb), 16)
            }
        }
        _ => {
            // Rgba16：先与纯白背景合成透明通道，再判断能否无损降为单通道
            let rgba = dynamic_img.into_rgba16();
            let mut rgb = Vec::with_capacity(width * height * 3);
            let mut all_gray = true;
            for pixel in rgba.pixels() {
                let a = pixel.0[3] as u64;
                let blend =
                    |c: u16| (((c as u64) * a + 65535 * (65535 - a)) / 65535) as u16;
                let (r, g, b) = if a == 65535 {
                    (pixel.0[0], pixel.0[1], pixel.0[2])
                } else if a == 0 {
                    (65535, 65535, 65535)
                } else {
                    (blend(pixel.0[0]), blend(pixel.0[1]), blend(pixel.0[2]))
                };
                if all_gray && (r != g || r != b) {
                    all_gray = false;
                }
                rgb.push(r);
                rgb.push(g);
                rgb.push(b);
            }
            if all_gray {
                let gray: Vec<u16> = rgb.into_iter().step_by(3).collect();
                (ColorSpace::DeviceGray, widen_u16_be(&gray), 16)
            } else {
                (ColorSpace::DeviceRgb, widen_u16_be(&rgb), 16)
            }
        }
    }
}

/// 把 16-bit 大端像素降采样为 8-bit（每个样本取高字节），用于必须走 8-bit 编码器的有损重编路径。
fn narrow_to_8bit(raw: Vec<u8>, bits_per_component: u8) -> Vec<u8> {
    if bits_per_component == 8 {
        raw
    } else {
        raw.into_iter().step_by(2).collect()
    }
}

fn decode_and_compress_with_quality(bytes: &[u8], quality: u8) -> Result<ProcessedImage> {
    let dynamic_img = decode_oriented(bytes)?;

    let width = dynamic_img.width() as f32;
    let height = dynamic_img.height() as f32;
    let q = quality.clamp(10, 100);
    let mut jpeg_buf = Vec::new();

    let (color_space, raw_pixels, bits_per_component) = flatten_to_white(dynamic_img);
    // 有损重编走 8-bit JPEG 编码器：16-bit 源在此降采样为 8-bit 属于「有损模式」的预期行为
    let raw_pixels = narrow_to_8bit(raw_pixels, bits_per_component);
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
        flate_predictor: false,
        dct_color_transform: None,
        stream_data: jpeg_buf,
        invert_cmyk: false,
        orientation: 1,
    })
}

#[inline(always)]
fn paeth_predictor(a: u8, b: u8, c: u8) -> u8 {
    let a = a as i32;
    let b = b as i32;
    let c = c as i32;
    let p = a + b - c;
    let pa = (p - a).abs();
    let pb = (p - b).abs();
    let pc = (p - c).abs();
    if pa <= pb && pa <= pc {
        a as u8
    } else if pb <= pc {
        b as u8
    } else {
        c as u8
    }
}

/// 按 PNG 规范（RFC 2083）逐行自适应选择最佳滤波器（None/Sub/Up/Average/Paeth），
/// 配合 PDF `/FlateDecode` + `/DecodeParms << /Predictor 15 ... >>` 显著压缩无损漫画/渐变图体积。
fn apply_png_adaptive_filter(
    raw_pixels: &[u8],
    width: usize,
    height: usize,
    channels: usize,
    bytes_per_sample: usize,
) -> Vec<u8> {
    let bpp = channels * bytes_per_sample;
    let stride = width * bpp;
    debug_assert_eq!(raw_pixels.len(), stride * height);

    let mut out = Vec::with_capacity(height * (stride + 1));
    let zero_row = vec![0u8; stride];
    let mut sub_buf = vec![0u8; stride];
    let mut up_buf = vec![0u8; stride];
    let mut avg_buf = vec![0u8; stride];
    let mut paeth_buf = vec![0u8; stride];

    for y in 0..height {
        let cur = &raw_pixels[y * stride..(y + 1) * stride];
        let prev = if y == 0 {
            &zero_row[..]
        } else {
            &raw_pixels[(y - 1) * stride..y * stride]
        };

        // 0: None
        let mut best_type = 0u8;
        let mut best_score: usize = cur
            .iter()
            .map(|&v| (v as i8).unsigned_abs() as usize)
            .sum();

        // 1: Sub
        {
            let mut score = 0usize;
            for i in 0..stride {
                let left = if i >= bpp { cur[i - bpp] } else { 0 };
                let v = cur[i].wrapping_sub(left);
                sub_buf[i] = v;
                score += (v as i8).unsigned_abs() as usize;
                if score >= best_score {
                    break;
                }
            }
            if score < best_score {
                best_score = score;
                best_type = 1;
            }
        }

        // 2: Up (首行 prev 全 0，与 None 等价，跳过)
        if y > 0 {
            let mut score = 0usize;
            for i in 0..stride {
                let v = cur[i].wrapping_sub(prev[i]);
                up_buf[i] = v;
                score += (v as i8).unsigned_abs() as usize;
                if score >= best_score {
                    break;
                }
            }
            if score < best_score {
                best_score = score;
                best_type = 2;
            }
        }

        // 3: Average
        {
            let mut score = 0usize;
            for i in 0..stride {
                let left = if i >= bpp { cur[i - bpp] } else { 0 };
                let up = prev[i];
                let pred = ((left as u16 + up as u16) >> 1) as u8;
                let v = cur[i].wrapping_sub(pred);
                avg_buf[i] = v;
                score += (v as i8).unsigned_abs() as usize;
                if score >= best_score {
                    break;
                }
            }
            if score < best_score {
                best_score = score;
                best_type = 3;
            }
        }

        // 4: Paeth (首行 prev 全 0，Paeth(a,0,0) == a 与 Sub 等价，跳过)
        if y > 0 {
            let mut score = 0usize;
            for i in 0..stride {
                let left = if i >= bpp { cur[i - bpp] } else { 0 };
                let up = prev[i];
                let up_left = if i >= bpp { prev[i - bpp] } else { 0 };
                let v = cur[i].wrapping_sub(paeth_predictor(left, up, up_left));
                paeth_buf[i] = v;
                score += (v as i8).unsigned_abs() as usize;
                if score >= best_score {
                    break;
                }
            }
            if score < best_score {
                best_type = 4;
            }
        }

        out.push(best_type);
        match best_type {
            0 => out.extend_from_slice(cur),
            1 => out.extend_from_slice(&sub_buf),
            2 => out.extend_from_slice(&up_buf),
            3 => out.extend_from_slice(&avg_buf),
            _ => out.extend_from_slice(&paeth_buf),
        }
    }

    out
}

pub fn decode_and_compress_flate(bytes: &[u8]) -> Result<ProcessedImage> {
    let dynamic_img = decode_oriented(bytes)?;

    let w_usize = dynamic_img.width() as usize;
    let h_usize = dynamic_img.height() as usize;
    let width = w_usize as f32;
    let height = h_usize as f32;
    let (color_space, raw_pixels, bits_per_component) = flatten_to_white(dynamic_img);
    let channels = match color_space {
        ColorSpace::DeviceGray => 1,
        ColorSpace::DeviceRgb => 3,
        ColorSpace::DeviceCmyk => 4,
    };
    let bytes_per_sample = if bits_per_component == 16 { 2 } else { 1 };

    let filtered = apply_png_adaptive_filter(
        &raw_pixels,
        w_usize,
        h_usize,
        channels,
        bytes_per_sample,
    );
    // 提前释放原始像素缓冲，避免多核并发压缩大 PNG 时同时驻留 raw_pixels 与 filtered
    drop(raw_pixels);
    let stream_data = miniz_oxide::deflate::compress_to_vec_zlib(&filtered, 6);

    Ok(ProcessedImage {
        width,
        height,
        color_space,
        bits_per_component,
        filter: Filter::FlateDecode,
        flate_predictor: true,
        dct_color_transform: None,
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

    /// 构造含 (可选 APP14) + 4 通道 SOF0 + 最小 SOS 负载 + EOI 的结构完整 JPEG
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
        // 最小 SOS 头 (length = 14) + 1 字节扫描负载 + EOI
        d.extend_from_slice(&[
            0xFF, 0xDA, 0x00, 0x0E, 4, 1, 0, 2, 0, 3, 0, 4, 0, 0, 63, 0, 0x00, 0xFF, 0xD9,
        ]);
        d
    }

    #[test]
    fn test_cmyk_invert_only_with_adobe_marker() {
        let plain = parse_jpeg_header(&cmyk_header(false)).unwrap();
        assert_eq!(plain.color_space, ColorSpace::DeviceCmyk);
        assert!(!plain.invert_cmyk, "无 Adobe APP14 的 CMYK 不应反相");
        assert_eq!(plain.dct_color_transform, None);

        let adobe = parse_jpeg_header(&cmyk_header(true)).unwrap();
        assert_eq!(adobe.color_space, ColorSpace::DeviceCmyk);
        assert!(adobe.invert_cmyk, "Adobe CMYK JPEG 需要反相 Decode");
        assert_eq!(adobe.dct_color_transform, Some(true));
    }

    #[test]
    fn test_truncated_jpeg_without_sos_or_eoi_rejected_from_passthrough() {
        // 1. 仅有 SOF0、无 SOS 扫描段
        let mut no_sos = vec![0xFF, 0xD8, 0xFF, 0xC0, 0x00, 0x11, 8, 0, 16, 0, 16, 3];
        for id in 1..=3u8 {
            no_sos.extend_from_slice(&[id, 0x11, 0]);
        }
        no_sos.extend_from_slice(&[0xFF, 0xD9]);
        assert!(
            parse_jpeg_header(&no_sos).is_err(),
            "缺少 SOS 扫描段的残缺 JPEG 必须拒绝直通"
        );

        // 2. 合法 JPEG 被截断，缺少结尾 EOI (0xFF 0xD9)
        let mut truncated = make_jpeg(32, 32, None);
        assert!(truncated.ends_with(&[0xFF, 0xD9]));
        truncated.truncate(truncated.len() - 4);
        assert!(
            parse_jpeg_header(&truncated).is_err(),
            "缺少 EOI 结束标记的截断 JPEG 必须拒绝直通"
        );

        // 3. 合法 JPEG 尾部带附加填充字节（如 MPF/零填充）仍应允许直通
        let mut with_trailer = make_jpeg(32, 32, None);
        with_trailer.extend_from_slice(&[0x00, 0x00, 0x00]);
        assert!(parse_jpeg_header(&with_trailer).is_ok());
    }

    #[test]
    fn test_adobe_rgb_jpeg_sets_dct_color_transform_false() {
        let mut d = vec![0xFF, 0xD8];
        // Adobe APP14 transform = 0 (Raw RGB)
        d.extend_from_slice(&[0xFF, 0xEE, 0x00, 0x0E]);
        d.extend_from_slice(b"Adobe");
        d.extend_from_slice(&[0, 100, 0, 0, 0, 0, 0]);
        d.extend_from_slice(&[
            0xFF, 0xC0, 0x00, 0x11, 8, 0, 16, 0, 16, 3, b'R', 0x11, 0, b'G', 0x11, 0, b'B', 0x11,
            0,
        ]);
        d.extend_from_slice(&[
            0xFF, 0xDA, 0x00, 0x0C, 3, b'R', 0, b'G', 0, b'B', 0, 0, 63, 0, 0x00, 0xFF, 0xD9,
        ]);
        let info = parse_jpeg_header(&d).unwrap();
        assert_eq!(info.color_space, ColorSpace::DeviceRgb);
        assert_eq!(info.dct_color_transform, Some(false));
    }

    #[test]
    fn test_thumbnail_preserves_rgba_alpha_channel() {
        let mut png_bytes = Vec::new();
        let img = image::RgbaImage::from_fn(8, 8, |x, _| {
            if x < 4 {
                image::Rgba([0, 0, 0, 0])
            } else {
                image::Rgba([255, 0, 0, 255])
            }
        });
        img.write_to(
            &mut std::io::Cursor::new(&mut png_bytes),
            image::ImageFormat::Png,
        )
        .unwrap();

        let entry = create_image_entry(1, "alpha.png".into(), png_bytes, None, None);
        let rgba = entry.thumb_rgb.as_ref().expect("应生成 RGBA 缩略图");
        assert_eq!(rgba.len(), entry.thumb_width * entry.thumb_height * 4);
        assert_eq!(rgba[3], 0, "左边缘透明像素应保留 Alpha = 0");
        let right_edge_alpha = rgba[(entry.thumb_width - 1) * 4 + 3];
        assert_eq!(right_edge_alpha, 255, "右边缘不透明像素应保留 Alpha = 255");
    }

    /// 按 PNG 规范逆滤波还原像素，用于验证自适应预测器 100% 无损可逆
    fn undo_png_filter(filtered: &[u8], width: usize, height: usize, channels: usize) -> Vec<u8> {
        let stride = width * channels;
        let bpp = channels;
        assert_eq!(filtered.len(), height * (stride + 1));
        let mut out = vec![0u8; height * stride];
        for y in 0..height {
            let row_in = &filtered[y * (stride + 1)..(y + 1) * (stride + 1)];
            let filter_type = row_in[0];
            let src = &row_in[1..];
            for i in 0..stride {
                let left = if i >= bpp {
                    out[y * stride + i - bpp]
                } else {
                    0
                };
                let up = if y > 0 {
                    out[(y - 1) * stride + i]
                } else {
                    0
                };
                let up_left = if y > 0 && i >= bpp {
                    out[(y - 1) * stride + i - bpp]
                } else {
                    0
                };
                let pred = match filter_type {
                    0 => 0,
                    1 => left,
                    2 => up,
                    3 => ((left as u16 + up as u16) >> 1) as u8,
                    4 => paeth_predictor(left, up, up_left),
                    other => panic!("非法 PNG 滤波器类型: {}", other),
                };
                out[y * stride + i] = src[i].wrapping_add(pred);
            }
        }
        out
    }

    #[test]
    fn test_png_adaptive_filter_roundtrip_and_shrinks_gradient() {
        // 1. RGB 渐变图：验证逆滤波 bit-for-bit 还原，且体积显著小于裸像素 zlib
        let (w, h) = (128u32, 128u32);
        let rgb_img = image::RgbImage::from_fn(w, h, |x, y| {
            image::Rgb([
                ((x * 2) & 0xFF) as u8,
                ((y * 2) & 0xFF) as u8,
                ((x + y) & 0xFF) as u8,
            ])
        });
        let raw_rgb = rgb_img.as_raw();
        let mut png_bytes = Vec::new();
        rgb_img
            .write_to(
                &mut std::io::Cursor::new(&mut png_bytes),
                image::ImageFormat::Png,
            )
            .unwrap();

        let processed = decode_and_compress_flate(&png_bytes).unwrap();
        assert!(processed.flate_predictor);
        assert_eq!(processed.color_space, ColorSpace::DeviceRgb);

        let inflated = miniz_oxide::inflate::decompress_to_vec_zlib(&processed.stream_data)
            .expect("FlateDecode 流应为合法 zlib 数据");
        let restored = undo_png_filter(&inflated, w as usize, h as usize, 3);
        assert_eq!(restored, *raw_rgb, "RGB 预测滤波还原必须与原始像素逐字节一致");

        let raw_zlib = miniz_oxide::deflate::compress_to_vec_zlib(raw_rgb, 6);
        assert!(
            processed.stream_data.len() < raw_zlib.len(),
            "带 PNG 预测器的压缩流 ({}) 应小于裸像素 zlib ({})",
            processed.stream_data.len(),
            raw_zlib.len()
        );

        // 2. Grayscale 渐变图：验证单通道逆滤波还原
        let gray_img = image::GrayImage::from_fn(w, h, |x, y| {
            image::Luma([((x * 3 + y * 5) & 0xFF) as u8])
        });
        let raw_gray = gray_img.as_raw();
        let filtered_gray = apply_png_adaptive_filter(raw_gray, w as usize, h as usize, 1, 1);
        assert_eq!(
            undo_png_filter(&filtered_gray, w as usize, h as usize, 1),
            *raw_gray
        );
    }

    #[test]
    fn test_native_grayscale_png_direct_idat_passthrough() {
        let img = image::GrayImage::from_fn(64, 48, |x, y| {
            image::Luma([((x * 4 + y * 2) & 0xFF) as u8])
        });
        let mut png_bytes = Vec::new();
        img.write_to(
            &mut std::io::Cursor::new(&mut png_bytes),
            image::ImageFormat::Png,
        )
        .unwrap();

        // 提取原始 PNG 中的全部 IDAT 字节用于比对
        let mut expected_idat = Vec::new();
        let mut pos = 8usize;
        while pos + 12 <= png_bytes.len() {
            let len = u32::from_be_bytes(png_bytes[pos..pos + 4].try_into().unwrap()) as usize;
            if &png_bytes[pos + 4..pos + 8] == b"IDAT" {
                expected_idat.extend_from_slice(&png_bytes[pos + 8..pos + 8 + len]);
            }
            pos += 12 + len;
        }

        let processed = process_image_bytes(png_bytes, true, 100).unwrap();
        assert_eq!(processed.color_space, ColorSpace::DeviceGray);
        assert_eq!(processed.bits_per_component, 8);
        assert_eq!(processed.filter, Filter::FlateDecode);
        assert!(processed.flate_predictor);
        assert_eq!(
            processed.stream_data, expected_idat,
            "原生非隔行灰度 PNG 应直接复用原始 IDAT 流，无需重新编码"
        );
    }

    #[test]
    fn test_truncated_png_does_not_passthrough() {
        let img = image::GrayImage::from_fn(16, 16, |x, _| image::Luma([x as u8 * 10]));
        let mut png_bytes = Vec::new();
        img.write_to(
            &mut std::io::Cursor::new(&mut png_bytes),
            image::ImageFormat::Png,
        )
        .unwrap();
        // 截断尾部 IEND 块
        png_bytes.truncate(png_bytes.len() - 8);
        assert!(
            try_passthrough_grayscale_png(&png_bytes).is_none(),
            "缺少 IEND 的截断 PNG 不应触发直通"
        );
    }

    #[test]
    fn test_truncated_jpeg_rejected_in_both_modes() {
        let full = make_jpeg(400, 400, None);
        let truncated = &full[..full.len() * 55 / 100];
        assert!(!truncated.ends_with(&[0xFF, 0xD9]), "截断后不应还有 EOI");
        assert!(parse_jpeg_header(truncated).is_err());

        // 解码器对截断流很"宽容"（会补齐缺失行），所以必须由结构校验统一拦下，
        // 否则残缺图会以 Flate 形式静默写入 PDF，产出白条或半张图。
        assert!(
            process_image_bytes(truncated.to_vec(), true, 100).is_err(),
            "截断 JPEG 不得进入无损直通路径"
        );
        assert!(
            process_image_bytes(truncated.to_vec(), false, 80).is_err(),
            "截断 JPEG 不得进入有损重编路径"
        );
        // 完整的 JPEG 仍然正常工作
        assert!(process_image_bytes(full, true, 100).is_ok());
    }

    #[test]
    fn test_corrupted_png_idat_crc_rejected_from_passthrough() {
        let img =
            image::GrayImage::from_fn(32, 32, |x, y| image::Luma([((x * 7 + y * 3) & 0xFF) as u8]));
        let mut png_bytes = Vec::new();
        img.write_to(
            &mut std::io::Cursor::new(&mut png_bytes),
            image::ImageFormat::Png,
        )
        .unwrap();
        assert!(
            try_passthrough_grayscale_png(&png_bytes).is_some(),
            "完好的灰度 PNG 应可直通"
        );

        // 翻转 IDAT 数据区中的 1 个比特，使该块的 CRC32 不再匹配
        let idat_type_pos = png_bytes
            .windows(4)
            .position(|w| w == b"IDAT")
            .expect("应存在 IDAT 块");
        let data_pos = idat_type_pos + 4;
        assert!(data_pos + 3 < png_bytes.len());
        png_bytes[data_pos + 3] ^= 0xFF;

        assert!(
            try_passthrough_grayscale_png(&png_bytes).is_none(),
            "IDAT 校验和损坏的 PNG 必须回退解码路径，不能原样写入 PDF"
        );
    }

    #[test]
    fn test_16bit_grayscale_png_passthrough_keeps_bit_depth() {
        let img =
            image::ImageBuffer::<image::Luma<u16>, Vec<u16>>::from_fn(24, 16, |x, y| {
                image::Luma([(x as u16 * 997).wrapping_add(y as u16 * 131)])
            });
        let mut png_bytes = Vec::new();
        img.write_to(
            &mut std::io::Cursor::new(&mut png_bytes),
            image::ImageFormat::Png,
        )
        .unwrap();

        let processed =
            try_passthrough_grayscale_png(&png_bytes).expect("16-bit 灰度 PNG 应可零解码直通");
        assert_eq!(processed.bits_per_component, 16, "不得把 16-bit 降为 8-bit");
        assert_eq!(processed.color_space, ColorSpace::DeviceGray);
        assert_eq!(processed.filter, Filter::FlateDecode);
        assert_eq!((processed.width, processed.height), (24.0, 16.0));
    }

    #[test]
    fn test_16bit_rgb_png_preserves_bit_depth_through_flate_path() {
        let img =
            image::ImageBuffer::<image::Rgb<u16>, Vec<u16>>::from_fn(20, 10, |x, y| {
                image::Rgb([x as u16 * 1000, y as u16 * 2000, 30000])
            });
        let mut png_bytes = Vec::new();
        img.write_to(
            &mut std::io::Cursor::new(&mut png_bytes),
            image::ImageFormat::Png,
        )
        .unwrap();

        assert!(
            try_passthrough_grayscale_png(&png_bytes).is_none(),
            "彩色 PNG 不走灰度直通"
        );
        let processed = process_image_bytes(png_bytes, true, 100).unwrap();
        assert_eq!(
            processed.bits_per_component, 16,
            "16-bit 源在无损模式下不得被静默降为 8-bit"
        );
        assert_eq!(processed.color_space, ColorSpace::DeviceRgb);
    }
}
