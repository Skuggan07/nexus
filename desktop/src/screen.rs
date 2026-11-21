//! Display capture for screen sharing (any connected monitor), plus a "keep the
//! screen awake" switch for the device doing the viewing. Capture is implemented on
//! Windows (Desktop Duplication, falling back to GDI BitBlt); other platforms report
//! that it isn't available yet.

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

/// One picture from a [`Grabber`]: its size, whether it differs from the last one,
/// which monitor it is and how many there are.
pub struct Grabbed {
    pub w: u32,
    pub h: u32,
    pub changed: bool,
    pub mon: usize,
    pub mons: usize,
}

/// Captures one monitor over and over, as fast as Windows allows. It uses Desktop
/// Duplication (the graphics card hands over each new picture, and says when nothing
/// changed) and falls back to the older, slower BitBlt copy when that isn't available
/// (some remote sessions, the lock screen, old drivers).
pub struct Grabber {
    index: usize,
    list: Vec<Monitor>,
    listed: Option<std::time::Instant>,
    /// The latest picture, top-down BGRA.
    pub buf: Vec<u8>,
    w: u32,
    h: u32,
    have: bool,
    #[cfg(windows)]
    fast: Option<dxgi::Dup>,
    #[cfg(windows)]
    fast_off_until: Option<std::time::Instant>,
}

impl Grabber {
    pub fn new(index: usize) -> Self {
        Grabber {
            index,
            list: Vec::new(),
            listed: None,
            buf: Vec::new(),
            w: 0,
            h: 0,
            have: false,
            #[cfg(windows)]
            fast: None,
            #[cfg(windows)]
            fast_off_until: None,
        }
    }

    /// Waits at most `wait` for the next picture of the monitor (into `buf`).
    #[cfg(windows)]
    pub fn grab(&mut self, wait: std::time::Duration) -> Result<Grabbed, String> {
        use std::time::{Duration, Instant};
        if self.listed.map_or(true, |t| t.elapsed() > Duration::from_secs(2)) {
            self.list = monitors();
            self.listed = Some(Instant::now());
        }
        let (used, rect) = match self.list.get(self.index) {
            Some(m) => (self.index, *m),
            None => (0, self.list.first().copied().ok_or("No monitor found")?),
        };
        let mons = self.list.len().max(1);
        if self.fast.as_ref().is_some_and(|d| d.rect != (rect.x, rect.y, rect.w, rect.h)) {
            self.fast = None;
        }
        if self.fast.is_none() && self.fast_off_until.map_or(true, |t| Instant::now() >= t) {
            match dxgi::Dup::open(rect) {
                Ok(d) => self.fast = Some(d),
                Err(_) => self.fast_off_until = Some(Instant::now() + Duration::from_secs(5)),
            }
        }
        if let Some(d) = self.fast.as_mut() {
            match d.next(wait.as_millis().clamp(1, 100) as u32, &mut self.buf) {
                Ok(true) => {
                    (self.w, self.h, self.have) = (d.w, d.h, true);
                    return Ok(Grabbed { w: d.w, h: d.h, changed: true, mon: used, mons });
                }
                Ok(false) if self.have => {
                    return Ok(Grabbed { w: self.w, h: self.h, changed: false, mon: used, mons });
                }
                Ok(false) => {}
                Err(_) => {
                    self.fast = None;
                    self.fast_off_until = Some(Instant::now() + Duration::from_secs(2));
                }
            }
        }
        let (w, h, bgra) = win::grab(Some(rect))?;
        let changed = !self.have || w != self.w || h != self.h || bgra != self.buf;
        (self.buf, self.w, self.h, self.have) = (bgra, w, h, true);
        Ok(Grabbed { w, h, changed, mon: used, mons })
    }

