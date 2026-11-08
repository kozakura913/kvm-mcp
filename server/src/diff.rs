//! Frame-to-frame change detection on raw YUYV buffers.
//!
//! capture_frame keeps ONE baseline frame per video device: the most recent
//! full grab, whatever mode produced it. mode=diff compares the current grab
//! against that baseline and reports changed-pixel count + bounding box as
//! TEXT ONLY (no image block): a cheap "did the target screen move?" check
//! between HID actions. The baseline lives in memory only; it resets on
//! server restart or when the negotiated frame dimensions change.
//!
//! Comparison runs on the raw YUYV bytes before any RGB conversion or crop:
//! a YUYV quad (Y0 U Y1 V) covers two pixels, so Y bytes decide per-pixel
//! changes while a U/V change marks both paired pixels. Live HDMI carries a
//! few LSBs of sensor noise, hence the per-component threshold.

use std::collections::HashMap;
use std::sync::Mutex;

pub struct SavedFrame {
    pub w: u32,
    pub h: u32,
    pub yuyv: Vec<u8>,
}

/// Per-device baselines (one entry per device path).
pub struct Baselines {
    map: Mutex<HashMap<String, SavedFrame>>,
}

impl Baselines {
    pub fn new() -> Self {
        Self { map: Mutex::new(HashMap::new()) }
    }

    /// Remove and return the baseline stored for `dev` (None on first capture).
    pub fn take(&self, dev: &str) -> Option<SavedFrame> {
        let mut m = self.map.lock().unwrap_or_else(|e| e.into_inner());
        m.remove(dev)
    }

    /// Store `frame` as the new baseline for `dev`.
    pub fn put(&self, dev: &str, frame: SavedFrame) {
        let mut m = self.map.lock().unwrap_or_else(|e| e.into_inner());
        m.insert(dev.to_string(), frame);
    }
}

pub struct Stats {
    pub changed: usize,
    pub total: usize,
    /// Changed-pixel bounding box as (x, y, w, h) in FULL-frame pixel
    /// coordinates; None when nothing changed.
    pub bbox: Option<(u32, u32, u32, u32)>,
}

