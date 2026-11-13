//! Key/character tables (HID Usage Page 0x07, US layout) per third-party/wch/ch9329-spec.md §10.

#[derive(Debug, Clone, Copy)]
pub struct Key {
    pub usage: u8,
    pub shift: bool,
}

/// Printable ASCII → usage+shift.
pub fn char_key(c: char) -> Option<Key> {
    let (usage, shift) = match c {
        ' ' => (0x2C, false),
        '!' => (0x1E, true),
        '@' => (0x1F, true),
        '#' => (0x20, true),
        '$' => (0x21, true),
        '%' => (0x22, true),
        '^' => (0x23, true),
        '&' => (0x24, true),
        '*' => (0x25, true),
        '(' => (0x26, true),
        ')' => (0x27, true),
        '\n' | '\r' => (0x28, false), // Enter
        '\t' => (0x2B, false),        // Tab
        '-' | '_' => (0x2D, c == '_'),
        '=' | '+' => (0x2E, c == '+'),
        '[' | '{' => (0x2F, c == '{'),
        ']' | '}' => (0x30, c == '}'),
        '\\' | '|' => (0x31, c == '|'),
        ';' | ':' => (0x33, c == ':'),
        '\'' | '"' => (0x34, c == '"'),
        ',' | '<' => (0x36, c == '<'),
        '.' | '>' => (0x37, c == '>'),
        '/' | '?' => (0x38, c == '?'),
        '`' | '~' => (0x35, c == '~'),
        '0' => (0x27, false),
        c if c.is_ascii_digit() => (0x1E + (c as u8 - b'1'), false), // '1'=0x1E..'9'=0x26
        c if c.is_ascii_lowercase() => (0x04 + (c as u8 - b'a'), false),
        c if c.is_ascii_uppercase() => (0x04 + (c as u8 - b'A'), true),
        _ => return None,
    };
    // digits are already covered above; keep the digit arm from matching 0
    Some(Key { usage, shift })
}

/// Named keys accepted by hid_key_press (case-insensitive).
pub fn key_name(name: &str) -> Option<Key> {
    // hex escape "0x1E" style passes raw usage ids through
    if let Some(hex) = name.strip_prefix("0x").or_else(|| name.strip_prefix("0X")) {
        return u8::from_str_radix(hex, 16).ok().map(|u| Key { usage: u, shift: false });
    }
    // single char
    let mut chars = name.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        if let Some(k) = char_key(c) {
            return Some(k);
        }
    }
    let n = name.to_ascii_lowercase();
    let usage: u8 = match n.as_str() {
        "enter" | "return" => 0x28,
        "esc" | "escape" => 0x29,
        "backspace" => 0x2A,
        "tab" => 0x2B,
        "space" => 0x2C,
        "capslock" => 0x39,
        "printscreen" | "prtsc" => 0x46,
        "scrolllock" => 0x47,
        "pause" => 0x48,
        "insert" | "ins" => 0x49,
        "home" => 0x4A,
        "pageup" | "pgup" => 0x4B,
        "delete" | "del" => 0x4C,
        "end" => 0x4D,
        "pagedown" | "pgdn" => 0x4E,
        "right" => 0x4F,
        "left" => 0x50,
        "down" => 0x51,
        "up" => 0x52,
        "numlock" => 0x53,
        "app" | "menu" => 0x65,
        "f1" => 0x3A,
        "f2" => 0x3B,
        "f3" => 0x3C,
        "f4" => 0x3D,
        "f5" => 0x3E,
        "f6" => 0x3F,
        "f7" => 0x40,
        "f8" => 0x41,
        "f9" => 0x42,
        "f10" => 0x43,
        "f11" => 0x44,
        "f12" => 0x45,
        // numpad
        "kp_divide" => 0x54, "kp_multiply" => 0x55, "kp_minus" => 0x56,
        "kp_plus" => 0x57, "kp_enter" => 0x58, "kp_1" => 0x59, "kp_2" => 0x5A,
        "kp_3" => 0x5B, "kp_4" => 0x5C, "kp_5" => 0x5D, "kp_6" => 0x5E,
        "kp_7" => 0x5F, "kp_8" => 0x60, "kp_9" => 0x61, "kp_0" => 0x62, "kp_dot" => 0x63,
        _ => return None,
    };
    Some(Key { usage, shift: false })
}

