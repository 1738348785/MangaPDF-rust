use std::fs;
use std::path::Path;
use image::{imageops::FilterType, DynamicImage, Rgba, RgbaImage};

struct IconFrame {
    width: u32,
    height: u32,
    data: Vec<u8>, // DIB or PNG data
}

fn create_dib_frame(img: &DynamicImage, size: u32) -> IconFrame {
    let resized = img.resize_exact(size, size, FilterType::Lanczos3).to_rgba8();
    let width = size;
    let height = size;

    let mut dib = Vec::new();
    // 1. BITMAPINFOHEADER (40 bytes)
    let header_size = 40u32;
    let bi_height = (height * 2) as i32; // In ICO DIB, height is doubled (XOR + AND)
    let bi_size_image = width * height * 4;

    dib.extend_from_slice(&header_size.to_le_bytes()); // biSize (40)
    dib.extend_from_slice(&(width as i32).to_le_bytes()); // biWidth
    dib.extend_from_slice(&bi_height.to_le_bytes()); // biHeight (doubled)
    dib.extend_from_slice(&1u16.to_le_bytes()); // biPlanes (1)
    dib.extend_from_slice(&32u16.to_le_bytes()); // biBitCount (32)
    dib.extend_from_slice(&0u32.to_le_bytes()); // biCompression (BI_RGB = 0)
    dib.extend_from_slice(&bi_size_image.to_le_bytes()); // biSizeImage
    dib.extend_from_slice(&0i32.to_le_bytes()); // biXPelsPerMeter
    dib.extend_from_slice(&0i32.to_le_bytes()); // biYPelsPerMeter
    dib.extend_from_slice(&0u32.to_le_bytes()); // biClrUsed
    dib.extend_from_slice(&0u32.to_le_bytes()); // biClrImportant

    // 2. Pixel data in BGRA, bottom-to-top
    for y in (0..height).rev() {
        for x in 0..width {
            let pixel = resized.get_pixel(x, y);
            let r = pixel[0];
            let g = pixel[1];
            let b = pixel[2];
            let a = pixel[3];
            dib.push(b);
            dib.push(g);
            dib.push(r);
            dib.push(a);
        }
    }

    // 3. 1-bit AND mask (all 0s since alpha is in 32-bit channel)
    let mask_row_bytes = ((width + 31) / 32) * 4;
    let and_mask = vec![0u8; (mask_row_bytes * height) as usize];
    dib.extend_from_slice(&and_mask);

    IconFrame {
        width,
        height,
        data: dib,
    }
}

fn create_png_frame(img: &DynamicImage, size: u32) -> IconFrame {
    let resized = img.resize_exact(size, size, FilterType::Lanczos3);
    let mut png_bytes = Vec::new();
    let mut cursor = std::io::Cursor::new(&mut png_bytes);
    resized.write_to(&mut cursor, image::ImageFormat::Png).unwrap();

    IconFrame {
        width: size,
        height: size,
        data: png_bytes,
    }
}

// Signed distance to a 2D rounded box
fn sd_rounded_box(px: f32, py: f32, half_w: f32, half_h: f32, r: f32) -> f32 {
    let qx = px.abs() - half_w + r;
    let qy = py.abs() - half_h + r;
    let outside_x = qx.max(0.0);
    let outside_y = qy.max(0.0);
    let dist_outside = (outside_x * outside_x + outside_y * outside_y).sqrt();
    let dist_inside = qx.max(qy).min(0.0);
    dist_outside + dist_inside - r
}

