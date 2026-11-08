//! UVC capture (MS2109 /dev/video0) via v4l2-ctl.
//!
//! Raw YUYV only. The device's MJPEG path was retired 2026-11-08: its stream
//! headers came out corrupt at the head of a fresh capture and it kept emitting
//! stale no-signal frames after signal/format switches (measured history in
//! memo/20261107-usb-device-status.md). Frames are validated by exact size
//! (w*h*2 bytes) and retried with a deeper skip on a stream hiccup.

use std::process::Command;

/// The only pixel format we drive the device with.
const PIXFMT: &str = "YUYV";

pub struct Frame {
    pub data: Vec<u8>,
    /// actual (possibly device-adjusted) frame dimensions
    pub w: u32,
    pub h: u32,
}

/// Ask the device what it actually negotiated for width/height/fourcc.
/// v4l2 SET_FMT silently ADJUSTS unsupported modes (measured: 640x360 YUYV on
/// the MS2109 comes back as 640x480), so grab requests must be re-aligned to
/// the negotiated values. Returns (w, h, fourcc).
fn negotiate(dev: &str, w: u32, h: u32) -> Result<(u32, u32, String), String> {
    let out = Command::new("v4l2-ctl")
        .arg("-d")
        .arg(dev)
        .arg(format!("--set-fmt-video=width={},height={},pixelformat={}", w, h, PIXFMT))
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

fn v4l2_grab(dev: &str, w: u32, h: u32, skip: u32) -> Result<Vec<u8>, String> {
    // NOTE v4l2-ctl semantics (measured 2026-11-07): --stream-skip discards N
    // buffers, then --stream-count buffers are WRITTEN to the output file
    // concatenated. We want exactly ONE frame per call, so count is fixed at 1.
    // --stream-to=/dev/stdout lets us keep the frame in memory instead of a
    // local temp file; v4l2-ctl prints its progress spinner to stderr only.
    let out = Command::new("v4l2-ctl")
        .arg("-d")
        .arg(dev)
        .arg(format!("--set-fmt-video=width={},height={},pixelformat={}", w, h, PIXFMT))
        .arg("--stream-mmap")
        .arg("--stream-count=1")
        .arg(format!("--stream-skip={}", skip))
        .arg("--stream-to=/dev/stdout")
        .output()
        .map_err(|e| format!("failed to run v4l2-ctl: {}", e))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("v4l2-ctl failed: {}", err.trim()));
    }
    Ok(out.stdout)
}

/// Grab one raw YUYV frame (w*h*2 bytes); the only capture path (PNG output).
///
/// Measured behavior (2026-11-07): right after a format switch the first
/// buffers of a fresh YUYV stream come out unsettled (dark/garbage, Y-avg ~42
/// while a settled bar pattern sits ~132). A SEPARATE subsequent invocation is
/// clean after only ~3 buffers. So: burn buffers with a warmup grab, discard
/// it, then do the real grab. Retries with deeper skip on short frames.
pub fn grab_yuyv(dev: &str, w: u32, h: u32, skip: u32) -> Result<Frame, String> {
    let (aw, ah, fourcc) = negotiate(dev, w, h)?;
    if !fourcc.starts_with("YUYV") && !fourcc.starts_with("YUY2") {
        return Err(format!(
            "device does not support YUYV at {}x{} (negotiated pixelformat '{}' instead) — check capture_info for supported sizes. MJPEG is not captured even if the device offers it (retired 2026-11-08: unstable stream headers / stale no-signal frames)",
            w, h, fourcc
        ));
    }
    let need = (aw as usize) * (ah as usize) * 2;
    // Warmup: discard the first stream so the device settles after format switches.
    let _ = v4l2_grab(dev, aw, ah, skip + 6);
    let mut last_err = String::from("no attempt made");
    let mut cur_skip = skip;
    for _ in 0..3 {
        match v4l2_grab(dev, aw, ah, cur_skip) {
            Ok(data) if data.len() == need => return Ok(Frame { data, w: aw, h: ah }),
            Ok(data) => {
                last_err = format!(
                    "yuyv frame {}x{} got {} bytes (expected exactly {}) — stream hiccup, retrying with skip={}",
                    aw, ah, data.len(), need, cur_skip + 2
                );
            }
            Err(e) => last_err = e,
        }
        cur_skip += 2;
    }
    Err(format!(
        "{} — if the target image never arrives, the device may be showing its no-signal or black frames",
        last_err
    ))
}

