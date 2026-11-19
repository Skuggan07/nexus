//! Turns remote-control events from a paired viewer into real mouse and keyboard
//! input on this PC. Positions arrive as fractions (0..1) of the shared screen;
//! an optional `m` says which monitor (0 = the main one).
//!
//! Event lines (JSON, field `op`):
//!   move   {x, y}                       – move the pointer
//!   button {b: left|right|middle, down, x?, y?}
//!   click  {b, x?, y?, n?}              – press and release, n times
//!   scroll {dy?, dx?}                   – wheel notches (+ = up / right)
//!   text   {s}                          – type text exactly as given
//!   key    {k, mods?: [ctrl|alt|shift|win]} – press a named key or shortcut
//!
//! Windows only injects into windows of the same or lower privilege, so an
//! elevated window (or the lock/UAC screen) cannot be controlled unless Nexus
//! itself runs as administrator.

#[cfg(windows)]
pub fn apply(v: &serde_json::Value) {
    win::apply(v)
}

#[cfg(not(windows))]
pub fn apply(_v: &serde_json::Value) {}

/// Virtual-key code for a key name, and whether it is an "extended" key.
#[cfg_attr(not(windows), allow(dead_code))]
fn vk(name: &str) -> Option<(u16, bool)> {
    let n = name.to_ascii_lowercase();
    let k = match n.as_str() {
        "enter" | "return" => (0x0D, false),
        "backspace" => (0x08, false),
        "tab" => (0x09, false),
        "escape" | "esc" => (0x1B, false),
        "space" => (0x20, false),
        "left" => (0x25, true),
        "up" => (0x26, true),
        "right" => (0x27, true),
        "down" => (0x28, true),
        "insert" => (0x2D, true),
        "delete" => (0x2E, true),
        "home" => (0x24, true),
        "end" => (0x23, true),
        "pageup" => (0x21, true),
        "pagedown" => (0x22, true),
        "win" => (0x5B, true),
        "ctrl" => (0x11, false),
        "alt" => (0x12, false),
        "shift" => (0x10, false),
        _ => {
            let b = n.as_bytes();
            if b.len() == 1 && b[0].is_ascii_lowercase() {
                (0x41 + (b[0] - b'a') as u16, false)
            } else if b.len() == 1 && b[0].is_ascii_digit() {
                (0x30 + (b[0] - b'0') as u16, false)
            } else if let Some(f) =
                n.strip_prefix('f').and_then(|d| d.parse::<u16>().ok()).filter(|f| (1..=24).contains(f))
            {
                (0x70 + f - 1, false)
            } else {
                return None;
            }
        }
    };
    Some(k)
}

#[cfg(windows)]
mod win {
    use super::vk;
    use serde_json::Value;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{SendInput, INPUT};

    const M_MOVE: u32 = 0x0001;
    const M_LEFTDOWN: u32 = 0x0002;
    const M_LEFTUP: u32 = 0x0004;
    const M_RIGHTDOWN: u32 = 0x0008;
    const M_RIGHTUP: u32 = 0x0010;
    const M_MIDDLEDOWN: u32 = 0x0020;
    const M_MIDDLEUP: u32 = 0x0040;
    const M_WHEEL: u32 = 0x0800;
    const M_HWHEEL: u32 = 0x1000;
    const M_ABSOLUTE: u32 = 0x8000;
    const M_VIRTUALDESK: u32 = 0x4000;
    const K_EXTENDED: u32 = 0x0001;
    const K_UP: u32 = 0x0002;
    const K_UNICODE: u32 = 0x0004;

    fn send(inputs: &[INPUT]) {
        if inputs.is_empty() {
            return;
        }
        unsafe {
            SendInput(inputs.len() as u32, inputs.as_ptr(), std::mem::size_of::<INPUT>() as i32);
        }
    }

    fn mouse(flags: u32, dx: i32, dy: i32, data: i32) -> INPUT {
        unsafe {
            let mut i: INPUT = std::mem::zeroed();
            i.r#type = 0; // INPUT_MOUSE
            i.Anonymous.mi.dx = dx;
            i.Anonymous.mi.dy = dy;
            i.Anonymous.mi.mouseData = data as _;
            i.Anonymous.mi.dwFlags = flags;
            i
        }
    }

    fn key(code: u16, scan: u16, flags: u32) -> INPUT {
        unsafe {
            let mut i: INPUT = std::mem::zeroed();
            i.r#type = 1; // INPUT_KEYBOARD
            i.Anonymous.ki.wVk = code;
            i.Anonymous.ki.wScan = scan;
            i.Anonymous.ki.dwFlags = flags;
            i
        }
    }

