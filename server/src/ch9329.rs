//! CH9329 UART→USB-HID driver.
//! Protocol per third-party/wch/ch9329-spec.md (verified on firmware V1.4, 2026-11-07).
//!
//! Safety notes baked into this driver:
//! - CMD 0x07 / 0x0D / 0x0E and any CMD > 0x0F are REFUSED: on V1.4 firmware
//!   these are hidden ISP/bootloader commands that make the USB side vanish
//!   (recovery requires physically unplugging the CH9329).
//! - Buttons/wheel only take effect via relative-mouse frames (CMD 0x05);
//!   absolute frames (CMD 0x04) are move-only (spec §13).

use std::ffi::CString;
use std::os::unix::io::{AsRawFd, FromRawFd};
use std::time::{Duration, Instant};

/// Commands that must never be sent (see header comment).
const FORBIDDEN: &[u8] = &[0x07, 0x0D, 0x0E];

/// Wraparound sum of all bytes (HEAD..DATA), == the trailing SUM byte.
pub fn checksum(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0u8, |a, b| a.wrapping_add(*b))
}

/// Build one frame: 57 AB ADDR CMD LEN DATA… SUM
pub fn build_frame(addr: u8, cmd: u8, data: &[u8]) -> Vec<u8> {
    let mut f = Vec::with_capacity(6 + data.len());
    f.push(0x57);
    f.push(0xAB);
    f.push(addr);
    f.push(cmd);
    f.push(data.len() as u8);
    f.extend_from_slice(data);
    let s = checksum(&f);
    f.push(s);
    f
}

#[derive(Debug)]
struct RawFrame {
    cmd: u8,
    data: Vec<u8>,
}

pub struct Ch9329 {
    path: String,
    baud: u32,
    file: Option<std::fs::File>,
    /// Mouse button bits currently held down (bit0 left / bit1 right / bit2 middle).
    /// Every relative-mouse frame carries the full button state, so a move or
    /// scroll must include these bits or it would implicitly release them.
    held_buttons: u8,
}

fn errno() -> i32 {
    unsafe { *libc::__errno_location() }
}

fn err_str() -> String {
    std::io::Error::from_raw_os_error(errno()).to_string()
}

impl Ch9329 {
    pub fn new(path: String, baud: u32) -> Self {
        Self { path, baud, file: None, held_buttons: 0 }
    }

    fn fd(&self) -> Option<i32> {
        self.file.as_ref().map(|f| f.as_raw_fd())
    }

    fn close_port(&mut self) {
        self.file = None;
    }

