//! usb-mcp: Streamable HTTP MCP server for the pi-mcp.lan USB rig.
//!
//! Devices (on the Pi):
//!   /dev/video0  MS2109 HDMI capture of the target PC's screen (UVC)
//!   /dev/ttyUSB0 CH340E UART wired to the CH9329 USB-HID injector (board §docs)
//!
//! Transport: MCP "Streamable HTTP" — JSON-RPC 2.0 over HTTP POST /mcp,
//! single application/json responses (SSE streams are optional per spec and
//! not offered; GET /mcp returns 405 per spec). Session IDs are assigned on
//! initialize and must be echoed in the Mcp-Session-Id header.

mod b64;
mod capture;
mod ch9329;
mod diff;
mod keys;
mod png;

use serde_json::{json, Value};
use std::collections::HashSet;
use std::io::Read;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use tiny_http::{Header, Method, Request, Response, Server};

const LATEST_PROTOCOL: &str = "2025-06-18";
const SUPPORTED_PROTOCOLS: [&str; 4] = ["2025-06-18", "2025-03-26", "2024-11-05", "2024-10-07"];

const INSTRUCTIONS: &str = "Rig topology: this server runs on a Raspberry Pi (pi-mcp.lan). \
/dev/video0 is an MS2109 HDMI capture of a target PC's screen; /dev/ttyUSB0 is a CH340E UART \
feeding a CH9329 that injects keyboard/mouse HID into the target PC over USB. \
Workflow: capture_frame to see the screen, then hid_* tools to act, then capture again to verify. \
capture_frame supports mode=diff (text-only change stats vs the previous capture, no image — cheap \
verify loop) and region {x,y,w,h} (crop a rect, e.g. to read one area at full detail). \
hid_mouse_click action=press holds a mouse button DOWN across tool calls (moves/scrolled frames keep \
it pressed) until action=release on the same button or hid_mouse_release; plain click still \
presses+releases within the call. \
Absolute mouse positioning is unreliable on the target — use hid_mouse_move (relative) plus \
captures to steer. Proving keyboard injection works: type 'echo <marker>' + Enter into a terminal \
on the target and check the capture shows the result. Never worry about bricking the CH9329: \
this server refuses the dangerous hidden commands by design.";

struct State {
    video_dev: String,
    hid: Mutex<ch9329::Ch9329>,
    sessions: Mutex<HashSet<String>>,
    baselines: diff::Baselines,
}

fn main() {
    let env = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.to_string());
    let bind = format!("{}:{}", env("MCP_HOST", "0.0.0.0"), env("MCP_PORT", "8765"));
    let video_dev = env("VIDEO_DEV", "/dev/video0");
    let serial_dev = env("SERIAL_DEV", "/dev/ttyUSB0");
    let baud: u32 = env("CH9329_BAUD", "9600").parse().unwrap_or(9600);

    let state = Arc::new(State {
        video_dev: video_dev.clone(),
        hid: Mutex::new(ch9329::Ch9329::new(serial_dev.clone(), baud)),
        sessions: Mutex::new(HashSet::new()),
        baselines: diff::Baselines::new(),
    });

    // A previous server process may have died while a mouse button was held;
    // send a zero-button frame so the target does not stay stuck in a drag.
    // Best effort: the serial device may not be plugged in yet at boot.
    {
        let mut hid = state.hid.lock().unwrap_or_else(|e| e.into_inner());
        if let Err(e) = hid.mouse_release_all() {
            eprintln!("usb-mcp: startup mouse-button release failed (ignored): {}", e);
        }
    }

    let server = match Server::http(bind.as_str()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("usb-mcp: cannot bind {}: {}", bind, e);
            std::process::exit(1);
        }
    };
    println!(
        "usb-mcp {} listening on http://{}/mcp  (video={}, serial={} @ {}baud)",
        env!("CARGO_PKG_VERSION"), bind, video_dev, serial_dev, baud
    );

    for request in server.incoming_requests() {
        let st = state.clone();
        // one thread per request: captures (v4l2-ctl) and serial transactions
        // are short-lived; the serial port itself is serialized by a mutex.
        thread::spawn(move || {
            let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                handle_request(st, request)
            }));
            if let Err(e) = res {
                eprintln!("usb-mcp: handler panicked: {:?}", e);
            }
        });
    }
}

// ---------------------------------------------------------------- HTTP layer

fn header(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("valid header")
}

fn cors_headers() -> Vec<Header> {
    vec![
        header("Access-Control-Allow-Origin", "*"),
        header("Access-Control-Allow-Headers", "Content-Type, Mcp-Session-Id, Last-Event-ID, Authorization"),
        header("Access-Control-Allow-Methods", "GET, POST, DELETE, OPTIONS"),
        header("Access-Control-Expose-Headers", "Mcp-Session-Id"),
    ]
}

