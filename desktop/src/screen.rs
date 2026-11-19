//! Display capture for screen sharing (any connected monitor), plus a "keep the
//! screen awake" switch for the device doing the viewing. Capture is implemented on
//! Windows (GDI BitBlt); other platforms report that it isn't available yet.

/// One connected monitor, in desktop pixels (the main monitor's top-left is 0,0).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Monitor {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub primary: bool,
}

/// The connected monitors: the main one first, then the others from left to right.
#[cfg(windows)]
pub fn monitors() -> Vec<Monitor> {
    let mut list = win::monitors();
    order(&mut list);
    list
}

#[cfg(not(windows))]
pub fn monitors() -> Vec<Monitor> {
    Vec::new()
}

fn order(list: &mut [Monitor]) {
    list.sort_by_key(|m| (!m.primary, m.x, m.y));
}

/// Captures monitor number `index` (0 = the main monitor; falls back to it if that
/// monitor is gone), downscales it so the width is at most `max_w`, and returns
/// `(width, height, jpeg_bytes, monitor_used, monitor_count)`.
#[cfg(windows)]
pub fn capture_jpeg(index: usize, max_w: u32, quality: u8) -> Result<(u32, u32, Vec<u8>, usize, usize), String> {
    let list = monitors();
    let (used, rect) = match list.get(index) {
        Some(m) => (index, Some(*m)),
        None => (0, list.first().copied()),
    };
    let (w, h, bgra) = win::grab(rect)?;
    let (tw, th, jpeg) = encode(w, h, &bgra, max_w, quality)?;
    Ok((tw, th, jpeg, used, list.len().max(1)))
}

#[cfg(not(windows))]
pub fn capture_jpeg(_index: usize, _max_w: u32, _quality: u8) -> Result<(u32, u32, Vec<u8>, usize, usize), String> {
    Err("Screen sharing is only available on Windows in this build".into())
}

/// Turns a position on monitor `index` (fractions 0..1 of that monitor) into the
/// 0..65535 coordinates SendInput uses across the whole desktop.
#[cfg(windows)]
pub fn to_desktop(index: usize, x: f64, y: f64) -> (i32, i32) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
    };
    let (vx, vy, vw, vh) = unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN).max(2),
            GetSystemMetrics(SM_CYVIRTUALSCREEN).max(2),
        )
    };
    let list = monitors();
    let m = list.get(index).or(list.first()).copied().unwrap_or(Monitor { x: vx, y: vy, w: vw, h: vh, primary: true });
    let px = m.x as f64 + x * (m.w - 1).max(1) as f64;
    let py = m.y as f64 + y * (m.h - 1).max(1) as f64;
    let nx = ((px - vx as f64) * 65535.0 / (vw - 1) as f64).round() as i32;
    let ny = ((py - vy as f64) * 65535.0 / (vh - 1) as f64).round() as i32;
    (nx.clamp(0, 65535), ny.clamp(0, 65535))
}

/// While `on` is true, stops the display (and system) from going to sleep. Must be
/// called from the thread that should hold the request — on Windows that is the UI
/// thread. Call with `false` to release it.
#[cfg(windows)]
pub fn keep_display_awake(on: bool) {
    use windows_sys::Win32::System::Power::{
        SetThreadExecutionState, ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED,
    };
    unsafe {
        if on {
            SetThreadExecutionState(ES_CONTINUOUS | ES_DISPLAY_REQUIRED | ES_SYSTEM_REQUIRED);
        } else {
            SetThreadExecutionState(ES_CONTINUOUS);
        }
    }
}

#[cfg(not(windows))]
pub fn keep_display_awake(_on: bool) {}

#[cfg(windows)]
fn encode(w: u32, h: u32, bgra: &[u8], max_w: u32, quality: u8) -> Result<(u32, u32, Vec<u8>), String> {
    use image::{codecs::jpeg::JpegEncoder, imageops, ImageBuffer, RgbImage};
    if w == 0 || h == 0 || bgra.len() < (w as usize) * (h as usize) * 4 {
        return Err("Empty screen capture".into());
    }
    // BGRA (top-down) → RGB.
    let pixels = (w as usize) * (h as usize);
    let mut rgb = vec![0u8; pixels * 3];
    for i in 0..pixels {
        rgb[i * 3] = bgra[i * 4 + 2];
        rgb[i * 3 + 1] = bgra[i * 4 + 1];
        rgb[i * 3 + 2] = bgra[i * 4];
    }
    let img: RgbImage = ImageBuffer::from_raw(w, h, rgb).ok_or("Bad screen buffer")?;
    let (tw, th) = if w > max_w {
        let th = ((max_w as u64 * h as u64) / w as u64).max(1) as u32;
        (max_w, th)
    } else {
        (w, h)
    };
    let scaled = if (tw, th) != (w, h) { imageops::resize(&img, tw, th, imageops::FilterType::Triangle) } else { img };
    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, quality).encode_image(&scaled).map_err(|e| e.to_string())?;
    Ok((tw, th, out))
}