/// Device/format info for humans. The device listing is restricted to UVC
/// devices (driver "uvcvideo"): on the Pi, a plain `v4l2-ctl --list-devices`
/// also dumps the bcm2835-codec / bcm2835-isp nodes, which are noise here.
pub fn info(dev: &str) -> String {
    let mut s = String::new();
    s.push_str("=== UVC capture devices (driver uvcvideo) ===\n");
    match list_uvc_devices() {
        Ok(nodes) if nodes.is_empty() => s.push_str("(no UVC /dev/video* device found)\n"),
        Ok(nodes) => {
            // group nodes by card+bus, preserving device order
            let mut groups: Vec<((String, String), Vec<String>)> = Vec::new();
            for (card, bus, path) in nodes {
                match groups.iter_mut().find(|((c, b), _)| *c == card && *b == bus) {
                    Some((_, paths)) => paths.push(path),
                    None => groups.push(((card, bus), vec![path])),
                }
            }
            for ((card, bus), paths) in groups {
                s.push_str(&format!("{} ({}):\n", card, bus));
                for p in paths {
                    s.push_str(&format!("\t{}\n", p));
                }
            }
        }
        Err(e) => s.push_str(&format!("(UVC device scan failed: {})\n", e)),
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

/// Scan /dev/video* and keep only real UVC nodes (driver "uvcvideo").
/// Returns (card, bus_info, path) in device-number order.
fn list_uvc_devices() -> Result<Vec<(String, String, String)>, String> {
    let mut vids: Vec<u32> = std::fs::read_dir("/dev")
        .map_err(|e| format!("cannot read /dev: {}", e))?
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.strip_prefix("video").and_then(|s| s.parse::<u32>().ok())
        })
        .collect();
    vids.sort();
    let mut nodes = Vec::new();
    for num in vids {
        let path = format!("/dev/video{}", num);
        if let Ok(o) = Command::new("v4l2-ctl").arg("-d").arg(&path).arg("--info").output() {
            if let Some((card, bus)) = uvc_node(&String::from_utf8_lossy(&o.stdout)) {
                nodes.push((card, bus, path));
            }
        }
    }
    Ok(nodes)
}

/// Parse `v4l2-ctl --info` output: Some((card, bus_info)) only if the node is
/// driven by uvcvideo (i.e. an actual UVC device), None for platform nodes
/// like bcm2835-codec / bcm2835-isp.
fn uvc_node(out: &str) -> Option<(String, String)> {
    let (mut driver, mut card, mut bus) = (None, None, None);
    for line in out.lines() {
        let mut kv = line.splitn(2, ':');
        let (k, v) = match (kv.next(), kv.next()) {
            (Some(k), Some(v)) => (k.trim(), v.trim()),
            _ => continue,
        };
        match k {
            "Driver name" => driver = Some(v),
            "Card type" => card = Some(v),
            "Bus info" => bus = Some(v),
            _ => {}
        }
    }
    if driver.as_deref() == Some("uvcvideo") {
        Some((card.unwrap_or_default().to_string(), bus.unwrap_or_default().to_string()))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_fmt, uvc_node};

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
        // device falling back to MJPG is what parse_fmt must report so
        // grab_yuyv can reject it (MJPEG itself is no longer captured)
        assert_eq!(parse_fmt(s), Some((1280, 720, "MJPG".to_string())));
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(parse_fmt(""), None);
        assert_eq!(parse_fmt("no fmt here"), None);
    }

    #[test]
    fn uvc_node_accepts_uvcvideo() {
        // real v4l2-ctl --info shape (MS2109 on the Pi); card contains ':' too
        let s = "\
Driver name      : uvcvideo
Card type        : USB Video: USB Video
Bus info         : usb-20980000.usb-1.2
Driver version   : 6.18.50
Capabilities     : 0x84a00001
\tVideo Capture
\tMetadata Capture
\tStreaming
";
        assert_eq!(
            uvc_node(s),
            Some(("USB Video: USB Video".to_string(), "usb-20980000.usb-1.2".to_string()))
        );
    }

    #[test]
    fn uvc_node_rejects_platform_nodes() {
        let s = "\
Driver name      : bcm2835-isp
Card type        : bcm2835-isp
Bus info         : platform:bcm2835-isp
Driver version   : 6.18.50
";
        assert_eq!(uvc_node(s), None);
        assert_eq!(uvc_node(""), None);
    }
}