    #[cfg(not(windows))]
    pub fn grab(&mut self, _wait: std::time::Duration) -> Result<Grabbed, String> {
        let _ = (self.index, &self.list, self.listed, self.w, self.h, self.have);
        Err("Screen sharing is only available on Windows in this build".into())
    }
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
/// Packs a BGRA picture as JPEG, first shrinking it so it is at most `max_w` wide.
/// Uses a fast encoder (SIMD) that reads BGRA directly. From quality 85 colours are
/// kept at full resolution, so small coloured text stays sharp.
pub fn encode(w: u32, h: u32, bgra: &[u8], max_w: u32, quality: u8) -> Result<(u32, u32, Vec<u8>), String> {
    if w == 0 || h == 0 || bgra.len() < (w as usize) * (h as usize) * 4 {
        return Err("Empty screen capture".into());
    }
    let (tw, th) = if w > max_w {
        (max_w, ((max_w as u64 * h as u64) / w as u64).max(1) as u32)
    } else {
        (w, h)
    };
    let shrunk;
    let src: &[u8] = if (tw, th) != (w, h) {
        shrunk = shrink(w as usize, h as usize, bgra, tw as usize, th as usize);
        &shrunk
    } else {
        &bgra[..(w as usize) * (h as usize) * 4]
    };
    if tw > u16::MAX as u32 || th > u16::MAX as u32 {
        return Err("The screen is too big".into());
    }
    let mut out = Vec::with_capacity(src.len() / 12);
    let mut enc = jpeg_encoder::Encoder::new(&mut out, quality);
    if quality >= 85 {
        enc.set_sampling_factor(jpeg_encoder::SamplingFactor::F_1_1);
    }
    enc.encode(src, tw as u16, th as u16, jpeg_encoder::ColorType::Bgra).map_err(|e| e.to_string())?;
    Ok((tw, th, out))
}

/// The size a picture is sent at: at most `max_w` wide, keeping its shape, and even
/// in both directions (video needs that).
fn target_size(w: u32, h: u32, max_w: u32) -> (u32, u32) {
    let (tw, th) = if w > max_w { (max_w, ((max_w as u64 * h as u64) / w as u64).max(2) as u32) } else { (w, h) };
    ((tw & !1).max(2), (th & !1).max(2))
}

/// The picture at its sending size (shrunk only when needed).
fn sized(w: u32, h: u32, bgra: &[u8], tw: u32, th: u32) -> std::borrow::Cow<'_, [u8]> {
    if (tw, th) == (w, h) {
        std::borrow::Cow::Borrowed(&bgra[..(w as usize) * (h as usize) * 4])
    } else {
        std::borrow::Cow::Owned(shrink(w as usize, h as usize, bgra, tw as usize, th as usize))
    }
}