/// Compare the baseline frame against a fresh raw YUYV grab (same dimensions
/// assumed — the handler checks and resets the baseline on dimension change).
/// `win` restricts the check to a rect in full-frame pixel coordinates (e.g.
/// the crop region); None means the whole frame. `thresh`: per-component
/// absolute difference that counts as changed (0 compares bit-exact).
pub fn diff_yuyv(
    prev: &SavedFrame,
    cur: &[u8],
    win: Option<(u32, u32, u32, u32)>,
    thresh: i32,
) -> Result<Stats, String> {
    let (w, h) = (prev.w, prev.h);
    let need = (w as usize) * (h as usize) * 2;
    if prev.yuyv.len() != need || cur.len() != need {
        return Err(format!(
            "buffer size mismatch: baseline {}B / current {}B, frame {}x{} needs {}B",
            prev.yuyv.len(),
            cur.len(),
            w,
            h,
            need
        ));
    }
    let (wx, wy, ww, wh) = match win {
        Some(r) => r,
        None => (0, 0, w, h),
    };
    if ww == 0 || wh == 0 || wx >= w || wy >= h {
        return Err(format!("bad diff window {},{} {}x{} for frame {}x{}", wx, wy, ww, wh, w, h));
    }
    let ww = ww.min(w - wx);
    let wh = wh.min(h - wy);

    let half = (w / 2) as usize;
    let x_end = wx + ww;
    let y_end = wy + wh;
    // every pair index overlapping the window horizontally
    let i0 = (wx / 2) as usize;
    let i1 = ((x_end + 1) / 2) as usize;

    let mut changed = 0usize;
    let (mut minx, mut miny, mut maxx, mut maxy) = (u32::MAX, u32::MAX, 0u32, 0u32);
    let mut mark = |x: u32, y: u32, changed: &mut usize| {
        *changed += 1;
        if x < minx { minx = x; }
        if x > maxx { maxx = x; }
        if y < miny { miny = y; }
        if y > maxy { maxy = y; }
    };

    for y in wy..y_end {
        let row = y as usize * half;
        for i in i0..i1 {
            let b = (row + i) * 4;
            let (p0, pu, p1, pv) = (
                prev.yuyv[b] as i32,
                prev.yuyv[b + 1] as i32,
                prev.yuyv[b + 2] as i32,
                prev.yuyv[b + 3] as i32,
            );
            let (c0, cu, c1, cv) = (
                cur[b] as i32,
                cur[b + 1] as i32,
                cur[b + 2] as i32,
                cur[b + 3] as i32,
            );
            let chroma = (pv - cv).abs() > thresh || (pu - cu).abs() > thresh;
            let col = (i * 2) as u32;
            if (chroma || (p0 - c0).abs() > thresh) && col >= wx {
                mark(col, y, &mut changed);
            }
            if (chroma || (p1 - c1).abs() > thresh) && col + 1 < x_end {
                mark(col + 1, y, &mut changed);
            }
        }
    }

    let bbox = if changed == 0 {
        None
    } else {
        Some((minx, miny, maxx - minx + 1, maxy - miny + 1))
    };
    Ok(Stats { changed, total: ww as usize * wh as usize, bbox })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// flat gray frame, all components identical
    fn gray(w: u32, h: u32, y: u8, uv: u8) -> SavedFrame {
        let mut yuyv = vec![0u8; (w as usize) * (h as usize) * 2];
        for (i, b) in yuyv.iter_mut().enumerate() {
            *b = match i % 4 {
                1 => uv,
                3 => uv,
                _ => y,
            };
        }
        SavedFrame { w, h, yuyv }
    }

    fn frame(mut yuyv: Vec<u8>, w: u32, h: u32) -> SavedFrame {
        yuyv.resize((w as usize) * (h as usize) * 2, 0x80);
        SavedFrame { w, h, yuyv }
    }

    #[test]
    fn identical_frames_report_no_change() {
        let f = gray(8, 4, 0x80, 0x80);
        let g = gray(8, 4, 0x80, 0x80);
        let st = diff_yuyv(&f, &g.yuyv, None, 12).unwrap();
        assert_eq!(st.changed, 0);
        assert_eq!(st.total, 32);
        assert_eq!(st.bbox, None);
    }

    #[test]
    fn noise_below_threshold_ignored() {
        let f = gray(8, 4, 100, 128);
        let mut g = gray(8, 4, 100, 128);
        // +6 on one Y byte: under thresh 12 -> no change; thresh 3 -> changed
        g.yuyv[0] += 6;
        assert_eq!(diff_yuyv(&f, &g.yuyv, None, 12).unwrap().changed, 0);
        let st = diff_yuyv(&f, &g.yuyv, None, 3).unwrap();
        assert_eq!(st.changed, 1);
        assert_eq!(st.bbox, Some((0, 0, 1, 1)));
    }

    #[test]
    fn single_pixel_bbox() {
        let f = gray(16, 8, 128, 128);
        let mut g = gray(16, 8, 128, 128);
        // pixel (5,3): w=16 -> half=8; quad index = 3*8 + 5/2(=2) ; Y1 is the odd pixel
        let base = (3 * 8 + 2) * 4;
        g.yuyv[base + 2] = 255;
        let st = diff_yuyv(&f, &g.yuyv, None, 12).unwrap();
        assert_eq!(st.changed, 1);
        assert_eq!(st.bbox, Some((5, 3, 1, 1)));
    }

    #[test]
    fn chroma_change_marks_both_pixels_of_the_pair() {
        let f = gray(8, 1, 128, 128);
        let mut g = gray(8, 1, 128, 128);
        g.yuyv[1] = 200; // U of quad 0 -> pixels 0 and 1
        let st = diff_yuyv(&f, &g.yuyv, None, 12).unwrap();
        assert_eq!(st.changed, 2);
        assert_eq!(st.bbox, Some((0, 0, 2, 1)));
    }

    #[test]
    fn window_restricts_count_and_keeps_full_frame_coords() {
        let f = gray(16, 8, 128, 128);
        let mut g = gray(16, 8, 128, 128);
        // pixel (10,4) changed (quad index = 4*8 + 5): OUTSIDE window (0,0,8,8)
        g.yuyv[(4 * 8 + 5) * 4] = 255;
        let st = diff_yuyv(&f, &g.yuyv, Some((0, 0, 8, 8)), 12).unwrap();
        assert_eq!(st.changed, 0);
        assert_eq!(st.total, 64);
        // pixel pair (2,3) at row 4: U change in quad index 4*8+1 -> marks BOTH 2 and 3
        let mut g2 = g.yuyv.clone();
        g2[(4 * 8 + 1) * 4 + 1] = 20;
        let st = diff_yuyv(&f, &g2, Some((0, 0, 8, 8)), 12).unwrap();
        assert_eq!(st.changed, 2);
        assert_eq!(st.bbox, Some((2, 4, 2, 1)));
    }

    #[test]
    fn odd_origin_window_covers_its_pixels_only() {
        // window x=5..=8: a U change in quad i=2 (cols 4,5) counts col 5 but not 4
        let f = gray(8, 1, 128, 128);
        let mut g = gray(8, 1, 128, 128);
        g.yuyv[2 * 4 + 1] = 20; // U, thresh 12 -> changed
        let st = diff_yuyv(&f, &g.yuyv, Some((5, 0, 4, 1)), 12).unwrap();
        assert_eq!(st.changed, 1); // col 4 excluded by wx, col 5 counted
        assert_eq!(st.bbox, Some((5, 0, 1, 1)));
    }

    #[test]
    fn size_mismatch_is_an_error() {
        let f = gray(8, 8, 128, 128);
        assert!(diff_yuyv(&f, &[0x80u8; 64], None, 12).is_err());
    }

    #[test]
    fn bad_window_is_an_error() {
        let f = gray(8, 4, 128, 128);
        assert!(diff_yuyv(&f, &f.yuyv, Some((8, 0, 4, 2)), 12).is_err());
        assert!(diff_yuyv(&f, &f.yuyv, Some((0, 0, 0, 4)), 12).is_err());
    }

    #[test]
    fn window_larger_than_frame_is_clamped() {
        let f = gray(8, 4, 128, 128);
        let mut g = gray(8, 4, 128, 128);
        // last pixel of last row: quad index 3*4+3, Y1
        g.yuyv[(3 * 4 + 3) * 4 + 2] = 0;
        let st = diff_yuyv(&f, &g.yuyv, Some((4, 2, 100, 100)), 12).unwrap();
        assert_eq!(st.total, 4 * 2); // clamped to (4,2)..(8,4)
        assert_eq!(st.changed, 1);
        assert_eq!(st.bbox, Some((7, 3, 1, 1)));
    }

    #[test]
    fn baselines_take_put_per_device() {
        let b = Baselines::new();
        assert!(b.take("/dev/video0").is_none());
        b.put("/dev/video0", frame(vec![1], 1, 1));
        b.put("/dev/video1", frame(vec![9], 1, 1));
        let prev = b.take("/dev/video0").unwrap();
        assert_eq!(prev.yuyv[0], 1);
        // take REMOVES: a second take is empty until put again
        assert!(b.take("/dev/video0").is_none());
        // other device untouched
        assert_eq!(b.take("/dev/video1").unwrap().yuyv[0], 9);
    }
}