fn reply(req: Request, status: u16, content_type: Option<&str>, body: String, extra: &mut Vec<Header>) {
    extra.extend(cors_headers());
    let mut resp = Response::from_string(body).with_status_code(status);
    if let Some(ct) = content_type {
        let mut ct = ct.to_string();
        if ct == "application/json" {
            ct = "application/json; charset=utf-8".to_string();
        }
        extra.push(header("Content-Type", &ct));
    }
    for h in extra.drain(..) {
        resp = resp.with_header(h);
    }
    let _ = req.respond(resp);
}

fn header_value(req: &Request, name: &str) -> Option<String> {
    let want = name.to_ascii_lowercase();
    req.headers()
        .iter()
        .find(|h| h.field.as_str().to_ascii_lowercase() == want)
        .map(|h| h.value.trim().to_string())
}

fn handle_request(state: Arc<State>, mut req: Request) {
    let path = req.url().split('?').next().unwrap_or("").to_string();
    match req.method() {
        Method::Get if path == "/" => {
            let body = format!(
                "usb-mcp {} — Streamable HTTP MCP endpoint: POST JSON-RPC to /mcp\nvideo: {}\n",
                env!("CARGO_PKG_VERSION"),
                state.video_dev
            );
            reply(req, 200, Some("text/plain; charset=utf-8"), body, &mut vec![]);
        }
        Method::Get if path == "/mcp" => {
            // Streamable HTTP: no server-initiated SSE stream offered → 405 per spec.
            let body = json!({
                "jsonrpc": "2.0",
                "id": null,
                "error": {
                    "code": -32000,
                    "message": "This server does not offer an SSE GET stream. POST JSON-RPC requests to /mcp."
                }
            })
            .to_string();
            reply(req, 405, Some("application/json"), body, &mut vec![]);
        }
        Method::Post if path == "/mcp" || path == "/" => {
            let sid = header_value(&req, "Mcp-Session-Id");
            let mut body = String::new();
            if req.as_reader().read_to_string(&mut body).is_err() || body.len() > 4_000_000 {
                let b = json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"body unreadable or too large"}}).to_string();
                reply(req, 400, Some("application/json"), b, &mut vec![]);
                return;
            }
            let val: Value = match serde_json::from_str(&body) {
                Ok(v) => v,
                Err(e) => {
                    let b = json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":format!("parse error: {}", e)}}).to_string();
                    reply(req, 400, Some("application/json"), b, &mut vec![]);
                    return;
                }
            };
            let mut new_sid: Option<String> = None;
            let mut out_headers: Vec<Header> = Vec::new();

            let (status, resp_body) = if let Some(msgs) = val.as_array() {
                // JSON-RPC batching (legacy; kept for client compatibility)
                let mut responses = Vec::new();
                let mut notify_only = true;
                for m in msgs.iter() {
                    if let Some(r) = process_message(&state, m, sid.as_deref(), &mut new_sid) {
                        responses.push(r);
                        notify_only = false;
                    }
                }
                if notify_only {
                    (202, String::new())
                } else {
                    (200, Value::Array(responses).to_string())
                }
            } else if val.is_object() {
                let r = process_message(&state, &val, sid.as_deref(), &mut new_sid);
                match r {
                    Some(resp) => (200, resp.to_string()),
                    None => (202, String::new()),
                }
            } else {
                (
                    400,
                    json!({"jsonrpc":"2.0","id":null,"error":{"code":-32600,"message":"invalid request"}}).to_string(),
                )
            };
            if let Some(ns) = &new_sid {
                out_headers.push(header("Mcp-Session-Id", ns));
            }
            let ct = if resp_body.is_empty() { None } else { Some("application/json") };
            reply(req, status, ct, resp_body, &mut out_headers);
        }
        Method::Delete if path == "/mcp" => {
            let sid = header_value(&req, "Mcp-Session-Id");
            let removed = match sid {
                Some(s) => state.sessions.lock().unwrap_or_else(|e| e.into_inner()).remove(&s),
                None => false,
            };
            if removed {
                reply(req, 200, Some("application/json"), json!({"jsonrpc":"2.0","id":null,"result":{}}).to_string(), &mut vec![]);
            } else {
                reply(req, 404, Some("application/json"), json!({"jsonrpc":"2.0","id":null,"error":{"code":-32000,"message":"unknown session"}}).to_string(), &mut vec![]);
            }
        }
        Method::Options => {
            reply(req, 204, None, String::new(), &mut vec![]);
        }
        _ => {
            reply(req, 404, Some("text/plain"), "not found (try POST /mcp)".into(), &mut vec![]);
        }
    }
}