fn smooth_and_transparize(input_img: &DynamicImage) -> DynamicImage {
    // 1. Crop to the content bounds and upscale 4x to high-res canvas (1024x1024)
    let target_dim = 1024u32;
    let high_res = input_img.resize_exact(target_dim, target_dim, FilterType::Lanczos3).to_rgba8();

    let mut output = RgbaImage::new(target_dim, target_dim);

    let center_x = target_dim as f32 / 2.0;
    let center_y = target_dim as f32 / 2.0;

    // Dimensions for rounded squircle container in 1024x1024 space
    let margin = 32.0;
    let half_w = (target_dim as f32 / 2.0) - margin;
    let half_h = (target_dim as f32 / 2.0) - margin;
    let corner_radius = 210.0; // Apple/Windows smooth squircle curvature

    for y in 0..target_dim {
        for x in 0..target_dim {
            // 8x8 supersampling per pixel for ultra-smooth anti-aliasing
            let mut alpha_sum = 0.0f32;
            let samples = 4;
            for sy in 0..samples {
                for sx in 0..samples {
                    let sub_x = (x as f32 + (sx as f32 + 0.5) / samples as f32) - center_x;
                    let sub_y = (y as f32 + (sy as f32 + 0.5) / samples as f32) - center_y;
                    let d = sd_rounded_box(sub_x, sub_y, half_w, half_h, corner_radius);
                    if d <= 0.0 {
                        alpha_sum += 1.0;
                    } else if d < 1.5 {
                        let factor = (1.5 - d) / 1.5;
                        alpha_sum += factor.clamp(0.0, 1.0);
                    }
                }
            }
            let coverage = alpha_sum / (samples * samples) as f32;

            if coverage > 0.0 {
                // Get pixel color from high-res image
                let src_px = high_res.get_pixel(x, y);
                // If it's a white background pixel that got caught inside boundary, clamp
                let r = src_px[0];
                let g = src_px[1];
                let b = src_px[2];
                
                let final_alpha = (coverage * 255.0).round().clamp(0.0, 255.0) as u8;
                output.put_pixel(x, y, Rgba([r, g, b, final_alpha]));
            } else {
                output.put_pixel(x, y, Rgba([0, 0, 0, 0]));
            }
        }
    }

    // Downscale to 256x256 with Lanczos3 for final perfection
    let dyn_img = DynamicImage::ImageRgba8(output);
    dyn_img.resize_exact(256, 256, FilterType::Lanczos3)
}

fn main() -> anyhow::Result<()> {
    let assets_dir = Path::new("assets");
    if !assets_dir.exists() {
        fs::create_dir_all(assets_dir)?;
    }

    // Priority: icon2.png if exists, else original
    let input_path = if assets_dir.join("icon2.png").exists() {
        assets_dir.join("icon2.png")
    } else {
        Path::new(r"C:\Users\1738348785\.gemini\antigravity\brain\89be20ae-147a-49a9-ac45-c1a4fae6fdb5\mangapdf_app_icon_1788030551238.jpg").to_path_buf()
    };

    println!("Processing: {}", input_path.display());
    let raw_img = image::open(&input_path)?;

    // Smooth and create transparent corners with subpixel anti-aliasing
    let smooth_img = smooth_and_transparize(&raw_img);

    // Save PNG 256x256
    smooth_img.save_with_format(assets_dir.join("icon.png"), image::ImageFormat::Png)?;
    println!("Generated ultra-smooth transparent assets/icon.png");

    // Standard Windows multi-resolution icon sizes: 16, 32, 48, 64, 128 (as DIB) and 256 (as PNG)
    let mut frames = Vec::new();
    for size in [16, 32, 48, 64, 128] {
        frames.push(create_dib_frame(&smooth_img, size));
    }
    frames.push(create_png_frame(&smooth_img, 256));

    // Build ICO binary
    let mut ico = Vec::new();
    let num_images = frames.len() as u16;

    // 1. Header (6 bytes)
    ico.extend_from_slice(&[0x00, 0x00]); // Reserved
    ico.extend_from_slice(&[0x01, 0x00]); // Type: 1 = ICO
    ico.extend_from_slice(&num_images.to_le_bytes()); // Count

    // Compute offsets
    let header_size = 6 + (num_images as usize * 16);
    let mut current_offset = header_size as u32;

    // 2. Directory Entries (16 bytes each)
    for frame in &frames {
        let w_byte = if frame.width >= 256 { 0 } else { frame.width as u8 };
        let h_byte = if frame.height >= 256 { 0 } else { frame.height as u8 };
        ico.push(w_byte);
        ico.push(h_byte);
        ico.push(0); // Palette
        ico.push(0); // Reserved
        ico.extend_from_slice(&1u16.to_le_bytes()); // Planes
        ico.extend_from_slice(&32u16.to_le_bytes()); // Bit count
        ico.extend_from_slice(&(frame.data.len() as u32).to_le_bytes()); // Size
        ico.extend_from_slice(&current_offset.to_le_bytes()); // Offset

        current_offset += frame.data.len() as u32;
    }

    // 3. Image Payloads
    for frame in &frames {
        ico.extend_from_slice(&frame.data);
    }

    fs::write(assets_dir.join("app_icon.ico"), ico)?;
    println!("Generated multi-resolution assets/app_icon.ico successfully!");

    Ok(())
}
