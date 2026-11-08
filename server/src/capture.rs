//! UVC capture (MS2109 /dev/video0) via v4l2-ctl.
//! Notes from AGENTS.md: MJPG headers are sometimes corrupt on this device,
//! so initial frames are skipped (default skip=2) and the JPEG is validated
//! (SOI FFD8 / EOI FFD9) before being returned; failed grabs are retried
//! with a deeper skip.

use std::process::Command;

pub struct Frame {
    pub path: String,
    pub data: Vec<u8>,
    /// actual (possibly device-adjusted) frame dimensions
    pub w: u32,
    pub h: u32,
}

/// Ask the device what it actually negotiated for width/height/fourcc.
/// v4l2 SET_FMT silently ADJUSTS unsupported modes (measured: 640x360 YUYV on
/// the MS2109 comes back as 640x480), so grab requests must be re-aligned to
/// the negotiated values. Returns (w, h, fourcc).
fn negotiate(dev: &str, w: u32, h: u32, fmt: &str) -> Result<(u32, u32, String), String> {
    let out = Command::new("v4l2-ctl")
        .arg("-d")
        .arg(dev)
        .arg(format!("--set-fmt-video=width={},height={},pixelformat={}", w, h, fmt))
        .arg("--get-fmt-video")
        .output()
        .map_err(|e| format!("failed to run v4l2-ctl: {}", e))?;
    if !out.status.success() {
        return Err(format!("v4l2-ctl --get-fmt-video failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    parse_fmt(&String::from_utf8_lossy(&out.stdout)).ok_or_else(|| "could not parse --get-fmt-video output".into())
}

fn parse_fmt(s: &str) -> Option<(u32, u32, String)> {
    let mut wh = None;
    let mut fourcc = None;
    for line in s.lines() {
        let mut kv = line.splitn(2, ':');
        let (k, v) = (kv.next()?.trim(), kv.next()?.trim());
        if k == "Width/Height" {
            let mut p = v.split('/');
            let a = p.next()?.trim().parse().ok()?;
            let b = p.next()?.trim().parse().ok()?;
            wh = Some((a, b));
        } else if k == "Pixel Format" {
            fourcc = Some(v.split(' ').next()?.trim_matches('\'').to_string());
        }
    }
    Some((wh?.0, wh?.1, fourcc?))
}

fn v4l2_grab(dev: &str, w: u32, h: u32, fmt: &str, skip: u32, path: &str) -> Result<(), String> {
    // NOTE v4l2-ctl semantics (measured 2026-11-07): --stream-skip discards N
    // buffers, then --stream-count buffers are WRITTEN to the output file
    // concatenated. We want exactly ONE frame per call, so count is fixed at 1.
    let out = Command::new("v4l2-ctl")
        .arg("-d")
        .arg(dev)
        .arg(format!("--set-fmt-video=width={},height={},pixelformat={}", w, h, fmt))
        .arg("--stream-mmap")
        .arg("--stream-count=1")
        .arg(format!("--stream-skip={}", skip))
        .arg(format!("--stream-to={}", path))
        .output()
        .map_err(|e| format!("failed to run v4l2-ctl: {}", e))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("v4l2-ctl failed: {}", err.trim()));
    }
    Ok(())
}

fn looks_like_jpeg(b: &[u8]) -> bool {
    b.len() > 512 && b[0] == 0xFF && b[1] == 0xD8 && b[b.len() - 2] == 0xFF && b[b.len() - 1] == 0xD9
}

/// Grab one frame; retries with deeper skip if the frame comes out corrupt.
///
/// A warmup grab precedes the real one (same reasoning as grab_yuyv, and
/// confirmed for MJPG 2026-11-07: right after a format switch the device
/// re-negotiates EDID and the first MJPG frames are the stale no-signal
/// frames even with skip>=2, while the target signal was already live).
pub fn grab(dev: &str, w: u32, h: u32, skip: u32, seq: u32) -> Result<Frame, String> {
    let (aw, ah, _fourcc) = negotiate(dev, w, h, "MJPG")?;
    let path = format!("/tmp/usb-mcp-{}x{}-{}-{}.jpg", aw, ah, std::process::id(), seq);
    let warm = format!("{}.warm", path);
    let _ = v4l2_grab(dev, aw, ah, "MJPG", skip + 6, &warm);
    let _ = std::fs::remove_file(&warm);
    let mut last_err = String::from("no attempt made");
    let mut cur_skip = skip;
    for _ in 0..3 {
        let _ = std::fs::remove_file(&path);
        if let Err(e) = v4l2_grab(dev, aw, ah, "MJPG", cur_skip, &path) {
            last_err = e;
            cur_skip += 2;
            continue;
        }
        match std::fs::read(&path) {
            Ok(data) if looks_like_jpeg(&data) => return Ok(Frame { path, data, w: aw, h: ah }),
            Ok(data) => {
                let _ = std::fs::remove_file(&path);
                last_err = format!(
                    "frame {}x{} failed JPEG SOI/EOI check ({} bytes) — corrupt MJPG stream header, retrying with skip={}",
                    aw, ah, data.len(), cur_skip + 2
                );
            }
            Err(e) => last_err = format!("read {}: {}", path, e),
        }
        cur_skip += 2;
    }
    let _ = std::fs::remove_file(&path);
    Err(format!(
        "{} — if the target image never arrives, the device may be showing its no-signal pattern",
        last_err
    ))
}

/// Grab one raw YUYV frame (w*h*2 bytes); used for the PNG path.
///
/// Measured behavior (2026-11-07): right after a format switch the first
/// buffers of a fresh YUYV stream come out unsettled (dark/garbage, Y-avg ~42
/// while a settled bar pattern sits ~132). A SEPARATE subsequent invocation is
/// clean after only ~3 buffers. So: burn buffers with a warmup grab, discard
/// it, then do the real grab. Retries with deeper skip on short frames.
pub fn grab_yuyv(dev: &str, w: u32, h: u32, skip: u32, seq: u32) -> Result<Frame, String> {
    let (aw, ah, fourcc) = negotiate(dev, w, h, "YUYV")?;
    if !fourcc.starts_with("YUYV") && !fourcc.starts_with("YUY2") {
        return Err(format!(
            "device does not support YUYV at {}x{} (negotiated pixelformat '{}' instead) — check capture_info for supported sizes",
            w, h, fourcc
        ));
    }
    let path = format!("/tmp/usb-mcp-{}x{}-{}-{}.yuyv", aw, ah, std::process::id(), seq);
    let need = (aw as usize) * (ah as usize) * 2;
    let warm = format!("{}.warm", path);
    let _ = v4l2_grab(dev, aw, ah, "YUYV", skip + 6, &warm);
    let _ = std::fs::remove_file(&warm);
    let mut last_err = String::from("no attempt made");
    let mut cur_skip = skip;
    for _ in 0..3 {
        let _ = std::fs::remove_file(&path);
        if let Err(e) = v4l2_grab(dev, aw, ah, "YUYV", cur_skip, &path) {
            last_err = e;
            cur_skip += 2;
            continue;
        }
        match std::fs::read(&path) {
            Ok(data) if data.len() == need => return Ok(Frame { path, data, w: aw, h: ah }),
            Ok(data) => {
                let _ = std::fs::remove_file(&path);
                last_err = format!(
                    "yuyv frame {}x{} got {} bytes (expected exactly {}) — stream hiccup, retrying with skip={}",
                    aw, ah, data.len(), need, cur_skip + 2
                );
            }
            Err(e) => last_err = format!("read {}: {}", path, e),
        }
        cur_skip += 2;
    }
    let _ = std::fs::remove_file(&path);
    Err(format!(
        "{} — if the target image never arrives, the device may be showing its no-signal or black frames",
        last_err
    ))
}

/// Device/format info for humans (v4l2-ctl --list-devices / --list-formats-ext).
pub fn info(dev: &str) -> String {
    let mut s = String::new();
    if let Ok(o) = Command::new("v4l2-ctl").arg("--list-devices").output() {
        s.push_str("=== v4l2-ctl --list-devices ===\n");
        s.push_str(&String::from_utf8_lossy(&o.stdout));
        if !o.stderr.is_empty() {
            s.push_str(&String::from_utf8_lossy(&o.stderr));
        }
    } else {
        s.push_str("(v4l2-ctl --list-devices failed)\n");
    }
    match Command::new("v4l2-ctl").arg("-d").arg(dev).arg("--list-formats-ext").output() {
        Ok(o) => {
            s.push_str(&format!("\n=== v4l2-ctl -d {} --list-formats-ext ===\n", dev));
            s.push_str(&String::from_utf8_lossy(&o.stdout));
            if !o.stderr.is_empty() {
                s.push_str(&String::from_utf8_lossy(&o.stderr));
            }
        }
        Err(_) => s.push_str(&format!("(v4l2-ctl -d {} --list-formats-ext failed)\n", dev)),
    }
    s
}

#[cfg(test)]
mod tests {
    use super::parse_fmt;

    #[test]
    fn parses_negotiated_fmt() {
        // real v4l2-ctl --get-fmt-video shape
        let s = "\
Video Capture Format:
\tWidth/Height : 640/480
\tPixel Format : 'YUYV' (YUYV 4:2:2)
\tField             : None
";
        assert_eq!(parse_fmt(s), Some((640, 480, "YUYV".to_string())));
    }

    #[test]
    fn parses_adjusted_fourcc() {
        let s = "Width/Height : 1280/720\nPixel Format : 'MJPG' (Motion-JPEG)\n";
        assert_eq!(parse_fmt(s), Some((1280, 720, "MJPG".to_string())));
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(parse_fmt(""), None);
        assert_eq!(parse_fmt("no fmt here"), None);
    }
}