// ------------------------------------------------------------ JSON-RPC layer

/// Handle one JSON-RPC message. Returns Some(response-object) or None for
/// notifications (→ HTTP 202). Sets *new_sid on successful initialize.
fn process_message(state: &Arc<State>, msg: &Value, sid: Option<&str>, new_sid: &mut Option<String>) -> Option<Value> {
    let id = msg.get("id").cloned().filter(|v| !v.is_null());
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
    let params = msg.get("params").cloned().unwrap_or_else(|| json!({}));
    let is_notification = id.is_none() || method.starts_with("notifications/");

    if method == "initialize" {
        let requested = params.get("protocolVersion").and_then(Value::as_str).unwrap_or(LATEST_PROTOCOL);
        let version = if SUPPORTED_PROTOCOLS.contains(&requested) { requested } else { LATEST_PROTOCOL };
        let s = new_session_id();
        state.sessions.lock().unwrap_or_else(|e| e.into_inner()).insert(s.clone());
        *new_sid = Some(s);
        return (id.is_some()).then(|| {
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": version,
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": { "name": "usb-mcp", "version": env!("CARGO_PKG_VERSION") },
                    "instructions": INSTRUCTIONS,
                }
            })
        });
    }

    // every other message must carry a valid session id
    let known = sid.map(|s| state.sessions.lock().unwrap_or_else(|e| e.into_inner()).contains(s)).unwrap_or(false);
    if !known {
        return if is_notification {
            None
        } else {
            Some(json!({"jsonrpc":"2.0","id":id,"error":{"code":-32000,"message":"missing or unknown Mcp-Session-Id (send initialize first)"}}))
        };
    }

    if is_notification {
        return None; // initialized / cancelled / progress etc.
    }

    let result: Result<Value, (i64, String)> = match method {
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tool_defs() })),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            if !tool_names().iter().any(|t| *t == name) {
                Err((-32602, format!("unknown tool: {}", name)))
            } else {
                let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
                match call_tool(state, name, &args) {
                    Ok(content) => Ok(json!({ "content": content, "isError": false })),
                    Err(text) => Ok(json!({ "content": [{ "type": "text", "text": text }], "isError": true })),
                }
            }
        }
        "resources/list" | "prompts/list" => Ok(json!({})), // no resources/prompts
        _ => Err((-32601, format!("method not found: {}", method))),
    };

    Some(match result {
        Ok(r) => json!({"jsonrpc":"2.0","id":id,"result":r}),
        Err((code, message)) => json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}}),
    })
}

fn new_session_id() -> String {
    let mut b = [0u8; 16];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = f.read_exact(&mut b);
    } else {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        for (i, byte) in t.to_le_bytes().iter().enumerate() {
            b[i] = *byte;
            b[i + 8] = *byte;
        }
    }
    b.iter().map(|x| format!("{:02x}", x)).collect()
}

// ------------------------------------------------------------- tool schemas

fn tool_names() -> Vec<&'static str> {
    vec![
        "capture_info",
        "capture_frame",
        "hid_status",
        "hid_key_press",
        "hid_type_text",
        "hid_release_keys",
        "hid_media_key",
        "hid_mouse_move",
        "hid_mouse_click",
        "hid_mouse_release",
        "hid_mouse_scroll",
        "hid_mouse_abs_move",
    ]
}