    fn open_port(&mut self) -> Result<i32, String> {
        if let Some(fd) = self.fd() {
            return Ok(fd);
        }
        let cpath = CString::new(self.path.as_str())
            .map_err(|_| format!("invalid device path: {}", self.path))?;
        let fd = unsafe { libc::open(cpath.as_ptr(), libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC) };
        if fd < 0 {
            return Err(format!("open {}: {}", self.path, err_str()));
        }
        let file = unsafe { std::fs::File::from_raw_fd(fd) };

        let speed = match self.baud {
            1200 => libc::B1200,
            2400 => libc::B2400,
            4800 => libc::B4800,
            9600 => libc::B9600,
            19200 => libc::B19200,
            38400 => libc::B38400,
            57600 => libc::B57600,
            115200 => libc::B115200,
            _ => libc::B9600,
        };
        unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(fd, &mut t) != 0 {
                self.file = Some(file);
                self.close_port();
                return Err(format!("tcgetattr {}: {}", self.path, err_str()));
            }
            libc::cfmakeraw(&mut t);
            libc::cfsetspeed(&mut t, speed);
            t.c_cflag |= libc::CLOCAL | libc::CREAD | libc::CS8;
            t.c_cflag &= !(libc::PARENB | libc::CSTOPB | libc::CRTSCTS);
            t.c_iflag &= !(libc::IXON | libc::IXOFF | libc::ISTRIP | libc::ICRNL | libc::BRKINT | libc::INLCR);
            t.c_oflag &= !libc::OPOST;
            t.c_lflag &= !(libc::ECHO | libc::ECHOE | libc::ECHONL | libc::ICANON | libc::ISIG | libc::IEXTEN);
            t.c_cc[libc::VMIN as usize] = 0;
            t.c_cc[libc::VTIME as usize] = 0;
            if libc::tcsetattr(fd, libc::TCSANOW, &t) != 0 {
                self.file = Some(file);
                self.close_port();
                return Err(format!("tcsetattr {}: {}", self.path, err_str()));
            }
            libc::tcflush(fd, libc::TCIOFLUSH);
        }
        self.file = Some(file);
        Ok(fd)
    }

    /// Send one frame (single write, per spec §3) and wait ≤500 ms for its ACK.
    fn send_cmd(&mut self, cmd: u8, data: &[u8]) -> Result<Vec<u8>, String> {
        if FORBIDDEN.contains(&cmd) || cmd > 0x0F {
            return Err(format!(
                "refused CMD 0x{:02X}: not in the official CH9329 command set \
                 (0x07/0x0D/0x0E switch V1.4 firmware to bootloader and kill the USB side)",
                cmd
            ));
        }
        let fd = self.open_port()?;
        let frame = build_frame(0x00, cmd, data);

        let mut off = 0usize;
        while off < frame.len() {
            let n = unsafe {
                libc::write(
                    fd,
                    frame[off..].as_ptr() as *const libc::c_void,
                    frame.len() - off,
                )
            };
            if n < 0 {
                let e = errno();
                if e == libc::EINTR {
                    continue;
                }
                self.close_port();
                return Err(format!("write {}: {}", self.path, err_str()));
            }
            off += n as usize;
        }

        match self.wait_frame(cmd | 0x80, cmd | 0xC0)? {
            Some(f) if (f.cmd & 0x40) == 0 => Ok(f.data), // normal ACK: CMD|0x80
            Some(f) => {
                // Error ACK: CMD|0xC0, status byte may be absent (short ACK on V1.4).
                let code = f.data.first().copied();
                Err(format!("CH9329 rejected the frame: {}", status_desc(code)))
            }
            None => Err(format!(
                "no ACK within 500 ms on {} — check UART wiring, baud rate ({}) and CH9329 power",
                self.path, self.baud
            )),
        }
    }

    fn wait_frame(&mut self, want_ok: u8, want_err: u8) -> Result<Option<RawFrame>, String> {
        let fd = match self.fd() {
            Some(fd) => fd,
            None => return Ok(None),
        };
        let deadline = Instant::now() + Duration::from_millis(500);
        let mut buf: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 128];
        loop {
            let remain = deadline.saturating_duration_since(Instant::now());
            if remain.is_zero() {
                return Ok(None);
            }
            let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
            let rc = unsafe { libc::poll(&mut pfd, 1, remain.as_millis().min(1000) as i32) };
            if rc < 0 {
                let e = errno();
                if e == libc::EINTR {
                    continue;
                }
                self.close_port();
                return Err(format!("poll {}: {}", self.path, err_str()));
            }
            if rc == 0 {
                return Ok(None);
            }
            let n = unsafe { libc::read(fd, chunk.as_mut_ptr() as *mut libc::c_void, chunk.len()) };
            if n < 0 {
                let e = errno();
                if e == libc::EAGAIN || e == libc::EWOULDBLOCK || e == libc::EINTR {
                    continue;
                }
                self.close_port();
                return Err(format!("read {}: {}", self.path, err_str()));
            }
            if n == 0 {
                continue;
            }
            buf.extend_from_slice(&chunk[..n as usize]);
            while let Some(f) = extract_frame(&mut buf) {
                if f.cmd == want_ok || f.cmd == want_err {
                    return Ok(Some(f));
                }
                // unsolicited frames (e.g. 0x87 MY_HID reports) are dropped
            }
        }
    }

    // ---- high level operations -------------------------------------------------

    /// GET_INFO (CMD 0x01): firmware version / USB enumeration / LEDs.
    pub fn get_info(&mut self) -> Result<Info, String> {
        let data = self.send_cmd(0x01, &[])?;
        if data.len() < 3 {
            return Err(format!("GET_INFO ACK too short: {:02X?}", data));
        }
        Ok(Info { version: data[0], usb_status: data[1], led: data[2], raw: data })
    }

    /// SEND_KB_GENERAL_DATA (CMD 0x02): modifiers, reserved 0, six usage IDs.
    pub fn kb_report(&mut self, modifiers: u8, keys: &[u8]) -> Result<(), String> {
        let mut data = [0u8; 8];
        data[0] = modifiers;
        for (i, k) in keys.iter().take(6).enumerate() {
            data[2 + i] = *k;
        }
        self.send_cmd(0x02, &data).map(|_| ())
    }

    pub fn release_all_keys(&mut self) -> Result<(), String> {
        self.kb_report(0, &[])
    }

    /// One relative-mouse frame (CMD 0x05): [0x01][buttons][X][Y][wheel].
    /// CH9329 button bits: bit0=left bit1=right bit2=middle.
    /// `held_buttons` is always included, so pressing a button in one tool call
    /// stays down through later move/scroll calls until explicitly released.
    fn send_mouse_frame(&mut self, x: i8, y: i8, wheel: i8) -> Result<(), String> {
        let data = [0x01, self.held_buttons, x as u8, y as u8, wheel as u8];
        self.send_cmd(0x05, &data).map(|_| ())
    }

    /// Send `buttons` as the new held set; on failure keep the previous state
    /// (the target never received the frame, so the old state is still truth).
    fn set_held_buttons(&mut self, buttons: u8) -> Result<(), String> {
        let prev = self.held_buttons;
        self.held_buttons = buttons;
        if let Err(e) = self.send_mouse_frame(0, 0, 0) {
            self.held_buttons = prev;
            return Err(e);
        }
        Ok(())
    }

    /// Relative pointer move, keeping any button held by an earlier call pressed.
    pub fn mouse_move_rel(&mut self, x: i8, y: i8) -> Result<(), String> {
        self.send_mouse_frame(x, y, 0)
    }

    /// Wheel scroll, keeping any button held by an earlier call pressed.
    pub fn mouse_scroll_rel(&mut self, wheel: i8) -> Result<(), String> {
        self.send_mouse_frame(0, 0, wheel)
    }

    /// Press `buttons` (bitmask) and KEEP them pressed across later calls.
    pub fn mouse_press(&mut self, buttons: u8) -> Result<(), String> {
        let next = self.held_buttons | buttons;
        self.set_held_buttons(next)
    }

    /// Release `buttons` (bitmask); any other held buttons stay down.
    pub fn mouse_release(&mut self, buttons: u8) -> Result<(), String> {
        let next = self.held_buttons & !buttons;
        self.set_held_buttons(next)
    }

    /// Release every held button (also clears residual state from a previous
    /// server process; safe no-op when nothing is held).
    pub fn mouse_release_all(&mut self) -> Result<(), String> {
        self.set_held_buttons(0)
    }

    /// Current held-button bitmask (for status output / diagnostics).
    pub fn held_buttons(&self) -> u8 {
        self.held_buttons
    }

    /// Move only via absolute frame (CMD 0x04): [0x02][buttons][Xlo][Xhi][Ylo][Yhi][0].
    /// On this module the button byte never reaches the host (spec §13); it only
    /// mirrors `held_buttons` so this frame cannot implicitly release a held
    /// button on firmware that does track it. Use mouse_press/mouse_release for
    /// button events. The wheel byte is unused by this command.
    pub fn mouse_abs_move(&mut self, x: u16, y: u16) -> Result<(), String> {
        let data = [0x02, self.held_buttons, (x & 0xFF) as u8, (x >> 8) as u8, (y & 0xFF) as u8, (y >> 8) as u8, 0x00];
        self.send_cmd(0x04, &data).map(|_| ())
    }

    /// SEND_KB_MEDIA_DATA (CMD 0x03) report id 0x02 (consumer keys).
    pub fn media(&mut self, b2: u8, b3: u8, b4: u8) -> Result<(), String> {
        self.send_cmd(0x03, &[0x02, b2, b3, b4]).map(|_| ())
    }
}