#[cfg(windows)]
mod win {
    use super::Monitor;
    use std::ptr::null_mut;
    use windows_sys::Win32::Foundation::{BOOL, LPARAM, RECT};
    use windows_sys::Win32::Graphics::Gdi::{
        BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, EnumDisplayMonitors, GetDC,
        GetDIBits, GetMonitorInfoW, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HDC,
        HMONITOR, MONITORINFO, SRCCOPY,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};

    unsafe extern "system" fn add_monitor(hm: HMONITOR, _dc: HDC, _r: *mut RECT, data: LPARAM) -> BOOL {
        let list = &mut *(data as *mut Vec<Monitor>);
        let mut mi: MONITORINFO = core::mem::zeroed();
        mi.cbSize = core::mem::size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(hm, &mut mi) != 0 {
            let r = mi.rcMonitor;
            list.push(Monitor {
                x: r.left,
                y: r.top,
                w: r.right - r.left,
                h: r.bottom - r.top,
                primary: mi.dwFlags & 1 != 0,
            });
        }
        1
    }

    pub fn monitors() -> Vec<Monitor> {
        let mut list: Vec<Monitor> = Vec::new();
        unsafe {
            EnumDisplayMonitors(
                null_mut(),
                core::ptr::null(),
                Some(add_monitor),
                &mut list as *mut Vec<Monitor> as LPARAM,
            );
        }
        list.retain(|m| m.w > 0 && m.h > 0);
        list
    }

    /// Grabs one monitor (or the whole main screen when `rect` is None) as top-down BGRA bytes.
    pub fn grab(rect: Option<Monitor>) -> Result<(u32, u32, Vec<u8>), String> {
        unsafe {
            let screen = GetDC(null_mut());
            if screen.is_null() {
                return Err("Could not read the screen".into());
            }
            let (x, y, w, h) = match rect {
                Some(m) => (m.x, m.y, m.w, m.h),
                None => (0, 0, GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)),
            };
            if w <= 0 || h <= 0 {
                ReleaseDC(null_mut(), screen);
                return Err("The screen reported no size".into());
            }
            let memdc = CreateCompatibleDC(screen);
            let bmp = CreateCompatibleBitmap(screen, w, h);
            if memdc.is_null() || bmp.is_null() {
                if !bmp.is_null() {
                    DeleteObject(bmp);
                }
                if !memdc.is_null() {
                    DeleteDC(memdc);
                }
                ReleaseDC(null_mut(), screen);
                return Err("Could not allocate a screen buffer".into());
            }
            let old = SelectObject(memdc, bmp);
            let copied = BitBlt(memdc, 0, 0, w, h, screen, x, y, SRCCOPY);
            let mut data = vec![0u8; (w as usize) * (h as usize) * 4];
            let mut bi: BITMAPINFO = core::mem::zeroed();
            bi.bmiHeader.biSize = core::mem::size_of::<BITMAPINFOHEADER>() as u32;
            bi.bmiHeader.biWidth = w;
            bi.bmiHeader.biHeight = -h; // negative → top-down rows
            bi.bmiHeader.biPlanes = 1;
            bi.bmiHeader.biBitCount = 32;
            bi.bmiHeader.biCompression = BI_RGB as u32;
            let scanned = GetDIBits(
                memdc,
                bmp,
                0,
                h as u32,
                data.as_mut_ptr() as *mut core::ffi::c_void,
                &mut bi,
                DIB_RGB_COLORS,
            );
            SelectObject(memdc, old);
            DeleteObject(bmp);
            DeleteDC(memdc);
            ReleaseDC(null_mut(), screen);
            if copied == 0 || scanned == 0 {
                return Err("Copying the screen failed".into());
            }
            Ok((w as u32, h as u32, data))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{order, Monitor};

    #[test]
    fn main_monitor_comes_first_then_left_to_right() {
        let m = |x: i32, primary: bool| Monitor { x, y: 0, w: 1920, h: 1080, primary };
        let mut list = vec![m(1920, false), m(-1920, false), m(0, true)];
        order(&mut list);
        assert_eq!(list.iter().map(|m| m.x).collect::<Vec<_>>(), vec![0, -1920, 1920]);
    }
}