fn tool_defs() -> Value {
    json!([
      {
        "name": "capture_info",
        "description": "List UVC capture devices only (driver uvcvideo; platform nodes like bcm2835-codec/isp are filtered out) and supported capture formats/resolutions for the requested device (v4l2-ctl --info scan / --list-formats-ext).",
        "inputSchema": { "type": "object", "properties": { "device": { "type": "string", "description": "video device path, default /dev/video0" } } }
      },
      {
        "name": "capture_frame",
        "description": "Grab ONE frame from the UVC capture (the target PC's HDMI) from raw YUYV (always warmup-discarded first). mode=image (default): lossless compressed PNG image block (a few hundred KB), best for pixel-diff/text legibility. mode=diff: compares the grab with the PREVIOUS capture on the same device and returns ONLY text change stats (changed % + bounding box in frame px, no image) — a cheap 'did the screen move?' check between HID actions. mode=both: stats + PNG. Every capture stores its full frame as the new baseline (in-memory; resets on restart or dimension change). Optional region {x,y,w,h} crops the PNG to that rect and scopes diff stats to it. NOTE: the device MJPEG path was removed 2026-11-08 (unstable headers / stale no-signal frames); PNG remains the only image output.",
        "inputSchema": {
          "type": "object",
          "properties": {
            "width": { "type": "integer", "default": 1280 },
            "height": { "type": "integer", "default": 720 },
            "skip": { "type": "integer", "default": 2, "description": "discard N initial frames before the real grab (first buffers of a fresh YUYV stream come out unsettled on this device)" },
            "device": { "type": "string" },
            "mode": { "type": "string", "enum": ["image", "diff", "both"], "default": "image", "description": "image: PNG only; diff: change stats vs previous capture, NO image; both: stats + PNG" },
            "region": { "type": "object", "properties": { "x": { "type": "integer", "minimum": 0 }, "y": { "type": "integer", "minimum": 0 }, "w": { "type": "integer", "minimum": 1 }, "h": { "type": "integer", "minimum": 1 } }, "required": ["x", "y", "w", "h"], "description": "crop rect in frame px (origin outside the frame errors, size overruns clamp); also scopes diff-mode stats" },
            "diff_threshold": { "type": "integer", "default": 12, "minimum": 0, "maximum": 255, "description": "min per-component abs change on the raw YUYV bytes to count as a changed pixel (live HDMI has a few LSBs of noise; 0 = bit-exact)" }
          }
        }
      },
      {
        "name": "hid_status",
        "description": "CH9329 GET_INFO: firmware version + whether the TARGET PC currently enumerates the injected HID device (usb_status=0x01). First call in any session to check the link.",
        "inputSchema": { "type": "object", "properties": {} }
      },
      {
        "name": "hid_key_press",
        "description": "Press a key combination, hold it, then release ALL keys. Names: a-z 0-9 symbols, Enter Esc Tab Space Backspace Delete Insert Home End PgUp PgDn Up Down Left Right F1-F12 CapsLock NumLock App, kp_*, or raw '0xNN' usage ids. Modifiers: ctrl shift alt win (+ r- variants).",
        "inputSchema": {
          "type": "object",
          "properties": {
            "keys": { "type": "array", "items": { "type": "string" }, "maxItems": 6, "description": "keys to press simultaneously (max 6)" },
            "modifiers": { "type": "array", "items": { "type": "string" }, "default": [] },
            "hold_ms": { "type": "integer", "default": 100, "minimum": 10, "maximum": 5000 }
          },
          "required": ["keys"]
        }
      },
      {
        "name": "hid_type_text",
        "description": "Type a string into the target PC's focused window (US layout ASCII; shift handled automatically; \\n = Enter).",
        "inputSchema": {
          "type": "object",
          "properties": {
            "text": { "type": "string", "maxLength": 2048 },
            "hold_ms": { "type": "integer", "default": 60, "minimum": 10, "maximum": 1000 },
            "interval_ms": { "type": "integer", "default": 40, "minimum": 0, "maximum": 1000 }
          },
          "required": ["text"]
        }
      },
      {
        "name": "hid_release_keys",
        "description": "Send the all-keys-release keyboard report (clears stuck keys — safe no-op otherwise).",
        "inputSchema": { "type": "object", "properties": {} }
      },
      {
        "name": "hid_media_key",
        "description": "Consumer/media keys (CMD 0x03 report 0x02): volup voldown mute play next prev stop eject www_back www_forward www_stop refresh favorites search mail rewind record minimize my_computer screensaver save calculator www_home. ACPI power/sleep are deliberately not exposed.",
        "inputSchema": {
          "type": "object",
          "properties": {
            "key": { "type": "string" },
            "action": { "type": "string", "enum": ["click", "press", "release"], "default": "click" }
          },
          "required": ["key"]
        }
      },
      {
        "name": "hid_mouse_move",
        "description": "Move the target pointer RELATIVELY (recommended; absolute placement is unreliable on the target Cinnamon session). Large deltas are split into <=127px frames.",
        "inputSchema": {
          "type": "object",
          "properties": {
            "dx": { "type": "integer", "minimum": -4096, "maximum": 4096 },
            "dy": { "type": "integer", "minimum": -4096, "maximum": 4096 }
          }
        }
      },
      {
        "name": "hid_mouse_click",
        "description": "Mouse button action (left/right/middle) via relative frames. action=click (default) presses and releases within this call after hold_ms; action=press holds the button DOWN across tool calls (drag/long-press) until action=release on the same button or hid_mouse_release; action=release lifts that button. While a button is held, hid_mouse_move/hid_mouse_scroll keep it pressed. Use hid_mouse_move + captures first to aim at the visible element.",
        "inputSchema": {
          "type": "object",
          "properties": {
            "button": { "type": "string", "enum": ["left", "right", "middle"], "default": "left" },
            "action": { "type": "string", "enum": ["click", "press", "release"], "default": "click", "description": "click: press+release here; press: hold down across calls; release: lift this button" },
            "double": { "type": "boolean", "default": false, "description": "double-click (only with action=click)" },
            "hold_ms": { "type": "integer", "default": 80, "minimum": 10, "maximum": 3000, "description": "press duration for action=click" }
          }
        }
      },
      {
        "name": "hid_mouse_release",
        "description": "Release ALL held mouse buttons (sends a zero-button frame). Safe no-op when nothing is held; use it to recover after an error or interrupted drag.",
        "inputSchema": { "type": "object", "properties": {} }
      },
      {
        "name": "hid_mouse_scroll",
        "description": "Scroll wheel on the target (positive = up, negative = down; signed 8-bit chunked).",
        "inputSchema": {
          "type": "object",
          "properties": { "amount": { "type": "integer", "minimum": -50, "maximum": 50 } },
          "required": ["amount"]
        }
      },
      {
        "name": "hid_mouse_abs_move",
        "description": "Absolute pointer placement via CMD 0x04 (moves ONLY; buttons on absolute frames never reach the host). WARNING: observed unreliable on the target (pointer lands elsewhere — AGENTS 2026-11-07). Prefer hid_mouse_move.",
        "inputSchema": {
          "type": "object",
          "properties": {
            "x": { "type": "integer", "minimum": 0 },
            "y": { "type": "integer", "minimum": 0 },
            "screen_w": { "type": "integer", "default": 1280 },
            "screen_h": { "type": "integer", "default": 720 }
          },
          "required": ["x", "y"]
        }
      }
    ])
}