pub struct Info {
    pub version: u8,
    pub usb_status: u8,
    pub led: u8,
    pub raw: Vec<u8>,
}

impl Info {
    pub fn summary(&self) -> String {
        // spec §6: version byte is the ASCII digit of the minor version (0x34 = '4' → V1.4)
        let ver = if (0x30..=0x39).contains(&self.version) {
            format!("V1.{}", self.version as char)
        } else {
            format!("0x{:02X}", self.version)
        };
        let usb = match self.usb_status {
            0 => "NOT enumerated (CH9329 USB plug not connected / target not accepting HID)",
            1 => "enumerated (target host sees the HID composite device)",
            _ => "unknown",
        };
        format!(
            "CH9329 alive. firmware={} usb_status={:#04X} ({}) led={:#010b}",
            ver, self.usb_status, usb, self.led
        )
    }
}

fn status_desc(code: Option<u8>) -> String {
    match code {
        None => "short error ACK (no status byte; unknown/oversized frame on V1.4)".into(),
        Some(0x00) => "status 0x00 (unexpected 'success' in error ACK)".into(),
        Some(0xE1) => "E1: 1-byte receive timeout".into(),
        Some(0xE2) => "E2: bad header".into(),
        Some(0xE3) => "E3: invalid command".into(),
        Some(0xE4) => "E4: SUM mismatch (frame rejected, nothing was injected)".into(),
        Some(0xE5) => "E5: invalid parameter".into(),
        Some(0xE6) => "E6: frame OK but execution failed".into(),
        Some(c) => format!("unknown status {:#04X}", c).into(),
    }
}

