//! Primary-display capture for screen sharing, plus a "keep the screen awake"
//! switch for the device doing the viewing. Capture is implemented on Windows
//! (GDI BitBlt); other platforms report that it isn't available yet.

/// Captures the primary monitor, downscales it so the width is at most `max_w`,
/// and returns `(width, height, jpeg_bytes)`.
#[cfg(windows)]
pub fn capture_jpeg(max_w: u32, quality: u8) -> Result<(u32, u32, Vec<u8>), String> {
    let (w, h, bgra) = win::grab()?;
    encode(w, h, &bgra, max_w, quality)
}

#[cfg(not(windows))]
pub fn capture_jpeg(_max_w: u32, _quality: u8) -> Result<(u32, u32, Vec<u8>), String> {
    Err("Screen sharing is only available on Windows in this build".into())
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
    use std::ptr::null_mut;
    use windows_sys::Win32::Graphics::Gdi::{
        BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits, ReleaseDC,
        SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, SRCCOPY,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};

    /// Grabs the whole primary screen as top-down BGRA bytes.
    pub fn grab() -> Result<(u32, u32, Vec<u8>), String> {
        unsafe {
            let screen = GetDC(null_mut());
            if screen.is_null() {
                return Err("Could not read the screen".into());
            }
            let w = GetSystemMetrics(SM_CXSCREEN);
            let h = GetSystemMetrics(SM_CYSCREEN);
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
            let copied = BitBlt(memdc, 0, 0, w, h, screen, 0, 0, SRCCOPY);
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