/// BGRA to the YUV 4:2:0 layout video uses (Y plane, then U, then V; BT.601).
fn bgra_to_i420(src: &[u8], w: usize, h: usize) -> Vec<u8> {
    let (cw, ch) = (w / 2, h / 2);
    let mut out = vec![0u8; w * h + 2 * cw * ch];
    let (y_plane, uv) = out.split_at_mut(w * h);
    let (u_plane, v_plane) = uv.split_at_mut(cw * ch);
    for (row, y_row) in y_plane.chunks_exact_mut(w).enumerate() {
        let s = &src[row * w * 4..(row + 1) * w * 4];
        for (x, y) in y_row.iter_mut().enumerate() {
            let (b, g, r) = (s[x * 4] as i32, s[x * 4 + 1] as i32, s[x * 4 + 2] as i32);
            *y = (((66 * r + 129 * g + 25 * b + 128) >> 8) + 16) as u8;
        }
    }
    for cy in 0..ch {
        let r0 = &src[(cy * 2) * w * 4..(cy * 2 + 1) * w * 4];
        let r1 = &src[(cy * 2 + 1) * w * 4..(cy * 2 + 2) * w * 4];
        for cx in 0..cw {
            let mut sum = [0i32; 3];
            for p in [&r0[cx * 8..cx * 8 + 8], &r1[cx * 8..cx * 8 + 8]] {
                sum[0] += p[0] as i32 + p[4] as i32;
                sum[1] += p[1] as i32 + p[5] as i32;
                sum[2] += p[2] as i32 + p[6] as i32;
            }
            let (b, g, r) = (sum[0] / 4, sum[1] / 4, sum[2] / 4);
            u_plane[cy * cw + cx] = (((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128).clamp(0, 255) as u8;
            v_plane[cy * cw + cx] = (((112 * r - 94 * g - 18 * b + 128) >> 8) + 128).clamp(0, 255) as u8;
        }
    }
    out
}

/// Real video (H.264) for screen sharing: only what changed between pictures is sent,
/// so it needs a fraction of the data JPEG pictures do, and stays smooth on Wi-Fi.
pub struct VideoEncoder {
    enc: openh264::encoder::Encoder,
}

impl VideoEncoder {
    /// `fps` pictures a second at about `bps` bits a second.
    pub fn new(fps: u64, bps: u32) -> Result<Self, String> {
        use openh264::encoder::{
            BitRate, Complexity, EncoderConfig, FrameRate, IntraFramePeriod, RateControlMode, UsageType,
        };
        let cfg = EncoderConfig::new()
            .bitrate(BitRate::from_bps(bps as _))
            .max_frame_rate(FrameRate::from_hz(fps as _))
            .usage_type(UsageType::ScreenContentRealTime)
            .rate_control_mode(RateControlMode::Bitrate)
            .skip_frames(false)
            .complexity(Complexity::Low)
            .intra_frame_period(IntraFramePeriod::from_num_frames((fps * 10) as _))
            .num_threads(4);
        let enc = openh264::encoder::Encoder::with_api_config(openh264::OpenH264API::from_source(), cfg)
            .map_err(|e| format!("Video encoder: {e}"))?;
        Ok(VideoEncoder { enc })
    }

    /// Packs one picture; returns its sent size and the video data (may be empty when
    /// the encoder decides to skip it).
    pub fn encode(&mut self, w: u32, h: u32, bgra: &[u8], max_w: u32) -> Result<(u32, u32, Vec<u8>), String> {
        if w < 2 || h < 2 || bgra.len() < (w as usize) * (h as usize) * 4 {
            return Err("Empty screen capture".into());
        }
        let (tw, th) = target_size(w, h, max_w.min(3840));
        let th = th.min(2160);
        let src = sized(w, h, bgra, tw, th);
        let yuv = bgra_to_i420(&src, tw as usize, th as usize);
        let buf = openh264::formats::YUVBuffer::from_vec(yuv, tw as usize, th as usize);
        let bits = self.enc.encode(&buf).map_err(|e| format!("Video encoder: {e}"))?;
        Ok((tw, th, bits.to_vec()))
    }
}

/// Turns the video from a [`VideoEncoder`] back into pictures.
pub struct VideoDecoder {
    dec: openh264::decoder::Decoder,
    rgba: Vec<u8>,
}

impl VideoDecoder {
    pub fn new() -> Result<Self, String> {
        let dec = openh264::decoder::Decoder::new().map_err(|e| format!("Video decoder: {e}"))?;
        Ok(VideoDecoder { dec, rgba: Vec::new() })
    }

    /// Unpacks one piece of video; returns the picture (width, height, RGBA) when there is one.
    pub fn decode(&mut self, data: &[u8]) -> Option<(usize, usize, &[u8])> {
        use openh264::formats::YUVSource;
        let mut size = None;
        for nal in openh264::nal_units(data) {
            if let Ok(Some(yuv)) = self.dec.decode(nal) {
                let (w, h) = yuv.dimensions();
                self.rgba.resize(w * h * 4, 0);
                yuv.write_rgba8(&mut self.rgba);
                size = Some((w, h));
            }
        }
        size.map(|(w, h)| (w, h, &self.rgba[..w * h * 4]))
    }
}

/// About how many bits a second the video gets: enough for sharp text at the chosen
/// size and speed (`quality` 65 ≈ smooth, 85 ≈ extra sharp), at most 40 Mbit/s.
pub fn video_bitrate(w: u32, h: u32, fps: u64, quality: u8) -> u32 {
    let per_pixel = 0.02 + (quality.clamp(50, 95) as f64 - 50.0) * 0.004;
    ((w as f64 * h as f64 * fps.min(60) as f64 * per_pixel) as u64).clamp(1_000_000, 40_000_000) as u32
}

/// Shrinks a BGRA picture by averaging each block of pixels (keeps text readable).
fn shrink(w: usize, h: usize, src: &[u8], tw: usize, th: usize) -> Vec<u8> {
    let mut out = vec![0u8; tw * th * 4];
    let cols: Vec<(usize, usize)> =
        (0..tw).map(|x| (x * w / tw, ((x + 1) * w / tw).max(x * w / tw + 1).min(w))).collect();
    let mut acc = vec![0u32; tw * 3];
    for ty in 0..th {
        let y0 = ty * h / th;
        let y1 = ((ty + 1) * h / th).max(y0 + 1).min(h);
        acc.iter_mut().for_each(|a| *a = 0);
        for y in y0..y1 {
            let row = &src[y * w * 4..(y + 1) * w * 4];
            for (tx, &(x0, x1)) in cols.iter().enumerate() {
                let a = &mut acc[tx * 3..tx * 3 + 3];
                for p in row[x0 * 4..x1 * 4].chunks_exact(4) {
                    a[0] += p[0] as u32;
                    a[1] += p[1] as u32;
                    a[2] += p[2] as u32;
                }
            }
        }
        for (tx, &(x0, x1)) in cols.iter().enumerate() {
            let n = ((x1 - x0) * (y1 - y0)).max(1) as u32;
            let o = &mut out[(ty * tw + tx) * 4..(ty * tw + tx) * 4 + 4];
            o[0] = (acc[tx * 3] / n) as u8;
            o[1] = (acc[tx * 3 + 1] / n) as u8;
            o[2] = (acc[tx * 3 + 2] / n) as u8;
            o[3] = 255;
        }
    }
    out
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

/// Desktop Duplication: the fast way to capture a monitor on Windows 8 and newer.
#[cfg(windows)]
mod dxgi {
    use super::Monitor;
    use windows::core::Interface;
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
    use windows::Win32::Graphics::Direct3D11::{
        D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_CPU_ACCESS_READ,
        D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_SDK_VERSION,
        D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
    };
    use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
    use windows::Win32::Graphics::Dxgi::{
        CreateDXGIFactory1, IDXGIAdapter, IDXGIAdapter1, IDXGIFactory1, IDXGIOutput1, IDXGIOutputDuplication,
        IDXGIResource, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO,
    };

    pub struct Dup {
        device: ID3D11Device,
        ctx: ID3D11DeviceContext,
        dup: IDXGIOutputDuplication,
        staging: Option<ID3D11Texture2D>,
        /// The monitor this captures: x, y, width, height on the desktop.
        pub rect: (i32, i32, i32, i32),
        pub w: u32,
        pub h: u32,
        held: bool,
    }

    impl Dup {
        /// Starts duplicating the monitor whose top-left corner matches `m`.
        pub fn open(m: Monitor) -> Result<Dup, String> {
            let err = |e: windows::core::Error| e.to_string();
            unsafe {
                let factory: IDXGIFactory1 = CreateDXGIFactory1().map_err(err)?;
                let mut i = 0;
                while let Ok(adapter) = factory.EnumAdapters1(i) {
                    i += 1;
                    let adapter: IDXGIAdapter1 = adapter;
                    let mut j = 0;
                    while let Ok(output) = adapter.EnumOutputs(j) {
                        j += 1;
                        let Ok(desc) = output.GetDesc() else { continue };
                        let r = desc.DesktopCoordinates;
                        if r.left != m.x || r.top != m.y {
                            continue;
                        }
                        let output1: IDXGIOutput1 = output.cast().map_err(err)?;
                        let base: IDXGIAdapter = adapter.cast().map_err(err)?;
                        let mut device: Option<ID3D11Device> = None;
                        let mut ctx: Option<ID3D11DeviceContext> = None;
                        D3D11CreateDevice(
                            &base,
                            D3D_DRIVER_TYPE_UNKNOWN,
                            HMODULE::default(),
                            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                            None,
                            D3D11_SDK_VERSION,
                            Some(&mut device),
                            None,
                            Some(&mut ctx),
                        )
                        .map_err(err)?;
                        let (Some(device), Some(ctx)) = (device, ctx) else {
                            return Err("No Direct3D device".into());
                        };
                        let dup = output1.DuplicateOutput(&device).map_err(err)?;
                        return Ok(Dup {
                            device,
                            ctx,
                            dup,
                            staging: None,
                            rect: (r.left, r.top, r.right - r.left, r.bottom - r.top),
                            w: 0,
                            h: 0,
                            held: false,
                        });
                    }
                }
                Err("That monitor can't use fast capture".into())
            }
        }

        /// Waits up to `ms` for a new picture and copies it into `out` (BGRA, top-down).
        /// Ok(false): nothing on the screen changed (only the mouse moved, or nothing).
        pub fn next(&mut self, ms: u32, out: &mut Vec<u8>) -> Result<bool, String> {
            let err = |e: windows::core::Error| e.to_string();
            unsafe {
                if self.held {
                    let _ = self.dup.ReleaseFrame();
                    self.held = false;
                }
                let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
                let mut res: Option<IDXGIResource> = None;
                if let Err(e) = self.dup.AcquireNextFrame(ms, &mut info, &mut res) {
                    if e.code() == DXGI_ERROR_WAIT_TIMEOUT {
                        return Ok(false);
                    }
                    return Err(e.to_string());
                }
                self.held = true;
                if info.LastPresentTime == 0 {
                    return Ok(false);
                }
                let tex: ID3D11Texture2D = res.ok_or("No picture")?.cast().map_err(err)?;
                let mut desc = D3D11_TEXTURE2D_DESC::default();
                tex.GetDesc(&mut desc);
                if desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM {
                    return Err("The screen uses a colour format fast capture can't read (HDR?)".into());
                }
                if self.staging.is_none() || desc.Width != self.w || desc.Height != self.h {
                    let mut sd = desc;
                    sd.Usage = D3D11_USAGE_STAGING;
                    sd.BindFlags = 0;
                    sd.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
                    sd.MiscFlags = 0;
                    sd.MipLevels = 1;
                    sd.ArraySize = 1;
                    sd.SampleDesc.Count = 1;
                    sd.SampleDesc.Quality = 0;
                    let mut st: Option<ID3D11Texture2D> = None;
                    self.device.CreateTexture2D(&sd, None, Some(&mut st)).map_err(err)?;
                    self.staging = st;
                    (self.w, self.h) = (desc.Width, desc.Height);
                }
                let st = self.staging.as_ref().ok_or("No copy buffer")?;
                self.ctx.CopyResource(st, &tex);
                let _ = self.dup.ReleaseFrame();
                self.held = false;
                let mut map = D3D11_MAPPED_SUBRESOURCE::default();
                self.ctx.Map(st, 0, D3D11_MAP_READ, 0, Some(&mut map)).map_err(err)?;
                let (w, h, pitch) = (self.w as usize, self.h as usize, map.RowPitch as usize);
                out.resize(w * h * 4, 0);
                let src = map.pData as *const u8;
                for y in 0..h {
                    std::ptr::copy_nonoverlapping(src.add(y * pitch), out.as_mut_ptr().add(y * w * 4), w * 4);
                }
                self.ctx.Unmap(st, 0);
                Ok(true)
            }
        }
    }

    impl Drop for Dup {
        fn drop(&mut self) {
            if self.held {
                unsafe {
                    let _ = self.dup.ReleaseFrame();
                }
            }
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

    #[test]
    fn shrinks_and_packs() {
        // A 4x2 picture: left half white, right half black.
        let mut px = Vec::new();
        for _ in 0..2 {
            for x in 0..4 {
                let v = if x < 2 { 255 } else { 0 };
                px.extend_from_slice(&[v, v, v, 255]);
            }
        }
        let small = super::shrink(4, 2, &px, 2, 1);
        assert_eq!(small, vec![255, 255, 255, 255, 0, 0, 0, 255]);
        let (w, h, jpeg) = super::encode(4, 2, &px, 2, 80).unwrap();
        assert_eq!((w, h), (2, 1));
        assert_eq!(&jpeg[..2], &[0xFF, 0xD8]);
    }

    #[test]
    fn video_round_trip() {
        // A 64x48 picture with a white square on blue survives encoding and decoding.
        let (w, h) = (64u32, 48u32);
        let mut px = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let white = (16..48).contains(&x) && (8..40).contains(&y);
                px.extend_from_slice(if white { &[255, 255, 255, 255] } else { &[200, 60, 20, 255] });
            }
        }
        let mut enc = super::VideoEncoder::new(30, 2_000_000).unwrap();
        let mut dec = super::VideoDecoder::new().unwrap();
        let mut got = None;
        for _ in 0..3 {
            let (tw, th, data) = enc.encode(w, h, &px, 1920).unwrap();
            assert_eq!((tw, th), (64, 48));
            if let Some((dw, dh, rgba)) = dec.decode(&data) {
                got = Some((dw, dh, rgba[(24 * 64 + 32) * 4], rgba[(2 * 64 + 2) * 4 + 2]));
            }
        }
        let (dw, dh, centre_r, corner_b) = got.expect("a decoded picture");
        assert_eq!((dw, dh), (64, 48));
        assert!(centre_r > 220, "centre should be white, red was {centre_r}");
        assert!(corner_b > 150, "corner should be blue, blue was {corner_b}");
    }

    #[test]
    fn video_sizes_are_even() {
        assert_eq!(super::target_size(1921, 1081, 4000), (1920, 1080));
        assert_eq!(super::target_size(3840, 2160, 1920), (1920, 1080));
        assert!(super::video_bitrate(1920, 1080, 30, 75) > 5_000_000);
    }
}