// ------------------------------------------------------------- tool impls

fn lock_hid(state: &State) -> std::sync::MutexGuard<'_, ch9329::Ch9329> {
    state.hid.lock().unwrap_or_else(|e| e.into_inner())
}

fn int_arg(args: &Value, name: &str, default: i64) -> Result<i64, String> {
    match args.get(name) {
        None | Some(Value::Null) => Ok(default),
        Some(Value::Number(n)) => n.as_i64().ok_or_else(|| format!("{} must be an integer", name)),
        Some(Value::String(s)) => s.parse::<i64>().map_err(|_| format!("{} must be an integer", name)),
        Some(_) => Err(format!("{} must be an integer", name)),
    }
}

fn str_arg<'a>(args: &'a Value, name: &str, default: &'a str) -> &'a str {
    args.get(name).and_then(Value::as_str).filter(|s| !s.is_empty()).unwrap_or(default)
}

/// Parse capture_frame's optional region {x,y,w,h} arg (raw, pre-clamp).
fn region_arg(args: &Value) -> Result<Option<(u32, u32, u32, u32)>, String> {
    let r = match args.get("region") {
        None | Some(Value::Null) => return Ok(None),
        Some(v @ Value::Object(_)) => v,
        Some(_) => return Err("region must be an object {x, y, w, h}".into()),
    };
    let mut vals = [0i64; 4];
    for (i, k) in ["x", "y", "w", "h"].iter().enumerate() {
        match r.get(k) {
            Some(Value::Number(n)) => {
                vals[i] = n.as_i64().ok_or_else(|| format!("region.{} must be an integer", k))?
            }
            Some(Value::String(s)) => {
                vals[i] = s.parse::<i64>().map_err(|_| format!("region.{} must be an integer", k))?
            }
            None => return Err(format!("region.{} is required", k)),
            Some(_) => return Err(format!("region.{} must be an integer", k)),
        }
    }
    let (x, y, w, h) = (vals[0], vals[1], vals[2], vals[3]);
    if x < 0 || y < 0 {
        return Err("region.x/y must be >= 0".into());
    }
    if w < 1 || h < 1 {
        return Err("region.w/h must be >= 1".into());
    }
    Ok(Some((x as u32, y as u32, w as u32, h as u32)))
}

fn sleep_ms(ms: u64) {
    if ms > 0 {
        thread::sleep(Duration::from_millis(ms));
    }
}

