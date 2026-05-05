use anyhow::{bail, Result};
use image::ImageReader;
use std::io::Cursor;

const MAX_BOOK_BYTES: u64 = 2 * 1024 * 1024;
const MAX_DIMENSION: u32 = 4096;
const MAX_TOTAL_PIXELS: u64 = 4_000_000;
const MAX_ANIMATION_FRAMES: usize = 200;

/// Returns (width, height, animated) if the bytes pass validation. Errors are
/// human-readable since they're surfaced to the user.
pub fn validate_image(data: &[u8], strict_book_size: bool) -> Result<(u32, u32, bool)> {
    if strict_book_size && (data.len() as u64) > MAX_BOOK_BYTES {
        bail!("image too large for book ({} bytes; max {} bytes)", data.len(), MAX_BOOK_BYTES);
    }
    if data.len() < 16 {
        bail!("image too small to be valid");
    }

    let magic = &data[..16];
    let format = if magic.starts_with(&[0x89, 0x50, 0x4E, 0x47]) {
        "png"
    } else if magic.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "jpeg"
    } else if magic.starts_with(b"GIF87a") || magic.starts_with(b"GIF89a") {
        "gif"
    } else if magic.starts_with(b"RIFF") && &magic[8..12] == b"WEBP" {
        "webp"
    } else {
        bail!("unsupported image format (only PNG, JPEG, GIF, WebP are allowed)");
    };

    let reader = ImageReader::new(Cursor::new(data))
        .with_guessed_format()
        .map_err(|e| anyhow::anyhow!("failed to read image header: {}", e))?;
    let (width, height) = reader
        .into_dimensions()
        .map_err(|e| anyhow::anyhow!("invalid image header: {}", e))?;

    if width > MAX_DIMENSION || height > MAX_DIMENSION {
        bail!("image dimensions exceed limit ({}×{}; max {}×{} per axis)",
              width, height, MAX_DIMENSION, MAX_DIMENSION);
    }
    if (width as u64) * (height as u64) > MAX_TOTAL_PIXELS {
        bail!("image total pixels exceed limit ({}; max {})",
              (width as u64) * (height as u64), MAX_TOTAL_PIXELS);
    }

    let animated = match format {
        "gif" => count_gif_frames(data)? > 1,
        "webp" => is_webp_animated(data),
        _ => false,
    };

    if format == "gif" {
        let frames = count_gif_frames(data)?;
        if frames > MAX_ANIMATION_FRAMES {
            bail!("animated image has too many frames ({}; max {})", frames, MAX_ANIMATION_FRAMES);
        }
    }
    if format == "webp" && animated {
        let frames = count_webp_frames(data);
        if frames > MAX_ANIMATION_FRAMES {
            bail!("animated image has too many frames ({}; max {})", frames, MAX_ANIMATION_FRAMES);
        }
    }

    Ok((width, height, animated))
}

fn count_gif_frames(data: &[u8]) -> Result<usize> {
    if data.len() < 13 { bail!("gif too short"); }
    let packed = data[10];
    let has_gct = (packed & 0x80) != 0;
    let gct_size = if has_gct { 3 * (1 << ((packed & 0x07) + 1)) } else { 0 };
    let scan_start = 13 + gct_size;
    if scan_start >= data.len() { return Ok(0); }
    Ok(data[scan_start..].iter().filter(|&&b| b == 0x2C).count())
}

fn is_webp_animated(data: &[u8]) -> bool {
    if data.len() < 30 { return false; }
    let mut pos = 12;
    while pos + 8 <= data.len() {
        if &data[pos..pos+4] == b"ANIM" { return true; }
        let size = u32::from_le_bytes([data[pos+4], data[pos+5], data[pos+6], data[pos+7]]) as usize;
        pos += 8 + size + (size & 1);
    }
    false
}

fn count_webp_frames(data: &[u8]) -> usize {
    let mut count = 0;
    let mut pos = 12;
    while pos + 8 <= data.len() {
        if &data[pos..pos+4] == b"ANMF" { count += 1; }
        let size = u32::from_le_bytes([data[pos+4], data[pos+5], data[pos+6], data[pos+7]]) as usize;
        pos += 8 + size + (size & 1);
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_too_small() {
        assert!(validate_image(&[0u8; 4], false).is_err());
    }

    #[test]
    fn rejects_unknown_magic() {
        let mut data = vec![0u8; 100];
        data[0..4].copy_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
        let result = validate_image(&data, false);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("unsupported image format"));
    }

    #[test]
    fn accepts_minimal_png() {
        let png = generate_1x1_png();
        let (w, h, animated) = validate_image(&png, true).unwrap();
        assert_eq!(w, 1);
        assert_eq!(h, 1);
        assert!(!animated);
    }

    fn generate_1x1_png() -> Vec<u8> {
        vec![
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A,
            0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
            0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01,
            0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4, 0x89,
            0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54,
            0x78, 0x9C, 0x62, 0x00, 0x00, 0x00, 0x00, 0x05, 0x00, 0x01,
            0x0D, 0x0A, 0x2D, 0xB4,
            0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44,
            0xAE, 0x42, 0x60, 0x82,
        ]
    }
}