/// Incremental frame parser with header resync; consumes leading garbage itself.
/// Normal ACK: 57 AB ADDR CMD LEN DATA… SUM. V1.4 also emits 5-byte short
/// error/success ACKs `57 AB ADDR CMD SUM` (spec §13); those have a byte at the
/// LEN position > 0x40 that equals the running checksum — unambiguous since
/// real LEN values are ≤ 0x40 and short-ACK checksums are always ≥ 0x80.
/// Returns None while more bytes are still needed.
fn extract_frame(buf: &mut Vec<u8>) -> Option<RawFrame> {
    loop {
        if buf.is_empty() {
            return None;
        }
        if buf[0] != 0x57 {
            buf.remove(0);
            continue;
        }
        if buf.len() == 1 {
            return None;
        }
        if buf[1] != 0xAB {
            buf.drain(..1);
            continue;
        }
        if buf.len() < 5 {
            return None;
        }
        let cmd = buf[3];
        let len = buf[4] as usize;
        if len > 0x40 {
            // candidate short ACK
            if buf[4] == checksum(&buf[..4]) {
                let f = RawFrame { cmd, data: Vec::new() };
                buf.drain(..5);
                return Some(f);
            }
            buf.drain(..2);
            continue;
        }
        let total = 6 + len;
        if buf.len() < total {
            return None;
        }
        if buf[total - 1] == checksum(&buf[..total - 1]) {
            let f = RawFrame { cmd, data: buf[5..5 + len].to_vec() };
            buf.drain(..total);
            return Some(f);
        }
        // checksum invalid: skip past this header and keep scanning
        buf.drain(..2);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{:02x}", x)).collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn official_examples_sum_correctly() {
        // Frames from third-party/wch/ch9329-spec.md §6/§7/§8/§11 (SUM verified on hardware).
        assert_eq!(hex(&build_frame(0, 0x01, &[])), "57 ab 00 01 00 03");
        assert_eq!(
            hex(&build_frame(0, 0x02, &[0, 0, 0x04, 0, 0, 0, 0, 0])),
            "57 ab 00 02 08 00 00 04 00 00 00 00 00 10"
        );
        assert_eq!(
            hex(&build_frame(0, 0x02, &[0, 0, 0, 0, 0, 0, 0, 0])),
            "57 ab 00 02 08 00 00 00 00 00 00 00 00 0c"
        );
        assert_eq!(
            hex(&build_frame(0, 0x02, &[2, 0, 0x04, 0, 0, 0, 0, 0])),
            "57 ab 00 02 08 02 00 04 00 00 00 00 00 12"
        );
        assert_eq!(
            hex(&build_frame(0, 0x02, &[0, 0, 0x28, 0, 0, 0, 0, 0])),
            "57 ab 00 02 08 00 00 28 00 00 00 00 00 34"
        );
        // Alt+Tab (AGENTS.md target injection record)
        assert_eq!(
            hex(&build_frame(0, 0x02, &[4, 0, 0x2B, 0, 0, 0, 0, 0])),
            "57 ab 00 02 08 04 00 2b 00 00 00 00 00 3b"
        );
        assert_eq!(
            hex(&build_frame(0, 0x05, &[1, 1, 0, 0, 0])),
            "57 ab 00 05 05 01 01 00 00 00 0e"
        );
        assert_eq!(
            hex(&build_frame(0, 0x05, &[1, 0, 10, 5, 0])),
            "57 ab 00 05 05 01 00 0a 05 00 1c"
        );
        // absolute move (1280x768 at (100,100)) from spec §8
        assert_eq!(
            hex(&build_frame(0, 0x04, &[2, 0, 0x40, 0x01, 0x15, 0x02, 0])),
            "57 ab 00 04 07 02 00 40 01 15 02 00 67"
        );
        // media mute press/release (spec §7)
        assert_eq!(hex(&build_frame(0, 0x03, &[2, 4, 0, 0])), "57 ab 00 03 04 02 04 00 00 0f");
        assert_eq!(hex(&build_frame(0, 0x03, &[2, 0, 0, 0])), "57 ab 00 03 04 02 00 00 00 0b");
    }

    #[test]
    fn hidden_commands_are_refused() {
        // refusal must happen before any open/write (device path irrelevant)
        let mut h = Ch9329::new("/nonexistent".into(), 9600);
        for cmd in [0x07u8, 0x0D, 0x0E, 0x10, 0x20] {
            let e = h.send_cmd(cmd, &[]).unwrap_err();
            assert!(e.contains("refused"), "cmd {cmd:02X} was not refused: {e}");
        }
    }

    #[test]
    fn held_buttons_roll_back_when_send_fails() {
        // No device: the frame is never delivered, so the state must stay unchanged.
        let mut h = Ch9329::new("/nonexistent".into(), 9600);
        assert!(h.mouse_press(0x01).is_err());
        assert_eq!(h.held_buttons(), 0);
        assert!(h.mouse_release_all().is_err());
        assert_eq!(h.held_buttons(), 0);
    }

    #[test]
    fn parses_normal_and_short_ack() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&[0x00, 0x11, 0x22]); // garbage before header (resync)
        buf.extend_from_slice(&[0x57, 0xAB, 0x00, 0x81, 0x08, 0x34, 0x01, 0x00, 0, 0, 0, 0, 0, 0xC0]);
        let f = extract_frame(&mut buf).expect("good frame");
        assert_eq!(f.cmd, 0x81);
        assert_eq!(f.data, vec![0x34, 0x01, 0x00, 0, 0, 0, 0, 0]);
        // short error ACK from §13 (57 AB 00 87 89)
        buf.extend_from_slice(&[0x57, 0xAB, 0x00, 0x87, 0x89]);
        let f = extract_frame(&mut buf).expect("short ack");
        assert_eq!(f.cmd, 0x87);
        assert!(f.data.is_empty());
        // E4 error ack
        buf.extend_from_slice(&[0x57, 0xAB, 0x00, 0xC2, 0x01, 0xE4, 0xA9]);
        let f = extract_frame(&mut buf).expect("err ack");
        assert_eq!(f.cmd, 0xC2);
        assert_eq!(f.data, vec![0xE4]);
        assert!(buf.is_empty());
    }
}