/// Returns MCP content blocks on success, or Err(text) reported as isError:true.
fn call_tool(state: &Arc<State>, name: &str, args: &Value) -> Result<Vec<Value>, String> {
    match name {
        "capture_info" => {
            let dev = str_arg(args, "device", &state.video_dev).to_string();
            Ok(vec![text_block(capture::info(&dev))])
        }
        "capture_frame" => {
            let w = int_arg(args, "width", 1280)? as u32;
            let h = int_arg(args, "height", 720)? as u32;
            let skip = int_arg(args, "skip", 2)?.max(0) as u32;
            // The 'format' knob went away with the MJPEG path on 2026-11-08.
            // A cached-schema client may still send it: 'png' is accepted as a
            // no-op, jpeg/jpg gets an explicit error rather than silent success.
            if let Some(f) = args.get("format").and_then(Value::as_str) {
                if !f.eq_ignore_ascii_case("png") {
                    return Err(format!(
                        "format '{}' is no longer available: the device MJPEG capture was removed (unstable headers, stale no-signal frames). capture_frame image output is always a lossless PNG built from raw YUYV",
                        f
                    ));
                }
            }
            let mode = match str_arg(args, "mode", "image").to_ascii_lowercase().as_str() {
                "image" => "image",
                "diff" => "diff",
                "both" => "both",
                other => {
                    return Err(format!(
                        "mode '{}' is invalid: use image (default, PNG only), diff (change stats, no image) or both",
                        other
                    ))
                }
            };
            let thresh = int_arg(args, "diff_threshold", 12)?.clamp(0, 255) as i32;
            let region = region_arg(args)?;
            let dev = str_arg(args, "device", &state.video_dev).to_string();
            let fr = capture::grab_yuyv(&dev, w, h, skip)?;
            let region = png::normalize_region(region, fr.w, fr.h)?;

            // Baseline bookkeeping: take the device's previous full frame out
            // of the map, diff the current grab against it (diff/both modes),
            // and finally store the CURRENT frame as the new baseline (any
            // mode) — diffs always chain to the previously seen capture.
            let prev = state.baselines.take(&dev);
            let diff_note = if mode == "image" {
                None
            } else {
                match prev.as_ref() {
                    None => Some(String::from(
                        "no baseline yet for this device — this grab was stored as the baseline, diff stats come from the NEXT capture (use mode=both to also see this frame)",
                    )),
                    Some(p) if p.w != fr.w || p.h != fr.h => Some(format!(
                        "baseline reset: previous capture was {}x{}, this one is {}x{} — no diff this time",
                        p.w, p.h, fr.w, fr.h
                    )),
                    Some(p) => {
                        let st = diff::diff_yuyv(p, &fr.data, region, thresh)?;
                        let window = match region {
                            Some((rx, ry, rw, rh)) => format!("{},{} {}x{}", rx, ry, rw, rh),
                            None => format!("whole frame {}x{}", fr.w, fr.h),
                        };
                        Some(if st.changed == 0 {
                            format!(
                                "NO change in {} ({} px compared, threshold={})",
                                window, st.total, thresh
                            )
                        } else {
                            let (bx, by, bw, bh) = st.bbox.unwrap();
                            format!(
                                "CHANGED: {} of {} px ({:.2}%) in {} — bbox x={}, y={}, {}x{} (threshold={})",
                                st.changed,
                                st.total,
                                100.0 * st.changed as f64 / st.total as f64,
                                window,
                                bx, by, bw, bh,
                                thresh
                            )
                        })
                    }
                }
            };

            let (png_data, cw, ch) = if mode == "diff" {
                (None, fr.w, fr.h)
            } else {
                // RGB conversion only on the image path (Pi Zero: skip it for diff-only)
                let rgb = png::yuyv_to_rgb(&fr.data, fr.w, fr.h)?;
                let (rgb, cw, ch) = match region {
                    Some((rx, ry, rw, rh)) => png::crop_rgb(&rgb, fr.w, fr.h, rx, ry, rw, rh)?,
                    None => (rgb, fr.w, fr.h),
                };
                (Some(png::encode_rgb_png(cw, ch, &rgb)?), cw, ch)
            };

            // the current full frame is now this device's baseline (moved in, no copy)
            state.baselines.put(&dev, diff::SavedFrame { w: fr.w, h: fr.h, yuyv: fr.data });

            let adj = if (fr.w, fr.h) != (w, h) { format!(" [device adjusted {}x{} -> {}x{}]", w, h, fr.w, fr.h) } else { String::new() };
            let region_desc = match region {
                Some((rx, ry, rw, rh)) => format!(" region x={}, y={}, {}x{}", rx, ry, rw, rh),
                None => String::new(),
            };
            let head = match &png_data {
                Some(png_data) => format!(
                    "captured {}x{} lossless PNG (from raw YUYV){}{}, {} bytes from {}",
                    cw, ch, adj, region_desc, png_data.len(), dev
                ),
                None => format!(
                    "captured {}x{} raw YUYV{} (diff-only mode, no image sent) from {}",
                    fr.w, fr.h, adj, dev
                ),
            };
            let text = match diff_note {
                Some(d) => format!("{} | diff vs previous capture: {}", head, d),
                None => head,
            };
            let mut blocks = vec![text_block(text)];
            if let Some(png_data) = png_data {
                blocks.push(json!({ "type": "image", "data": b64::encode(&png_data), "mimeType": "image/png" }));
            }
            Ok(blocks)
        }
        "hid_status" => {
            let mut hid = lock_hid(state);
            let info = hid.get_info()?;
            let mut s = info.summary();
            if info.usb_status == 0 {
                s.push_str("\nHID injection is currently NOT possible: CH9329's USB side is not enumerated by the target.");
            }
            if hid.held_buttons() != 0 {
                s.push_str(&format!(
                    "\nmouse buttons currently HELD: {} (release with hid_mouse_click action=release or hid_mouse_release)",
                    keys::mouse_buttons_desc(hid.held_buttons())
                ));
            }
            Ok(vec![text_block(s)])
        }
        "hid_key_press" => {
            let keys_v = args.get("keys").and_then(Value::as_array).cloned().unwrap_or_default();
            let mods_v = args.get("modifiers").and_then(Value::as_array).cloned().unwrap_or_default();
            let hold = int_arg(args, "hold_ms", 100)?.clamp(10, 5000) as u64;
            let mut modifiers: u8 = 0;
            for m in &mods_v {
                let m = m.as_str().ok_or("modifiers must be strings")?;
                modifiers |= keys::modifier_bit(m).ok_or_else(|| format!("unknown modifier: {}", m))?;
            }
            if keys_v.len() > 6 {
                return Err("USB HID keyboards report at most 6 simultaneous keys".into());
            }
            let mut usages: Vec<u8> = Vec::new();
            let mut shift = modifiers;
            for k in &keys_v {
                let k = k.as_str().ok_or("keys must be strings")?;
                let ki = keys::key_name(k).ok_or_else(|| format!("unknown key name: {}", k))?;
                usages.push(ki.usage);
                if ki.shift {
                    shift |= 0x02;
                }
            }
            if usages.is_empty() && mods_v.is_empty() {
                return Err("give at least one key or modifier".into());
            }
            let mut hid = lock_hid(state);
            hid.kb_report(shift, &usages)?;
            sleep_ms(hold);
            if let Err(e) = hid.release_all_keys() {
                return Err(format!("press succeeded but RELEASE failed — keys may be stuck: {} (call hid_release_keys)", e));
            }
            Ok(vec![text_block(format!(
                "keys=[{}] mods={:#010b} held {}ms",
                keys_v.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("+"),
                shift,
                hold
            ))])
        }
        "hid_type_text" => {
            let text = args.get("text").and_then(Value::as_str).ok_or("text is required")?;
            if text.len() > 2048 {
                return Err("text too long (max 2048 bytes)".into());
            }
            let hold = int_arg(args, "hold_ms", 60)?.clamp(10, 1000) as u64;
            let gap = int_arg(args, "interval_ms", 40)?.clamp(0, 1000) as u64;
            let chars: Vec<(u8, bool)> = {
                let mut out = Vec::with_capacity(text.len());
                let mut bad: Vec<char> = Vec::new();
                for c in text.chars() {
                    match keys::char_key(c) {
                        Some(k) => out.push((k.usage, k.shift)),
                        None => bad.push(c),
                    }
                }
                if !bad.is_empty() {
                    return Err(format!(
                        "unsupported characters (US-layout ASCII only, no dead keys): {:?} — nothing was typed",
                        bad
                    ));
                }
                out
            };
            let mut hid = lock_hid(state);
            let mut fails = 0usize;
            for (i, (usage, sh)) in chars.iter().enumerate() {
                let mods = if *sh { 0x02 } else { 0x00 };
                if let Err(e) = hid.kb_report(mods, &[*usage]) {
                    fails += 1;
                    // try to recover for the rest of the string
                    let _ = hid.release_all_keys();
                    if fails == 1 {
                        eprintln!("usb-mcp: type_text char {} failed: {}", i, e);
                    }
                }
                sleep_ms(hold);
                hid.release_all_keys().ok();
                sleep_ms(gap);
            }
            if fails > 0 {
                return Err(format!("typed {} of {} chars ({} failures) — check wiring/status", chars.len() - fails, chars.len(), fails));
            }
            Ok(vec![text_block(format!("typed {} chars", chars.len()))])
        }
        "hid_release_keys" => {
            let mut hid = lock_hid(state);
            hid.release_all_keys()?;
            Ok(vec![text_block("all keyboard keys released")])
        }
        "hid_media_key" => {
            let key = args.get("key").and_then(Value::as_str).ok_or("key is required")?;
            let action = str_arg(args, "action", "click");
            let (idx, bit) = keys::media_bit(key).ok_or_else(|| format!("unknown media key: {}", key))?;
            let mut b = [0u8; 3];
            b[idx - 1] = bit;
            let mut hid = lock_hid(state);
            match action {
                "press" => hid.media(b[0], b[1], b[2])?,
                "release" => hid.media(0, 0, 0)?,
                _ => {
                    hid.media(b[0], b[1], b[2])?;
                    sleep_ms(80);
                    hid.media(0, 0, 0)?;
                }
            }
            Ok(vec![text_block(format!("media key {} ({})", key, action))])
        }
        "hid_mouse_move" => {
            let mut dx = int_arg(args, "dx", 0)?.clamp(-4096, 4096) as i32;
            let mut dy = int_arg(args, "dy", 0)?.clamp(-4096, 4096) as i32;
            if dx == 0 && dy == 0 {
                return Err("dx/dy both zero — nothing to do".into());
            }
            let mut hid = lock_hid(state);
            let mut frames = 0usize;
            while dx != 0 || dy != 0 {
                let sx = dx.clamp(-127, 127) as i8;
                let sy = dy.clamp(-127, 127) as i8;
                hid.mouse_move_rel(sx, sy)?;
                dx -= sx as i32;
                dy -= sy as i32;
                frames += 1;
                sleep_ms(10);
            }
            let held = hid.held_buttons();
            Ok(vec![text_block(format!(
                "relative move ({},{}) in {} frame(s){}",
                int_arg(args, "dx", 0)?,
                int_arg(args, "dy", 0)?,
                frames,
                if held != 0 { format!(" [holding {}]", keys::mouse_buttons_desc(held)) } else { String::new() }
            ))])
        }
        "hid_mouse_click" => {
            let button = str_arg(args, "button", "left");
            let bit = keys::mouse_button_bit(button).ok_or_else(|| format!("unknown button: {}", button))?;
            let action = str_arg(args, "action", "click");
            let double = args.get("double").and_then(Value::as_bool).unwrap_or(false);
            let hold = int_arg(args, "hold_ms", 80)?.clamp(10, 3000) as u64;
            let mut hid = lock_hid(state);
            match action {
                "press" => {
                    hid.mouse_press(bit)?;
                    Ok(vec![text_block(format!(
                        "{} button pressed and HELD across calls (currently held: {}) — release with hid_mouse_click action=release or hid_mouse_release",
                        button,
                        keys::mouse_buttons_desc(hid.held_buttons())
                    ))])
                }
                "release" => {
                    hid.mouse_release(bit)?;
                    Ok(vec![text_block(format!(
                        "{} button released (currently held: {})",
                        button,
                        keys::mouse_buttons_desc(hid.held_buttons())
                    ))])
                }
                "click" => {
                    let cycles = if double { 2 } else { 1 };
                    for _ in 0..cycles {
                        hid.mouse_press(bit)?;
                        sleep_ms(hold);
                        hid.mouse_release(bit)?;
                        sleep_ms(120);
                    }
                    Ok(vec![text_block(format!("{} {}click", button, if double { "double-" } else { "" }))])
                }
                other => Err(format!(
                    "action '{}' is invalid: use click (default), press (hold across calls) or release",
                    other
                )),
            }
        }
        "hid_mouse_release" => {
            let mut hid = lock_hid(state);
            hid.mouse_release_all()?;
            Ok(vec![text_block("all mouse buttons released")])
        }
        "hid_mouse_scroll" => {
            let mut amount = int_arg(args, "amount", 0)?.clamp(-50, 50) as i32;
            if amount == 0 {
                return Err("amount is 0".into());
            }
            let mut hid = lock_hid(state);
            let mut frames = 0;
            while amount != 0 {
                let step = amount.clamp(-127, 127) as i8;
                hid.mouse_scroll_rel(step)?;
                amount -= step as i32;
                frames += 1;
                sleep_ms(20);
            }
            let held = hid.held_buttons();
            Ok(vec![text_block(format!(
                "scroll {} ticks in {} frame(s){}",
                int_arg(args, "amount", 0)?,
                frames,
                if held != 0 { format!(" [holding {}]", keys::mouse_buttons_desc(held)) } else { String::new() }
            ))])
        }
        "hid_mouse_abs_move" => {
            let x = int_arg(args, "x", 0)?.clamp(0, 10000) as u32;
            let y = int_arg(args, "y", 0)?.clamp(0, 10000) as u32;
            let sw = int_arg(args, "screen_w", 1280)?.clamp(1, 10000) as u32;
            let sh = int_arg(args, "screen_h", 720)?.clamp(1, 10000) as u32;
            let rx = ((x * 4096) / sw).min(4095) as u16;
            let ry = ((y * 4096) / sh).min(4095) as u16;
            let mut hid = lock_hid(state);
            hid.mouse_abs_move(rx, ry)?;
            Ok(vec![text_block(format!(
                "absolute move: pixel ({},{}) of {}x{} -> raw ({},{})",
                x, y, sw, sh, rx, ry
            ))])
        }
        _ => Err(format!("unknown tool: {}", name)),
    }
}

fn text_block(t: impl Into<String>) -> Value {
    json!({ "type": "text", "text": t.into() })
}
