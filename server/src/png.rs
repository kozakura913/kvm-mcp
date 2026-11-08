//! PNG path for capture: raw YUYV → RGB → **compressed** PNG, encoded by the
//! `image` crate (default-features=false, png-only → pure-Rust png +
//! flate2/miniz_oxide, no C codecs). This is the only capture output: the
//! device's MJPEG path was retired 2026-11-08 (unstable), so no JPEG decoder
//! feature is compiled in.

use image::codecs::png::PngEncoder;
use image::{ExtendedColorType, ImageEncoder};

// ---------- YUYV → RGB ----------

/// Convert one YUYV frame (w*h*2 bytes, byte order Y0 U Y1 V) to packed RGB.
/// BT.601 integer approximation, values clamped to 0..=255.
pub fn yuyv_to_rgb(yuyv: &[u8], w: u32, h: u32) -> Result<Vec<u8>, String> {
    let want = (w as usize) * (h as usize) * 2;
    if yuyv.len() < want {
        return Err(format!("yuyv buffer too short: {} bytes, need {}", yuyv.len(), want));
    }
    let mut out = vec![0u8; (w as usize) * (h as usize) * 3];
    let clamp = |v: i32| -> u8 { v.max(0).min(255) as u8 };
    for (i, px) in out.chunks_mut(6).enumerate() {
        // 4 source bytes -> 2 RGB pixels
        let base = i * 4;
        let (y0, u, y1, v) = (
            yuyv[base] as i32,
            yuyv[base + 1] as i32,
            yuyv[base + 2] as i32,
            yuyv[base + 3] as i32,
        );
        let (c, d) = (v - 128, u - 128); // c = Cr offset, d = Cb offset
        px[0] = clamp(y0 + ((1436 * c) >> 10));
        px[1] = clamp(y0 - ((352 * c + 731 * d) >> 10));
        px[2] = clamp(y0 + ((1815 * d) >> 10));
        px[3] = clamp(y1 + ((1436 * c) >> 10));
        px[4] = clamp(y1 - ((352 * c + 731 * d) >> 10));
        px[5] = clamp(y1 + ((1815 * d) >> 10));
    }
    Ok(out)
}

// ---------- PNG encode (compressed) ----------

/// Encode packed RGB (len == w*h*3) as a compressed PNG (RGB8, deflate).
pub fn encode_rgb_png(w: u32, h: u32, rgb: &[u8]) -> Result<Vec<u8>, String> {
    let need = (w as usize) * (h as usize) * 3;
    if rgb.len() != need {
        return Err(format!("rgb buffer {} bytes, expected {}", rgb.len(), need));
    }
    if w == 0 || h == 0 {
        return Err("zero-size image".into());
    }
    let mut out: Vec<u8> = Vec::new();
    {
        let enc = PngEncoder::new(&mut out);
        enc.write_image(rgb, w, h, ExtendedColorType::Rgb8)
            .map_err(|e| format!("png encode failed: {e}"))?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yuyv_gray_and_clamp() {
        // 2x1: u=v=128 (neutral) -> r=g=b=y
        let frame = vec![0x80, 0x80, 0xFF, 0x80]; // y0=128, y1=255
        let rgb = yuyv_to_rgb(&frame, 2, 1).unwrap();
        assert_eq!(&rgb[..3], &[128, 128, 128]);
        assert_eq!(&rgb[3..6], &[255, 255, 255]);
        // saturated chroma: r clamps up, b clamps down (BT.601, y=128,u=0,v=255)
        let frame2 = vec![0x80, 0x00, 0x80, 0xFF];
        let rgb2 = yuyv_to_rgb(&frame2, 2, 1).unwrap();
        assert_eq!(rgb2[0], 255, "red should clamp at 255");
        assert_eq!(rgb2[1], 176, "green: 128 - ((352*127 + 731*-128)>>10) = 176");
        assert_eq!(rgb2[2], 0, "blue should clamp at 0");
        // short buffer rejected
        assert!(yuyv_to_rgb(&[0u8; 3], 2, 1).is_err());
    }

    #[test]
    fn png_encode_rejects_bad_input() {
        assert!(encode_rgb_png(2, 2, &[0u8; 5]).is_err());
        assert!(encode_rgb_png(0, 1, &[]).is_err());
    }

    #[test]
    fn png_roundtrip_is_lossless_and_compressed() {
        let (w, h) = (64u32, 64u32);
        let mut rgb = vec![0u8; (w * h * 3) as usize];
        for (i, px) in rgb.chunks_mut(3).enumerate() {
            px[0] = (i % 256) as u8;
            px[1] = 40;
            px[2] = 200;
        }
        let png = encode_rgb_png(w, h, &rgb).unwrap();
        assert_eq!(&png[..8], &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]);

        // decode back with image: pixel-exact round-trip
        let img = image::load_from_memory(&png).expect("decode our own PNG");
        assert_eq!((img.width(), img.height()), (w, h));
        assert_eq!(img.to_rgb8().as_raw(), &rgb);

        // compressed PNG must beat raw scanline bytes (deflate is really on)
        let raw_size = (w as usize * h as usize * 3) + h as usize; // + filter bytes
        assert!(
            png.len() < raw_size,
            "expected compressed PNG ({}B) < raw ({}B)",
            png.len(),
            raw_size
        );
    }
}