/// Modifier names → HID keyboard report modifier byte (report byte 1 bit map).
pub fn modifier_bit(name: &str) -> Option<u8> {
    let n = name.to_ascii_lowercase();
    Some(match n.as_str() {
        "ctrl" | "control" | "ctrl_l" | "lctrl" => 0x01,
        "shift" | "shift_l" | "lshift" => 0x02,
        "alt" | "meta" | "alt_l" | "lalt" => 0x04,
        "win" | "super" | "gui" | "win_l" | "lwin" => 0x08,
        "rctrl" | "ctrl_r" | "rightctrl" => 0x10,
        "rshift" | "shift_r" | "rightshift" => 0x20,
        "ralt" | "alt_r" | "altgr" | "rightalt" => 0x40,
        "rwin" | "win_r" | "rightwin" | "superr" => 0x80,
        _ => return None,
    })
}

/// Consumer media key names → (byte index within the CMD 0x03 data, bit mask).
/// data = [report_id=0x02, b0, b1, b2]; byte indexes here are 1..3 of `data`.
pub fn media_bit(name: &str) -> Option<(usize, u8)> {
    let n = name.to_ascii_lowercase().replace(['-', ' '], "_");
    Some(match n.as_str() {
        // data byte 1 (spec table row 2)
        "volumeup" | "volup" => (1, 0x01),
        "volumedown" | "voldown" => (1, 0x02),
        "mute" => (1, 0x04),
        "play" | "playpause" | "pause" => (1, 0x08),
        "next" | "nexttrack" => (1, 0x10),
        "prev" | "prevtrack" | "previous" => (1, 0x20),
        "stop" | "cd_stop" => (1, 0x40),
        "eject" => (1, 0x80),
        // data byte 2 (row 3)
        "refresh" => (2, 0x80),
        "www_stop" | "stop2" => (2, 0x40),
        "www_forward" | "wwwforward" => (2, 0x20),
        "www_back" | "wwwback" => (2, 0x10),
        "favorites" | "favorites_link" => (2, 0x08),
        "search" | "search_link" => (2, 0x04),
        "mail" | "email" | "mail_link" => (2, 0x02),
        // data byte 3 (row 4)
        "rewind" => (3, 0x80),
        "record" => (3, 0x40),
        "minimize" => (3, 0x20),
        "my_computer" | "computer" => (3, 0x10),
        "screen_saver" | "screensaver" => (3, 0x08),
        "www_save" | "save" => (3, 0x04),
        "calculator" | "calc" => (3, 0x02),
        "www_expert" | "www_home" | "www" => (3, 0x01),
        _ => return None,
    })
}

/// Mouse button names → CH9329 button byte (bit0 left / bit1 right / bit2 middle).
pub fn mouse_button_bit(name: &str) -> Option<u8> {
    let n = name.to_ascii_lowercase();
    Some(match n.as_str() {
        "left" => 0x01,
        "right" => 0x02,
        "middle" => 0x04,
        _ => return None,
    })
}

/// Human-readable summary of a CH9329 mouse-button bitmask.
pub fn mouse_buttons_desc(bits: u8) -> String {
    let mut names: Vec<&str> = Vec::new();
    if bits & 0x01 != 0 {
        names.push("left");
    }
    if bits & 0x02 != 0 {
        names.push("right");
    }
    if bits & 0x04 != 0 {
        names.push("middle");
    }
    if names.is_empty() {
        "none".into()
    } else {
        names.join("+")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chars_match_spec_table() {
        assert_eq!(char_key('a').unwrap().usage, 0x04);
        assert_eq!(char_key('z').unwrap().usage, 0x1D);
        assert_eq!(char_key('A').unwrap().shift, true);
        assert_eq!(char_key('1').unwrap().usage, 0x1E);
        assert_eq!(char_key('9').unwrap().usage, 0x26);
        assert_eq!(char_key('0').unwrap().usage, 0x27);
        assert_eq!(char_key('-').unwrap().usage, 0x2D);
        assert_eq!(char_key('_').unwrap().usage, 0x2D);
        assert!(char_key('_').unwrap().shift);
        assert_eq!(char_key('/').unwrap().usage, 0x38);
        assert_eq!(char_key('?').unwrap().usage, 0x38);
        assert!(char_key('日').is_none());
    }

    #[test]
    fn names_and_modifiers() {
        assert_eq!(key_name("Enter").unwrap().usage, 0x28);
        assert_eq!(key_name("f5").unwrap().usage, 0x3E);
        assert_eq!(key_name("tab").unwrap().usage, 0x2B);
        assert_eq!(modifier_bit("ctrl").unwrap(), 0x01);
        assert_eq!(modifier_bit("alt").unwrap(), 0x04);
        assert_eq!(modifier_bit("win").unwrap(), 0x08);
        assert_eq!(media_bit("mute").unwrap(), (1, 0x04));
        assert_eq!(media_bit("www_back").unwrap(), (2, 0x10));
        assert_eq!(mouse_button_bit("middle").unwrap(), 0x04);
        assert_eq!(mouse_buttons_desc(0), "none");
        assert_eq!(mouse_buttons_desc(0x01 | 0x04), "left+middle");
    }
}