    /// Absolute pointer position, if given: x and y are fractions (0..1) of monitor
    /// `m` (0 = the main monitor), placed on the whole desktop.
    fn at(v: &Value) -> Option<INPUT> {
        let x = v.get("x")?.as_f64()?.clamp(0.0, 1.0);
        let y = v.get("y")?.as_f64()?.clamp(0.0, 1.0);
        let m = v.get("m").and_then(|m| m.as_u64()).unwrap_or(0) as usize;
        let (dx, dy) = crate::screen::to_desktop(m, x, y);
        Some(mouse(M_MOVE | M_ABSOLUTE | M_VIRTUALDESK, dx, dy, 0))
    }

    fn buttons(v: &Value) -> (u32, u32) {
        match v.get("b").and_then(|b| b.as_str()) {
            Some("right") => (M_RIGHTDOWN, M_RIGHTUP),
            Some("middle") => (M_MIDDLEDOWN, M_MIDDLEUP),
            _ => (M_LEFTDOWN, M_LEFTUP),
        }
    }

    fn press(list: &mut Vec<INPUT>, name: &str, up: bool) {
        if let Some((code, ext)) = vk(name) {
            let flags = (if ext { K_EXTENDED } else { 0 }) | (if up { K_UP } else { 0 });
            list.push(key(code, 0, flags));
        }
    }

    pub fn apply(v: &Value) {
        let mut list = Vec::new();
        match v.get("op").and_then(|o| o.as_str()).unwrap_or("") {
            "move" => list.extend(at(v)),
            "button" => {
                let (down, up) = buttons(v);
                list.extend(at(v));
                let pressed = v.get("down").and_then(|d| d.as_bool()).unwrap_or(true);
                list.push(mouse(if pressed { down } else { up }, 0, 0, 0));
            }
            "click" => {
                let (down, up) = buttons(v);
                list.extend(at(v));
                for _ in 0..v.get("n").and_then(|n| n.as_u64()).unwrap_or(1).clamp(1, 3) {
                    list.push(mouse(down, 0, 0, 0));
                    list.push(mouse(up, 0, 0, 0));
                }
            }
            "scroll" => {
                list.extend(at(v));
                let dy = v.get("dy").and_then(|d| d.as_f64()).unwrap_or(0.0);
                let dx = v.get("dx").and_then(|d| d.as_f64()).unwrap_or(0.0);
                if dy != 0.0 {
                    list.push(mouse(M_WHEEL, 0, 0, (dy.clamp(-20.0, 20.0) * 120.0).round() as i32));
                }
                if dx != 0.0 {
                    list.push(mouse(M_HWHEEL, 0, 0, (dx.clamp(-20.0, 20.0) * 120.0).round() as i32));
                }
            }
            "text" => {
                let s = v.get("s").and_then(|s| s.as_str()).unwrap_or("");
                for ch in s.chars().take(2000) {
                    if ch == '\n' || ch == '\r' {
                        press(&mut list, "enter", false);
                        press(&mut list, "enter", true);
                        continue;
                    }
                    let mut buf = [0u16; 2];
                    for unit in ch.encode_utf16(&mut buf) {
                        list.push(key(0, *unit, K_UNICODE));
                        list.push(key(0, *unit, K_UNICODE | K_UP));
                    }
                }
            }
            "key" => {
                let Some(name) = v.get("k").and_then(|k| k.as_str()) else { return };
                if vk(name).is_none() {
                    return;
                }
                let mods: Vec<String> = v
                    .get("mods")
                    .and_then(|m| m.as_array())
                    .map(|a| a.iter().filter_map(|x| x.as_str()).map(|s| s.to_ascii_lowercase()).collect())
                    .unwrap_or_default();
                let mods: Vec<&str> =
                    ["ctrl", "alt", "shift", "win"].into_iter().filter(|m| mods.iter().any(|x| x == m)).collect();
                for m in &mods {
                    press(&mut list, m, false);
                }
                press(&mut list, name, false);
                press(&mut list, name, true);
                for m in mods.iter().rev() {
                    press(&mut list, m, true);
                }
            }
            _ => {}
        }
        send(&list);
    }
}

#[cfg(test)]
mod tests {
    use super::vk;

    #[test]
    fn names_map_to_virtual_keys() {
        assert_eq!(vk("Enter"), Some((0x0D, false)));
        assert_eq!(vk("left"), Some((0x25, true)));
        assert_eq!(vk("c"), Some((0x43, false)));
        assert_eq!(vk("7"), Some((0x37, false)));
        assert_eq!(vk("F5"), Some((0x74, false)));
        assert_eq!(vk("win"), Some((0x5B, true)));
        assert_eq!(vk("nope"), None);
        assert_eq!(vk("f30"), None);
    }
}
