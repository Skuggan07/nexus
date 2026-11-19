//! The window. Two modes the user switches between at the top:
//!   Simple   — one list, one button per device (the default)
//!   Detailed — sidebar, summary tiles, device cards with CPU/RAM/battery gauges
//! Look: black background, dark green boxes, teal buttons, dark orange gauges, white text.

use crate::{
    automation,
    identity::{self, Identity},
    net::{self, Command, Entry, PairTarget},
    state::{lock, PairSession, Phase, Role, SharedState, Status},
    store::{self, PairedPeer},
    sysstats::Stats,
    update,
};
use eframe::egui::{self, Color32, FontId, Margin, Pos2, RichText, Rounding, Sense, Stroke, TextStyle, Vec2};
use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

/// Live state of the file browser, shared with the worker threads that do the transfers.
#[derive(Default)]
struct BrowserInner {
    req: u64,
    loading: bool,
    entries: Vec<Entry>,
    error: Option<String>,
    status: Option<String>,
    busy: bool,
}

/// The open file browser: which device, where in its folders, and the live results.
struct Browser {
    peer_id: String,
    peer_name: String,
    path: Vec<String>,
    upload_input: String,
    inner: Arc<Mutex<BrowserInner>>,
}

/// Picture presets for screen viewing: (frames per second, max width, JPEG quality).
#[derive(Clone, Copy, PartialEq)]
enum Quality {
    Smooth,
    Balanced,
    Sharp,
}

impl Quality {
    const ALL: [Quality; 3] = [Quality::Smooth, Quality::Balanced, Quality::Sharp];

    fn label(self) -> &'static str {
        match self {
            Quality::Smooth => "Smooth",
            Quality::Balanced => "Balanced",
            Quality::Sharp => "Sharp",
        }
    }

    fn params(self) -> (u64, u32, u8) {
        match self {
            Quality::Smooth => (15, 1280, 60),
            Quality::Balanced => (12, 1920, 75),
            Quality::Sharp => (8, 2560, 88),
        }
    }
}

/// Live state of a screen-view session, shared with its streaming threads.
#[derive(Default)]
struct ScreenInner {
    jpeg: Option<Vec<u8>>,
    seq: u64,
    dims: (u32, u32),
    frames: u64,
    error: Option<String>,
    /// Set when the viewer window is closed, so late errors are not shown.
    closed: bool,
    control_ready: bool,
    control_error: Option<String>,
    /// How many monitors the other PC has (from its frames).
    mons: usize,
    /// Why sound could not be played, if it failed.
    sound_error: Option<String>,
}

/// One open screen viewer (several can be open at once, also several monitors of one PC).
struct ScreenView {
    peer_id: String,
    peer_name: String,
    /// Which monitor of that device is shown (0 = its main monitor).
    monitor: usize,
    /// "peer id#monitor": tells views of different monitors of one PC apart.
    key: String,
    /// How many monitors the other PC has.
    mons: usize,
    /// Open views of the PC's other monitors as soon as we know how many there are.
    open_rest: bool,
    /// Whether sound can be offered for this device (any paired PC or phone).
    is_pc: bool,
    /// Set while this PC plays the other PC's sound; storing true stops it.
    sound_stop: Option<Arc<AtomicBool>>,
    inner: Arc<Mutex<ScreenInner>>,
    /// Stops the current picture stream (a new one starts when the quality changes).
    stream_stop: Arc<AtomicBool>,
    tex: Option<egui::TextureHandle>,
    shown_seq: u64,
    /// Sends mouse/keyboard events to the other device while "Control" is on.
    control: Option<std::sync::mpsc::Sender<serde_json::Value>>,
    control_on: bool,
    /// True after clicking inside the picture: keystrokes then go to the other device.
    typing: bool,
    pressed: bool,
    panning: bool,
    last_move: Option<(f32, f32)>,
    scroll_acc: f32,
    quality: Quality,
    /// 1.0 shows the whole picture; higher values show a part of it.
    zoom: f32,
    /// Centre of the visible part, in fractions (0..1) of the picture.
    center: Pos2,
    fullscreen: bool,
    fps: f32,
    fps_frames: u64,
    fps_at: Instant,
    last_frame_at: Option<Instant>,
}

/// Live state of a monitor window, filled by its background link.
#[derive(Default)]
struct MonitorInner {
    snap: Option<serde_json::Value>,
    cpu_hist: std::collections::VecDeque<f32>,
    ram_hist: std::collections::VecDeque<f32>,
    error: Option<String>,
    notice: Option<String>,
    closed: bool,
}

/// One open "app list and monitoring" window.
struct MonitorView {
    peer_id: String,
    peer_name: String,
    inner: Arc<Mutex<MonitorInner>>,
    /// Sends "end app" requests; dropping it closes the link.
    tx: Option<std::sync::mpsc::Sender<serde_json::Value>>,
    search: String,
    by_cpu: bool,
    confirm_end: Option<String>,
}

const HISTORY: usize = 90;

const BG: Color32 = Color32::from_rgb(0, 0, 0);
const SIDEBAR: Color32 = Color32::from_rgb(10, 10, 10);
const BOX: Color32 = Color32::from_rgb(14, 58, 42);
const BOX2: Color32 = Color32::from_rgb(20, 80, 54);
const LINE: Color32 = Color32::from_rgb(30, 102, 73);
const TEXT: Color32 = Color32::WHITE;
const ACCENT: Color32 = Color32::from_rgb(17, 169, 154); // teal buttons
const ONLINE: Color32 = Color32::from_rgb(94, 234, 212); // light teal: online dots and stripes
const RING: Color32 = Color32::from_rgb(194, 65, 12); // dark orange gauges
const OFF: Color32 = Color32::from_rgb(107, 127, 117);
const BAD: Color32 = Color32::from_rgb(240, 115, 106);

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Devices,
    Activity,
    Settings,
}

pub struct NexusApp {
    me: Arc<Identity>,
    shared: SharedState,
    dir: PathBuf,
    tcp_port: u16,
    startup_error: Option<String>,
    manual_addr: String,
    confirm_remove: Option<String>,
    /// A restart/shutdown awaiting the user's confirmation: (device id, command).
    confirm_cmd: Option<(String, Command)>,
    details: Option<String>,
    browser: Option<Browser>,
    screens: Vec<ScreenView>,
    fullscreen_on: bool,
    awake_on: bool,
    monitors: Vec<MonitorView>,
    auto_open: bool,
    auto_draft: Option<AutoDraft>,
    updates: update::Shared,
    update_hidden: bool,
    detailed: bool,
    tab: Tab,
    styled: bool,
}

enum Action {
    OpenWindow,
    CloseWindow,
    Pair(PairTarget),
    Answer(bool),
    CloseSession,
    AskRemove(String),
    Remove(String),
    CancelRemove,
    PairByAddress,
    Go(Tab),
    SetDetailed(bool),
    ShowDetails(Option<String>),
    AskCommand(String, Command),
    CancelCommand,
    RunCommand(String, Command),
    Wake(String),
    OpenBrowser(String),
    CloseBrowser,
    BrowseTo(Vec<String>),
    Download(String),
    Upload(String),
    ViewScreen(String),
    /// Open a view of one monitor of a device: (device id, monitor).
    ViewMonitor(String, usize),
    /// Show every monitor of one device.
    ViewAllMonitors(String),
    /// Switch an open view (by key) to another monitor.
    SwitchMonitor(String, usize),
    /// Turn the other PC's sound on or off for a view (by key).
    SetSound(String, bool),
    ViewAllScreens,
    CloseScreen(String),
    SetQuality(String, Quality),
    SetFullscreen(String, bool),
    OpenMonitor(String),
    CloseMonitor(String),
    EndApp(String, String),
    OpenAutomation,
}

impl NexusApp {
    pub fn new(
        me: Arc<Identity>,
        shared: SharedState,
        dir: PathBuf,
        tcp_port: u16,
        startup_error: Option<String>,
    ) -> Self {
        let detailed = store::load_detailed(&dir);
        Self {
            me,
            shared,
            dir,
            tcp_port,
            startup_error,
            manual_addr: String::new(),
            confirm_remove: None,
            confirm_cmd: None,
            details: None,
            browser: None,
            screens: Vec::new(),
            fullscreen_on: false,
            awake_on: false,
            monitors: Vec::new(),
            auto_open: false,
            auto_draft: None,
            updates: {
                let u = update::Shared::default();
                update::check(&u);
                u
            },
            update_hidden: false,
            detailed,
            tab: Tab::Devices,
            styled: false,
        }
    }

    fn apply(&mut self, a: Action) {
        match a {
            Action::OpenWindow => net::open_pairing_window(&self.shared),
            Action::CloseWindow => {
                let mut s = lock(&self.shared);
                s.window = None;
                s.log("Pairing no longer allowed");
            }
            Action::Pair(target) => net::pair_with(self.me.clone(), self.shared.clone(), self.dir.clone(), target),
            Action::Answer(ok) => {
                if let Some(p) = lock(&self.shared).pair.as_mut() {
                    p.local = Some(ok);
                }
            }
            Action::CloseSession => lock(&self.shared).pair = None,
            Action::AskRemove(id) => self.confirm_remove = Some(id),
            Action::CancelRemove => self.confirm_remove = None,
            Action::Remove(id) => {
                let mut s = lock(&self.shared);
                if let Some(i) = s.paired.iter().position(|p| p.id == id) {
                    let name = s.paired.remove(i).name;
                    s.checks.remove(&id);
                    let _ = store::save_peers(&self.dir, &s.paired);
                    s.log(format!("Removed {name}. It can no longer verify with this PC."));
                }
                self.confirm_remove = None;
                self.details = None;
            }
            Action::PairByAddress => {
                let input = self.manual_addr.trim().to_string();
                let addr = input.parse::<SocketAddr>().or_else(|_| {
                    input.parse::<std::net::IpAddr>().map(|ip| SocketAddr::new(ip, net::DEFAULT_TCP_PORT))
                });
                match addr {
                    Ok(addr) => self.apply(Action::Pair(PairTarget {
                        addr,
                        name: addr.ip().to_string(),
                        expected_id: None,
                        expected_key: None,
                    })),
                    Err(_) => lock(&self.shared).log(format!("“{input}” is not an IP address")),
                }
            }
            Action::Go(t) => self.tab = t,
            Action::SetDetailed(d) => {
                self.detailed = d;
                store::save_detailed(&self.dir, d);
                if !d {
                    self.tab = Tab::Devices;
                }
            }
            Action::ShowDetails(id) => {
                self.details = id;
                self.confirm_remove = None;
                self.confirm_cmd = None;
            }
            Action::AskCommand(id, cmd) => self.confirm_cmd = Some((id, cmd)),
            Action::CancelCommand => self.confirm_cmd = None,
            Action::RunCommand(id, cmd) => {
                self.confirm_cmd = None;
                let me = self.me.clone();
                let shared = self.shared.clone();
                let resolved = {
                    let s = lock(&self.shared);
                    s.paired.iter().find(|p| p.id == id).cloned().map(|p| {
                        let addr = s
                            .seen
                            .get(&p.id)
                            .filter(|x| x.fresh())
                            .map(|x| SocketAddr::new(x.addr, x.tcp_port))
                            .or_else(|| p.last_addr.as_deref().and_then(|a| a.parse().ok()));
                        (p, addr)
                    })
                };
                if let Some((peer, Some(addr))) = resolved {
                    std::thread::spawn(move || {
                        let msg = match net::send_command(&me, &peer, addr, cmd) {
                            Ok(()) => format!("Asked {} to {}", peer.name, cmd.label().to_lowercase()),
                            Err(e) => format!("Could not {} {}: {e}", cmd.label().to_lowercase(), peer.name),
                        };
                        lock(&shared).log(msg);
                    });
                } else {
                    lock(&self.shared).log("That device is not reachable right now");
                }
            }
            Action::Wake(id) => {
                let peer = lock(&self.shared).paired.iter().find(|p| p.id == id).cloned();
                if let Some(peer) = peer {
                    let shared = self.shared.clone();
                    std::thread::spawn(move || {
                        let msg = match net::wake(&peer) {
                            Ok(()) => format!("Sent a wake signal to {}", peer.name),
                            Err(e) => format!("Could not wake {}: {e}", peer.name),
                        };
                        lock(&shared).log(msg);
                    });
                }
            }
            Action::OpenBrowser(id) => {
                let name = lock(&self.shared)
                    .paired
                    .iter()
                    .find(|p| p.id == id)
                    .map(|p| p.name.clone())
                    .unwrap_or_else(|| "Device".into());
                self.details = None;
                self.browser = Some(Browser {
                    peer_id: id,
                    peer_name: name,
                    path: Vec::new(),
                    upload_input: String::new(),
                    inner: Arc::new(Mutex::new(BrowserInner::default())),
                });
                self.start_list();
            }
            Action::CloseBrowser => self.browser = None,
            Action::BrowseTo(path) => {
                if let Some(b) = self.browser.as_mut() {
                    b.path = path;
                }
                self.start_list();
            }
            Action::Download(name) => self.start_transfer(Some(name), None),
            Action::Upload(path) => self.start_transfer(None, Some(path)),
            Action::ViewScreen(id) => self.open_screen(id, 0, false),
            Action::ViewMonitor(id, m) => self.open_screen(id, m, false),
            Action::ViewAllMonitors(id) => {
                // Use any open view of this PC that already knows how many monitors it has.
                let known = self.screens.iter().find(|s| s.peer_id == id && s.mons > 1).map(|s| s.mons);
                match known {
                    Some(n) => {
                        for m in 0..n {
                            self.open_screen(id.clone(), m, false);
                        }
                    }
                    None => match self.screens.iter_mut().find(|s| s.peer_id == id) {
                        Some(sv) => sv.open_rest = true,
                        None => self.open_screen(id, 0, true),
                    },
                }
            }
            Action::SetSound(key, on) => {
                let Some(i) = self.screens.iter().position(|s| s.key == key) else { return };
                if let Some(stop) = self.screens[i].sound_stop.take() {
                    stop.store(true, Ordering::Relaxed);
                }
                self.screens[i].inner.lock().unwrap_or_else(|e| e.into_inner()).sound_error = None;
                if !on {
                    return;
                }
                let id = self.screens[i].peer_id.clone();
                let (Some(addr), Some(peer)) = (self.addr_of(&id), self.peer_of(&id)) else {
                    self.screens[i].inner.lock().unwrap_or_else(|e| e.into_inner()).sound_error =
                        Some("That device is not reachable right now".into());
                    return;
                };
                let stop = Arc::new(AtomicBool::new(false));
                let inner = self.screens[i].inner.clone();
                net::audio_session(self.me.clone(), peer, addr, stop.clone(), move |e| {
                    inner.lock().unwrap_or_else(|e| e.into_inner()).sound_error = Some(e);
                });
                self.screens[i].sound_stop = Some(stop);
            }
            Action::SwitchMonitor(key, m) => {
                let Some(i) = self.screens.iter().position(|s| s.key == key) else { return };
                let new_key = format!("{}#{m}", self.screens[i].peer_id);
                if self.screens.iter().any(|s| s.key == new_key) {
                    return;
                }
                let id = self.screens[i].peer_id.clone();
                let target = (self.addr_of(&id), self.peer_of(&id));
                let me = self.me.clone();
                let sv = &mut self.screens[i];
                sv.monitor = m;
                sv.key = new_key;
                sv.zoom = 1.0;
                sv.center = Pos2::new(0.5, 0.5);
                sv.stream_stop.store(true, Ordering::Relaxed);
                sv.stream_stop = Arc::new(AtomicBool::new(false));
                if let (Some(addr), Some(peer)) = target {
                    spawn_stream(me, peer, addr, sv.inner.clone(), sv.stream_stop.clone(), sv.quality, m);
                }
            }
            Action::ViewAllScreens => {
                let ids: Vec<String> = {
                    let s = lock(&self.shared);
                    s.paired.iter().filter(|p| is_online(&s.status(p))).map(|p| p.id.clone()).collect()
                };
                for id in ids {
                    self.open_screen(id, 0, false);
                }
            }
            Action::CloseScreen(id) => {
                if let Some(i) = self.screens.iter().position(|s| s.key == id) {
                    let sv = self.screens.remove(i);
                    if let Some(stop) = &sv.sound_stop {
                        stop.store(true, Ordering::Relaxed);
                    }
                    sv.stream_stop.store(true, Ordering::Relaxed);
                    sv.inner.lock().unwrap_or_else(|e| e.into_inner()).closed = true;
                }
            }
            Action::SetQuality(key, q) => {
                let Some(peer_id) = self.screens.iter().find(|s| s.key == key).map(|s| s.peer_id.clone()) else {
                    return;
                };
                let target = (self.addr_of(&peer_id), self.peer_of(&peer_id));
                let me = self.me.clone();
                if let Some(sv) = self.screens.iter_mut().find(|s| s.key == key) {
                    if sv.quality != q {
                        sv.quality = q;
                        sv.stream_stop.store(true, Ordering::Relaxed);
                        sv.stream_stop = Arc::new(AtomicBool::new(false));
                        if let (Some(addr), Some(peer)) = target {
                            spawn_stream(me, peer, addr, sv.inner.clone(), sv.stream_stop.clone(), q, sv.monitor);
                        }
                    }
                }
            }
            Action::SetFullscreen(id, on) => {
                for sv in &mut self.screens {
                    sv.fullscreen = on && sv.key == id;
                }
            }
            Action::OpenMonitor(id) => self.open_monitor(id),
            Action::OpenAutomation => {
                self.auto_open = true;
                self.details = None;
            }
            Action::CloseMonitor(id) => {
                if let Some(i) = self.monitors.iter().position(|m| m.peer_id == id) {
                    let mv = self.monitors.remove(i);
                    mv.inner.lock().unwrap_or_else(|e| e.into_inner()).closed = true;
                }
            }
            Action::EndApp(id, name) => {
                if let Some(mv) = self.monitors.iter().find(|m| m.peer_id == id) {
                    if let Some(tx) = &mv.tx {
                        let _ = tx.send(serde_json::json!({ "op": "end", "name": name }));
                    }
                    mv.inner.lock().unwrap_or_else(|e| e.into_inner()).notice = Some(format!("Ending {name}…"));
                }
            }
        }
    }

    /// Opens the app list and monitoring window for a paired device.
    fn open_monitor(&mut self, id: String) {
        if self.monitors.iter().any(|m| m.peer_id == id) {
            return;
        }
        let name = self.peer_of(&id).map(|p| p.name).unwrap_or_else(|| "Device".into());
        let inner = Arc::new(Mutex::new(MonitorInner::default()));
        let tx = match (self.addr_of(&id), self.peer_of(&id)) {
            (Some(addr), Some(peer)) => {
                let st = inner.clone();
                Some(net::monitor_session(self.me.clone(), peer, addr, move |r| {
                    let mut g = st.lock().unwrap_or_else(|e| e.into_inner());
                    match r {
                        Ok(v) => {
                            if let Some(e) = v.get("error").and_then(|e| e.as_str()) {
                                g.notice = Some(e.to_string());
                            } else if v.get("op").and_then(|o| o.as_str()) == Some("ended") {
                                let n = v.get("count").and_then(|c| c.as_u64()).unwrap_or(0);
                                let app = v.get("name").and_then(|a| a.as_str()).unwrap_or("the app");
                                g.notice = Some(format!("Ended {app} ({n} process{})", if n == 1 { "" } else { "es" }));
                            } else {
                                if let Some(c) = v.get("cpu").and_then(|c| c.as_f64()) {
                                    g.cpu_hist.push_back(c as f32);
                                }
                                let used = v.get("ram_used").and_then(|x| x.as_f64()).unwrap_or(0.0);
                                let total = v.get("ram_total").and_then(|x| x.as_f64()).unwrap_or(0.0);
                                if total > 0.0 {
                                    g.ram_hist.push_back((used / total * 100.0) as f32);
                                }
                                while g.cpu_hist.len() > HISTORY {
                                    g.cpu_hist.pop_front();
                                }
                                while g.ram_hist.len() > HISTORY {
                                    g.ram_hist.pop_front();
                                }
                                g.snap = Some(v);
                                g.error = None;
                            }
                        }
                        Err(e) => {
                            if !g.closed {
                                g.error = Some(e);
                            }
                        }
                    }
                }))
            }
            _ => {
                inner.lock().unwrap_or_else(|e| e.into_inner()).error =
                    Some("That device is not reachable right now".into());
                None
            }
        };
        self.details = None;
        self.monitors.push(MonitorView {
            peer_id: id,
            peer_name: name,
            inner,
            tx,
            search: String::new(),
            by_cpu: false,
            confirm_end: None,
        });
    }

    /// Opens a live screen view of a paired device (if it is not open already) and
    /// starts its picture stream and remote-control link.
    fn open_screen(&mut self, id: String, monitor: usize, open_rest: bool) {
        let key = format!("{id}#{monitor}");
        if self.screens.iter().any(|s| s.key == key) {
            return;
        }
        let name = self.peer_of(&id).map(|p| p.name).unwrap_or_else(|| "Device".into());
        let inner = Arc::new(Mutex::new(ScreenInner::default()));
        let stream_stop = Arc::new(AtomicBool::new(false));
        self.details = None;
        let mut sv = ScreenView {
            peer_id: id.clone(),
            peer_name: name,
            monitor,
            key,
            mons: 1,
            open_rest,
            is_pc: self.peer_of(&id).is_some(),
            sound_stop: None,
            inner: inner.clone(),
            stream_stop: stream_stop.clone(),
            tex: None,
            shown_seq: u64::MAX,
            control: None,
            control_on: true,
            typing: false,
            pressed: false,
            panning: false,
            last_move: None,
            scroll_acc: 0.0,
            quality: Quality::Balanced,
            zoom: 1.0,
            center: Pos2::new(0.5, 0.5),
            fullscreen: false,
            fps: 0.0,
            fps_frames: 0,
            fps_at: Instant::now(),
            last_frame_at: None,
        };
        match (self.addr_of(&id), self.peer_of(&id)) {
            (Some(addr), Some(peer)) => {
                let status = inner.clone();
                sv.control = Some(net::control_session(self.me.clone(), peer.clone(), addr, move |r| {
                    let mut g = status.lock().unwrap_or_else(|e| e.into_inner());
                    match r {
                        Ok(()) => {
                            g.control_ready = true;
                            g.control_error = None;
                        }
                        Err(e) => {
                            g.control_ready = false;
                            if !g.closed {
                                g.control_error = Some(e);
                            }
                        }
                    }
                }));
                spawn_stream(self.me.clone(), peer, addr, inner, stream_stop, sv.quality, monitor);
            }
            _ => {
                inner.lock().unwrap_or_else(|e| e.into_inner()).error =
                    Some("That device is not reachable right now".into())
            }
        }
        self.screens.push(sv);
    }

    fn addr_of(&self, id: &str) -> Option<SocketAddr> {
        let s = lock(&self.shared);
        s.seen.get(id).filter(|x| x.fresh()).map(|x| SocketAddr::new(x.addr, x.tcp_port)).or_else(|| {
            s.paired.iter().find(|p| p.id == id).and_then(|p| p.last_addr.as_deref()).and_then(|a| a.parse().ok())
        })
    }

    fn peer_of(&self, id: &str) -> Option<PairedPeer> {
        lock(&self.shared).paired.iter().find(|p| p.id == id).cloned()
    }

    /// Loads the current folder of the open browser in the background.
    fn start_list(&self) {
        let Some(b) = &self.browser else { return };
        let inner = b.inner.clone();
        let req = {
            let mut g = inner.lock().unwrap();
            g.req += 1;
            g.loading = true;
            g.error = None;
            g.req
        };
        let (Some(addr), Some(peer)) = (self.addr_of(&b.peer_id), self.peer_of(&b.peer_id)) else {
            let mut g = inner.lock().unwrap();
            g.loading = false;
            g.error = Some("That device is not reachable right now".into());
            return;
        };
        let me = self.me.clone();
        let rel = b.path.join("/");
        std::thread::spawn(move || {
            let res = net::fs_list(&me, &peer, addr, &rel);
            let mut g = inner.lock().unwrap();
            if g.req != req {
                return; // a newer navigation replaced this one
            }
            g.loading = false;
            match res {
                Ok(entries) => {
                    g.entries = entries;
                    g.error = None;
                }
                Err(e) => {
                    g.entries.clear();
                    g.error = Some(e);
                }
            }
        });
    }

    /// Downloads a file (name set) or uploads a local file (path set), in the background.
    fn start_transfer(&self, download: Option<String>, upload: Option<String>) {
        let Some(b) = &self.browser else { return };
        let inner = b.inner.clone();
        {
            let mut g = inner.lock().unwrap();
            if g.busy {
                return;
            }
            g.busy = true;
            g.status = Some("Transferring…".into());
        }
        let (Some(addr), Some(peer)) = (self.addr_of(&b.peer_id), self.peer_of(&b.peer_id)) else {
            let mut g = inner.lock().unwrap();
            g.busy = false;
            g.status = Some("That device is not reachable right now".into());
            return;
        };
        let me = self.me.clone();
        let path = b.path.clone();
        std::thread::spawn(move || {
            let status = if let Some(name) = download {
                let rel = if path.is_empty() { name.clone() } else { format!("{}/{}", path.join("/"), name) };
                let dir = directories::UserDirs::new()
                    .and_then(|u| u.download_dir().map(|d| d.to_path_buf()))
                    .unwrap_or_else(std::env::temp_dir);
                let _ = std::fs::create_dir_all(&dir);
                let dest = dir.join(&name);
                match net::fs_get(&me, &peer, addr, &rel, &dest) {
                    Ok(()) => format!("Saved to {}", dest.display()),
                    Err(e) => format!("Download failed: {e}"),
                }
            } else if let Some(p) = upload {
                match net::fs_put(&me, &peer, addr, std::path::Path::new(p.trim())) {
                    Ok(saved) => format!("Uploaded {saved} to the device's “Nexus Inbox” folder"),
                    Err(e) => format!("Upload failed: {e}"),
                }
            } else {
                String::new()
            };
            let mut g = inner.lock().unwrap();
            g.busy = false;
            g.status = Some(status);
        });
    }
}

struct Row {
    peer: PairedPeer,
    status: Status,
    addr: Option<String>,
}
struct Near {
    id: String,
    name: String,
    os: String,
    public_key: String,
    addr: SocketAddr,
}

/* ------------------------------------------------------------ styling */

fn apply_style(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = BOX;
    v.extreme_bg_color = BOX2;
    v.faint_bg_color = BOX2;
    v.window_rounding = Rounding::same(14.0);
    v.window_stroke = Stroke::new(1.0, LINE);
    v.selection.bg_fill = ACCENT;
    v.selection.stroke = Stroke::new(1.0, TEXT);
    v.hyperlink_color = ONLINE;
    v.override_text_color = Some(TEXT);
    let r = Rounding::same(8.0);
    v.widgets.noninteractive.rounding = r;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, LINE);
    v.widgets.inactive.rounding = r;
    v.widgets.inactive.bg_fill = BOX2;
    v.widgets.inactive.weak_bg_fill = BOX2;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, LINE);
    v.widgets.hovered.rounding = r;
    v.widgets.hovered.bg_fill = LINE;
    v.widgets.hovered.weak_bg_fill = LINE;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, ONLINE);
    v.widgets.active.rounding = r;
    v.widgets.active.bg_fill = LINE;
    v.widgets.active.weak_bg_fill = LINE;
    v.widgets.open.rounding = r;
    ctx.set_visuals(v);
    ctx.style_mut(|s| {
        s.spacing.item_spacing = egui::vec2(10.0, 10.0);
        s.spacing.button_padding = egui::vec2(14.0, 7.0);
        s.text_styles.insert(TextStyle::Heading, FontId::proportional(24.0));
        s.text_styles.insert(TextStyle::Body, FontId::proportional(15.0));
        s.text_styles.insert(TextStyle::Button, FontId::proportional(15.0));
        s.text_styles.insert(TextStyle::Small, FontId::proportional(12.5));
    });
}

fn box_frame() -> egui::Frame {
    egui::Frame::none()
        .fill(BOX)
        .stroke(Stroke::new(1.0, LINE))
        .rounding(Rounding::same(12.0))
        .inner_margin(Margin::same(16.0))
}

/// Teal button with bold white text.
fn primary(text: &str) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.to_string()).color(TEXT).strong()).fill(ACCENT).stroke(Stroke::NONE)
}

/// Dark green button with a thin border.
fn secondary(text: &str) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.to_string()).color(TEXT).strong()).fill(BOX2).stroke(Stroke::new(1.0, LINE))
}

fn dot(ui: &mut egui::Ui, color: Color32, radius: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(radius * 2.0 + 4.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), radius, color);
}

/// Diamond logo with an N, like the design.
fn logo(ui: &mut egui::Ui, size: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    let c = rect.center();
    let h = size * 0.48;
    let pts = vec![Pos2::new(c.x, c.y - h), Pos2::new(c.x + h, c.y), Pos2::new(c.x, c.y + h), Pos2::new(c.x - h, c.y)];
    ui.painter().add(egui::Shape::closed_line(pts, Stroke::new(2.0, ACCENT)));
    ui.painter().text(c, egui::Align2::CENTER_CENTER, "N", FontId::proportional(size * 0.42), TEXT);
}

/// Circular percentage gauge. `None` shows a dash.
fn gauge(ui: &mut egui::Ui, value: Option<f32>, label: &str) {
    ui.vertical(|ui| {
        ui.set_width(70.0);
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(64.0), Sense::hover());
        let c = rect.center();
        let r = 27.0;
        let p = ui.painter();
        p.circle_stroke(c, r, Stroke::new(5.0, LINE));
        if let Some(v) = value {
            let frac = (v / 100.0).clamp(0.0, 1.0);
            let steps = (64.0 * frac).ceil().max(2.0) as usize;
            let start = -std::f32::consts::FRAC_PI_2;
            let pts: Vec<Pos2> = (0..=steps)
                .map(|i| {
                    let a = start + std::f32::consts::TAU * frac * (i as f32 / steps as f32);
                    Pos2::new(c.x + r * a.cos(), c.y + r * a.sin())
                })
                .collect();
            p.add(egui::Shape::line(pts, Stroke::new(5.0, RING)));
        }
        let text = value.map(|v| format!("{v:.0}%")).unwrap_or_else(|| "—".into());
        p.text(c, egui::Align2::CENTER_CENTER, text, FontId::proportional(14.0), TEXT);
        ui.vertical_centered(|ui| ui.label(RichText::new(label).size(11.0).strong()));
    });
}

fn status_line(s: &Status) -> (Color32, String) {
    match s {
        Status::Verified { rtt_ms, .. } => (ONLINE, format!("Online · {rtt_ms} ms")),
        Status::Checking => (ONLINE, "Online · checking".into()),
        Status::NotAnswering(_) => (RING, "Not answering".into()),
        Status::IdentityChanged => (BAD, "Key changed · not trusted".into()),
        Status::Offline => (OFF, "Offline".into()),
    }
}

fn stats_of(s: &Status) -> Option<&Stats> {
    match s {
        Status::Verified { stats, .. } => stats.as_ref(),
        _ => None,
    }
}

fn is_online(s: &Status) -> bool {
    matches!(s, Status::Verified { .. } | Status::Checking)
}

/* -------------------------------------------------------------- frame */

impl eframe::App for NexusApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if !self.styled {
            apply_style(ctx);
            self.styled = true;
        }
        self.update_banner(ctx);

        let (rows, nearby, window, session, log) = {
            let s = lock(&self.shared);
            let mut rows: Vec<Row> = s
                .paired
                .iter()
                .map(|p| Row {
                    status: s.status(p),
                    addr: s.seen.get(&p.id).filter(|x| x.fresh()).map(|x| x.addr.to_string()).or_else(|| {
                        p.last_addr.as_deref().and_then(|a| a.parse::<SocketAddr>().ok()).map(|a| a.ip().to_string())
                    }),
                    peer: p.clone(),
                })
                .collect();
            rows.sort_by_key(|r| r.peer.name.to_lowercase());
            let mut nearby: Vec<Near> = s
                .seen
                .values()
                .filter(|x| x.fresh() && !s.is_paired(&x.id))
                .map(|x| Near {
                    id: x.id.clone(),
                    name: x.name.clone(),
                    os: x.os.clone(),
                    public_key: x.public_key.clone(),
                    addr: SocketAddr::new(x.addr, x.tcp_port),
                })
                .collect();
            nearby.sort_by_key(|n| n.name.to_lowercase());
            let window = s
                .open_window()
                .map(|w| (w.expires.saturating_duration_since(Instant::now()).as_secs(), w.attempts_left));
            let log: Vec<(u64, String)> =
                s.log.iter().take(80).map(|(t, m)| (t.elapsed().as_secs(), m.clone())).collect();
            (rows, nearby, window, s.pair.clone(), log)
        };
        let online = rows.iter().filter(|r| is_online(&r.status)).count();
        let busy = session.is_some();
        let mut actions = Vec::new();

        if self.detailed {
            self.sidebar(ctx, &mut actions);
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(BG).inner_margin(Margin::symmetric(32.0, 24.0)))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    if let Some(e) = &self.startup_error {
                        egui::Frame::none()
                            .fill(BAD.gamma_multiply(0.2))
                            .rounding(Rounding::same(10.0))
                            .inner_margin(Margin::same(12.0))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.label(e);
                            });
                    }
                    if self.detailed {
                        self.detailed_header(ui, window, &mut actions);
                        match self.tab {
                            Tab::Devices => self.detailed_devices(ui, &rows, &nearby, online, busy, &mut actions),
                            Tab::Activity => activity(ui, &log),
                            Tab::Settings => self.settings(ui),
                        }
                    } else {
                        self.simple(ui, &rows, &nearby, online, window, busy, &mut actions);
                    }
                });
            });

        if let Some(id) = self.details.clone() {
            match rows.iter().find(|r| r.peer.id == id) {
                Some(r) => self.details_window(ctx, r, &mut actions),
                None => actions.push(Action::ShowDetails(None)),
            }
        }
        if let Some(p) = &session {
            pair_dialog(ctx, p, &mut actions);
        }
        if self.browser.is_some() {
            self.file_browser(ctx, &mut actions);
        }
        for i in 0..self.screens.len() {
            self.screen_view(ctx, i, &mut actions);
        }
        for i in 0..self.monitors.len() {
            self.monitor_window(ctx, i, &mut actions);
        }
        if self.auto_open {
            self.automation_window(ctx);
        }
        // Keep this PC's display awake while any screen is being watched.
        if !self.screens.is_empty() {
            crate::screen::keep_display_awake(true);
            self.awake_on = true;
        } else if self.awake_on {
            crate::screen::keep_display_awake(false);
            self.awake_on = false;
        }

        for a in actions {
            self.apply(a);
        }
        let full = self.screens.iter().any(|s| s.fullscreen);
        if full != self.fullscreen_on {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(full));
            self.fullscreen_on = full;
        }
        let viewing = !self.screens.is_empty();
        let fast =
            session.is_some() || window.is_some() || self.browser.is_some() || viewing || !self.monitors.is_empty();
        let ms = if viewing {
            80
        } else if fast {
            250
        } else {
            1000
        };
        ctx.request_repaint_after(Duration::from_millis(ms));
    }
}

impl NexusApp {
    /// Simple | Detailed switch.
    fn mode_switch(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        egui::Frame::none()
            .fill(BOX)
            .stroke(Stroke::new(1.0, LINE))
            .rounding(Rounding::same(10.0))
            .inner_margin(Margin::same(3.0))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                ui.horizontal(|ui| {
                    for (label, d) in [("Simple", false), ("Detailed", true)] {
                        let on = self.detailed == d;
                        let b = egui::Button::new(RichText::new(label).strong().color(TEXT))
                            .fill(if on { ACCENT } else { Color32::TRANSPARENT })
                            .stroke(Stroke::NONE)
                            .min_size(Vec2::new(86.0, 30.0));
                        if ui.add(b).clicked() && !on {
                            actions.push(Action::SetDetailed(d));
                        }
                    }
                });
            });
    }

    fn pairing_button(&self, ui: &mut egui::Ui, window: Option<(u64, u8)>, label: &str, actions: &mut Vec<Action>) {
        match window {
            Some((secs, _)) => {
                if ui.add(secondary("Stop").min_size(Vec2::new(70.0, 40.0))).clicked() {
                    actions.push(Action::CloseWindow);
                }
                ui.label(RichText::new(format!("Pairing on · {}:{:02}", secs / 60, secs % 60)).strong().color(ONLINE));
            }
            None => {
                if ui.add(primary(label).min_size(Vec2::new(160.0, 40.0))).clicked() {
                    actions.push(Action::OpenWindow);
                }
            }
        }
    }

    /* ---------------------------------------------------------- simple */

    #[allow(clippy::too_many_arguments)]
    fn simple(
        &self,
        ui: &mut egui::Ui,
        rows: &[Row],
        nearby: &[Near],
        online: usize,
        window: Option<(u64, u8)>,
        busy: bool,
        actions: &mut Vec<Action>,
    ) {
        let width = ui.available_width().min(760.0);
        let side = ((ui.available_width() - width) / 2.0).max(0.0);
        ui.horizontal(|ui| {
            ui.add_space(side);
            ui.vertical(|ui| {
                ui.set_width(width);
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    logo(ui, 30.0);
                    ui.label(RichText::new("NEXUS").size(22.0).strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        self.pairing_button(ui, window, "+  ADD DEVICE", actions);
                        ui.add_space(6.0);
                        self.mode_switch(ui, actions);
                    });
                });
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("{online} of {} devices online", rows.len())));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add(secondary("Automation")).clicked() {
                            actions.push(Action::OpenAutomation);
                        }
                        if online > 0 && ui.add(secondary("Watch all screens")).clicked() {
                            actions.push(Action::ViewAllScreens);
                        }
                    });
                });
                if rows.is_empty() {
                    box_frame().show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(RichText::new("No devices yet").size(17.0).strong());
                        ui.label("Open Nexus on your phone, press “+ ADD DEVICE” here, then tap “Pair” on the phone.");
                    });
                }
                for r in rows {
                    let (color, text) = status_line(&r.status);
                    box_frame().inner_margin(Margin::symmetric(20.0, 16.0)).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            dot(ui, color, 6.0);
                            ui.add_space(6.0);
                            ui.vertical(|ui| {
                                ui.label(RichText::new(&r.peer.name).size(18.0).strong());
                                ui.label(text);
                            });
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.add(secondary("Details").min_size(Vec2::new(90.0, 38.0))).clicked() {
                                    actions.push(Action::ShowDetails(Some(r.peer.id.clone())));
                                }
                                if is_online(&r.status)
                                    && ui.add(secondary("View screen").min_size(Vec2::new(110.0, 38.0))).clicked()
                                {
                                    actions.push(Action::ViewScreen(r.peer.id.clone()));
                                }
                            });
                        });
                    });
                }
                for n in nearby {
                    ui.add_space(8.0);
                    egui::Frame::none()
                        .stroke(Stroke::new(1.0, ACCENT))
                        .rounding(Rounding::same(12.0))
                        .inner_margin(Margin::symmetric(20.0, 14.0))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                ui.vertical(|ui| {
                                    ui.label(RichText::new("Found nearby").size(13.0).strong().color(ONLINE));
                                    ui.label(RichText::new(&n.name).size(17.0).strong());
                                });
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if ui.add_enabled(!busy, primary("Pair").min_size(Vec2::new(90.0, 38.0))).clicked()
                                    {
                                        actions.push(pair_action(n));
                                    }
                                });
                            });
                        });
                }
                ui.add_space(24.0);
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new(format!("This PC: {}", self.me.name)).size(13.0));
                });
            });
        });
    }

    /* -------------------------------------------------------- detailed */

    fn sidebar(&self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        egui::SidePanel::left("nav")
            .exact_width(220.0)
            .resizable(false)
            .frame(egui::Frame::none().fill(SIDEBAR).inner_margin(Margin::same(16.0)).stroke(Stroke::new(1.0, BOX2)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    logo(ui, 30.0);
                    ui.label(RichText::new("NEXUS").size(20.0).strong());
                });
                ui.add_space(16.0);
                for (t, label) in [(Tab::Devices, "Devices"), (Tab::Activity, "Activity"), (Tab::Settings, "Settings")]
                {
                    let on = self.tab == t;
                    let b = egui::Button::new(RichText::new(label).size(15.0).strong().color(TEXT))
                        .fill(if on { BOX2 } else { Color32::TRANSPARENT })
                        .stroke(if on { Stroke::new(1.0, ACCENT) } else { Stroke::NONE })
                        .min_size(Vec2::new(ui.available_width(), 36.0));
                    if ui.add(b).clicked() {
                        actions.push(Action::Go(t));
                    }
                }
                ui.add_space(6.0);
                if ui.add(secondary("Watch all screens").min_size(Vec2::new(ui.available_width(), 32.0))).clicked() {
                    actions.push(Action::ViewAllScreens);
                }
                ui.add_space(6.0);
                if ui.add(secondary("Automation").min_size(Vec2::new(ui.available_width(), 32.0))).clicked() {
                    actions.push(Action::OpenAutomation);
                }
                ui.add_space(6.0);
                ui.label(RichText::new("Coming next: Floating screen · Better wake").size(12.0));
                ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                    ui.label(
                        RichText::new(format!("key {}", identity::fingerprint(&self.me.public_hex())))
                            .size(12.0)
                            .monospace(),
                    );
                    ui.label(RichText::new(&self.me.name).strong());
                    ui.label(RichText::new("● THIS PC").size(11.0).color(ONLINE));
                    ui.separator();
                });
            });
    }

    fn detailed_header(&self, ui: &mut egui::Ui, window: Option<(u64, u8)>, actions: &mut Vec<Action>) {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new("CONTROL CENTER").size(12.0).strong());
                let title = match self.tab {
                    Tab::Devices => "MY DEVICES",
                    Tab::Activity => "ACTIVITY",
                    Tab::Settings => "SETTINGS",
                };
                ui.label(RichText::new(title).size(30.0).strong());
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                self.pairing_button(ui, window, "+  ALLOW PAIRING", actions);
                ui.add_space(6.0);
                self.mode_switch(ui, actions);
            });
        });
        ui.add_space(8.0);
    }

    fn detailed_devices(
        &mut self,
        ui: &mut egui::Ui,
        rows: &[Row],
        nearby: &[Near],
        online: usize,
        busy: bool,
        actions: &mut Vec<Action>,
    ) {
        ui.columns(3, |c| {
            for (i, (label, n)) in
                [("ONLINE", online), ("PAIRED", rows.len()), ("NEARBY", nearby.len())].iter().enumerate()
            {
                box_frame().show(&mut c[i], |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(*label).size(13.0).strong());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(RichText::new(format!("{n:02}")).size(30.0).strong());
                        });
                    });
                });
            }
        });
        ui.add_space(6.0);

        if rows.is_empty() {
            box_frame().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new("No paired devices yet").size(17.0).strong());
                ui.label("1. Open Nexus on your phone or another PC on the same Wi-Fi.");
                ui.label("2. Press “+ ALLOW PAIRING” here, then tap “Pair” on the other device.");
                ui.label("3. Check that both screens show the same number and confirm.");
            });
        } else {
            let cols = ((ui.available_width() + 10.0) / 330.0).floor().max(1.0) as usize;
            let confirm_cmd = self.confirm_cmd.clone();
            ui.columns(cols, |c| {
                for (i, r) in rows.iter().enumerate() {
                    device_card(&mut c[i % cols], r, self.confirm_remove.as_deref(), confirm_cmd.as_ref(), actions);
                }
            });
        }

        for n in nearby {
            ui.add_space(4.0);
            egui::Frame::none()
                .fill(BOX)
                .stroke(Stroke::new(1.0, ACCENT))
                .rounding(Rounding::same(12.0))
                .inner_margin(Margin::symmetric(18.0, 12.0))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("NEW SIGNAL").size(12.0).strong().color(ONLINE));
                        ui.label(RichText::new(&n.name).strong());
                        ui.label(format!(
                            "· {} · {} · key {}",
                            n.os,
                            n.addr.ip(),
                            identity::fingerprint(&n.public_key)
                        ));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.add_enabled(!busy, primary("PAIR").min_size(Vec2::new(80.0, 36.0))).clicked() {
                                actions.push(pair_action(n));
                            }
                        });
                    });
                });
        }

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label("Not listed? Pair by IP address");
            let resp = ui
                .add(egui::TextEdit::singleline(&mut self.manual_addr).hint_text("192.168.1.31").desired_width(160.0));
            let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if (ui.add(secondary("Pair")).clicked() || enter) && !self.manual_addr.trim().is_empty() && !busy {
                actions.push(Action::PairByAddress);
            }
        });
    }

    /// A strip at the top of the window when a newer Nexus is available or downloading.
    fn update_banner(&mut self, ctx: &egui::Context) {
        let state = update::lock(&self.updates).clone();
        let show = match &state {
            update::State::Available { .. } => !self.update_hidden,
            update::State::Downloading { .. } => true,
            _ => false,
        };
        if !show {
            return;
        }
        egui::TopBottomPanel::top("update-banner")
            .frame(egui::Frame::none().fill(BOX2).inner_margin(Margin::symmetric(16.0, 8.0)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| match state {
                    update::State::Available { build, .. } => {
                        ui.label(
                            RichText::new(format!(
                                "Nexus build {build} is available (you have build {})",
                                update::build()
                            ))
                            .strong(),
                        );
                        if ui.add(primary("Update now")).clicked() {
                            update::install(&self.updates);
                        }
                        if ui.add(secondary("Later")).clicked() {
                            self.update_hidden = true;
                        }
                    }
                    update::State::Downloading { build, done, total } => {
                        ui.label(
                            RichText::new(format!("Downloading build {build}… Nexus restarts by itself")).strong(),
                        );
                        let frac = if total > 0 { done as f32 / total as f32 } else { 0.0 };
                        ui.add(egui::ProgressBar::new(frac).desired_width(220.0).show_percentage());
                    }
                    _ => {}
                });
            });
        ctx.request_repaint_after(Duration::from_millis(200));
    }

    fn settings(&self, ui: &mut egui::Ui) {
        box_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("UPDATES").size(14.0).strong());
            let state = update::lock(&self.updates).clone();
            let mine = update::build();
            let text = match &state {
                update::State::Idle => format!("This is build {mine}."),
                update::State::Checking => "Checking for a newer version…".to_string(),
                update::State::UpToDate => format!("This is build {mine} — the newest version."),
                update::State::Available { build, .. } => {
                    format!("Build {build} is available (you have build {mine}).")
                }
                update::State::Downloading { build, .. } => format!("Downloading build {build}…"),
                update::State::Failed(e) => format!("Could not update: {e}"),
            };
            ui.label(RichText::new(text).color(if matches!(state, update::State::Failed(_)) { BAD } else { TEXT }));
            ui.horizontal(|ui| {
                if matches!(state, update::State::Available { .. }) {
                    if ui.add(primary("Update now")).clicked() {
                        update::install(&self.updates);
                    }
                } else if ui.add(secondary("Check for updates")).clicked() {
                    update::check(&self.updates);
                }
            });
            ui.label(
                RichText::new("Nexus checks when it starts. Updating keeps your paired devices and settings.")
                    .size(12.0)
                    .color(OFF),
            );
        });
        box_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("THIS PC").size(14.0).strong());
            egui::Grid::new("me").num_columns(2).spacing([24.0, 8.0]).show(ui, |ui| {
                for (k, v) in [
                    ("Name", self.me.name.clone()),
                    ("System", identity::os_label()),
                    ("Key fingerprint", identity::fingerprint(&self.me.public_hex())),
                    ("Device ID", self.me.id.clone()),
                    ("Listening on", format!("TCP {} · discovery UDP 47800", self.tcp_port)),
                    ("Shared folder", net::fs_root().display().to_string()),
                    ("Data folder", self.dir.display().to_string()),
                    ("Version", env!("CARGO_PKG_VERSION").to_string()),
                ] {
                    ui.label(RichText::new(k).strong());
                    ui.label(v);
                    ui.end_row();
                }
            });
        });
        box_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("PRIVACY AND SECURITY").size(14.0).strong());
            let mut allow = lock(&self.shared).allow_control;
            if ui
                .checkbox(&mut allow, "Allow remote control — paired devices can use this PC's mouse and keyboard")
                .changed()
            {
                lock(&self.shared).allow_control = allow;
                store::save_allow_control(&self.dir, allow);
            }
            let mut share = lock(&self.shared).share_clipboard;
            if ui
                .checkbox(&mut share, "Shared clipboard — text and files you copy here go to your paired devices that are online, and theirs come here")
                .changed()
            {
                lock(&self.shared).share_clipboard = share;
                store::save_share_clipboard(&self.dir, share);
            }
            ui.add_space(4.0);
            ui.label(
                "Only devices you paired by comparing the 6-digit number are trusted. Status checks and remote \
                 commands are signed so only the real device can issue them. File browsing opens a separate \
                 connection that both sides authenticate before any file is listed or sent. Screen viewing and remote control use the same kind of authenticated connection.",
            );
        });
    }

    fn details_window(&self, ctx: &egui::Context, r: &Row, actions: &mut Vec<Action>) {
        let (color, text) = status_line(&r.status);
        let stats = stats_of(&r.status).cloned().unwrap_or_default();
        let mut open = true;
        egui::Window::new(RichText::new(r.peer.name.to_uppercase()).size(18.0).strong())
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .frame(box_frame().inner_margin(Margin::same(20.0)))
            .show(ctx, |ui| {
                ui.set_width(360.0);
                ui.horizontal(|ui| {
                    dot(ui, color, 5.0);
                    ui.label(RichText::new(text).strong());
                });
                ui.horizontal(|ui| {
                    gauge(ui, stats.cpu, "CPU");
                    ui.add_space(24.0);
                    gauge(ui, stats.ram, "RAM");
                    ui.add_space(24.0);
                    gauge(ui, stats.battery, "BATTERY");
                });
                ui.label(format!("{} · {}", r.peer.os, r.addr.as_deref().unwrap_or("address unknown")));
                ui.label(
                    RichText::new(format!("key {}", identity::fingerprint(&r.peer.public_key))).monospace().size(12.5),
                );
                ui.add_space(6.0);
                power_controls(ui, r, self.confirm_cmd.as_ref(), actions);
                if is_online(&r.status) {
                    ui.add_space(2.0);
                    if ui.add(secondary("Browse files").min_size(Vec2::new(0.0, 36.0))).clicked() {
                        actions.push(Action::OpenBrowser(r.peer.id.clone()));
                    }
                    ui.add_space(2.0);
                    if ui.add(secondary("View screen").min_size(Vec2::new(0.0, 36.0))).clicked() {
                        actions.push(Action::ViewScreen(r.peer.id.clone()));
                    }
                    if !matches!(r.peer.os.as_str(), "Android" | "iOS") {
                        ui.add_space(2.0);
                        if ui
                            .add(secondary("View all monitors").min_size(Vec2::new(0.0, 36.0)))
                            .on_hover_text("One window for each monitor of this PC")
                            .clicked()
                        {
                            actions.push(Action::ViewAllMonitors(r.peer.id.clone()));
                        }
                    }
                    ui.add_space(2.0);
                    if ui.add(secondary("Apps and monitoring").min_size(Vec2::new(0.0, 36.0))).clicked() {
                        actions.push(Action::OpenMonitor(r.peer.id.clone()));
                    }
                }
                ui.add_space(2.0);
                remove_controls(ui, r, self.confirm_remove.as_deref(), actions);
            });
        if !open {
            actions.push(Action::ShowDetails(None));
        }
    }

    /// The file browser window: breadcrumb, folder list, download and upload.
    fn file_browser(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let Some(b) = self.browser.as_mut() else { return };
        let (loading, entries, error, status, busy) = {
            let g = b.inner.lock().unwrap_or_else(|e| e.into_inner());
            (g.loading, g.entries.clone(), g.error.clone(), g.status.clone(), g.busy)
        };
        let mut open = true;
        egui::Window::new(RichText::new(format!("{} — files", b.peer_name)).size(17.0).strong())
            .collapsible(false)
            .resizable(true)
            .open(&mut open)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .frame(box_frame().inner_margin(Margin::same(18.0)))
            .show(ctx, |ui| {
                ui.set_width(540.0);
                ui.horizontal_wrapped(|ui| {
                    if ui.add(secondary("Home")).clicked() {
                        actions.push(Action::BrowseTo(Vec::new()));
                    }
                    let mut acc = Vec::new();
                    for part in &b.path {
                        ui.label("/");
                        acc.push(part.clone());
                        if ui.add(secondary(part.as_str())).clicked() {
                            actions.push(Action::BrowseTo(acc.clone()));
                        }
                    }
                });
                ui.separator();
                if loading {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Loading…");
                    });
                }
                if let Some(e) = &error {
                    ui.label(RichText::new(e).color(BAD));
                }
                egui::ScrollArea::vertical().max_height(320.0).auto_shrink([false, false]).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    if !b.path.is_empty() && ui.add(secondary("Up one folder")).clicked() {
                        let mut p = b.path.clone();
                        p.pop();
                        actions.push(Action::BrowseTo(p));
                    }
                    for e in &entries {
                        ui.horizontal(|ui| {
                            if e.dir {
                                if ui.add(secondary(&format!("{}/", e.name))).clicked() {
                                    let mut p = b.path.clone();
                                    p.push(e.name.clone());
                                    actions.push(Action::BrowseTo(p));
                                }
                            } else {
                                ui.label(format!("{}  ·  {}", e.name, human_size(e.size)));
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if ui.add_enabled(!busy, secondary("Download")).clicked() {
                                        actions.push(Action::Download(e.name.clone()));
                                    }
                                });
                            }
                        });
                    }
                });
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label("Upload (full path):");
                    ui.add(
                        egui::TextEdit::singleline(&mut b.upload_input)
                            .hint_text("C:\\Users\\me\\file.pdf")
                            .desired_width(280.0),
                    );
                    let ready = !busy && !b.upload_input.trim().is_empty();
                    if ui.add_enabled(ready, primary("Upload")).clicked() {
                        actions.push(Action::Upload(b.upload_input.clone()));
                    }
                });
                if let Some(st) = &status {
                    ui.label(RichText::new(st).color(ONLINE));
                }
            });
        if !open {
            actions.push(Action::CloseBrowser);
        }
    }

    /// Draws one screen viewer — as a window, or covering the whole app in fullscreen —
    /// and forwards mouse/keyboard input to the other device while "Control" is on.
    fn screen_view(&mut self, ctx: &egui::Context, i: usize, actions: &mut Vec<Action>) {
        let sv = &mut self.screens[i];
        let (new_jpeg, dims, error, frames, cur_seq, ctl_ready, ctl_error) = {
            let g = sv.inner.lock().unwrap_or_else(|e| e.into_inner());
            if g.mons > 0 {
                sv.mons = g.mons;
            }
            let nj = if g.jpeg.is_some() && g.seq != sv.shown_seq { g.jpeg.clone() } else { None };
            (nj, g.dims, g.error.clone(), g.frames, g.seq, g.control_ready, g.control_error.clone())
        };
        // Sound that failed turns itself off, so the box unticks and the reason stays shown.
        if sv.sound_stop.is_some() && sv.inner.lock().unwrap_or_else(|e| e.into_inner()).sound_error.is_some() {
            if let Some(stop) = sv.sound_stop.take() {
                stop.store(true, Ordering::Relaxed);
            }
        }
        // "All monitors": once we know how many there are, open the others too.
        if sv.open_rest && frames > 0 {
            sv.open_rest = false;
            for m in (0..sv.mons).filter(|m| *m != sv.monitor) {
                actions.push(Action::ViewMonitor(sv.peer_id.clone(), m));
            }
        }
        if let Some(bytes) = new_jpeg {
            if let Ok(decoded) = image::load_from_memory(&bytes) {
                let rgba = decoded.to_rgba8();
                let size = [rgba.width() as usize, rgba.height() as usize];
                let color = egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw());
                match &mut sv.tex {
                    Some(t) => t.set(color, egui::TextureOptions::LINEAR),
                    None => {
                        sv.tex =
                            Some(ctx.load_texture(format!("screen-{}", sv.key), color, egui::TextureOptions::LINEAR))
                    }
                }
                sv.shown_seq = cur_seq;
                sv.last_frame_at = Some(Instant::now());
            }
        }
        let since = sv.fps_at.elapsed().as_secs_f32();
        if since >= 1.0 {
            sv.fps = frames.saturating_sub(sv.fps_frames) as f32 / since;
            sv.fps_frames = frames;
            sv.fps_at = Instant::now();
        }
        let live = sv.last_frame_at.is_some_and(|t| t.elapsed() < Duration::from_secs(4));

        let mut out: Vec<serde_json::Value> = Vec::new();
        let mut pic: Option<(egui::Rect, egui::LayerId)> = None;
        if sv.fullscreen {
            let screen = ctx.screen_rect();
            egui::Area::new(egui::Id::new(("screen-full", sv.key.clone())))
                .order(egui::Order::Foreground)
                .fixed_pos(screen.min)
                .show(ctx, |ui| {
                    ui.painter().rect_filled(screen, 0.0, Color32::BLACK);
                    ui.set_min_size(screen.size());
                    egui::Frame::none().inner_margin(Margin::same(8.0)).show(ui, |ui| {
                        ui.set_width(screen.width() - 16.0);
                        screen_toolbar(ui, sv, live, dims, ctl_ready, &ctl_error, &mut out, actions);
                        let h = (screen.max.y - ui.cursor().min.y - 10.0).max(80.0);
                        pic = screen_picture(ui, sv, &error, h).map(|r| (r, ui.layer_id()));
                    });
                });
        } else {
            let mut open = true;
            let title = if sv.mons > 1 {
                format!("{} — monitor {}", sv.peer_name, sv.monitor + 1)
            } else {
                sv.peer_name.clone()
            };
            egui::Window::new(RichText::new(title).size(16.0).strong())
                .id(egui::Id::new(("screen", sv.key.clone())))
                .open(&mut open)
                .collapsible(false)
                .default_size(Vec2::new(960.0, 640.0))
                .default_pos(Pos2::new(220.0 + 40.0 * i as f32, 50.0 + 40.0 * i as f32))
                .frame(egui::Frame::window(&ctx.style()).fill(Color32::from_rgb(8, 12, 10)))
                .show(ctx, |ui| {
                    screen_toolbar(ui, sv, live, dims, ctl_ready, &ctl_error, &mut out, actions);
                    let h = (ui.available_height() - 4.0).max(120.0);
                    pic = screen_picture(ui, sv, &error, h).map(|r| (r, ui.layer_id()));
                });
            if !open {
                actions.push(Action::CloseScreen(sv.key.clone()));
            }
        }
        if let Some((rect, layer)) = pic {
            screen_input(ctx, sv, rect, layer, &mut out, actions);
        }
        if let Some(tx) = &sv.control {
            for mut v in out {
                // Tell the other PC which of its monitors this position is on.
                if let Some(o) = v.as_object_mut() {
                    o.insert("m".into(), serde_json::json!(sv.monitor));
                }
                let _ = tx.send(v);
            }
        }
    }
}

impl NexusApp {
    /// The "app list and monitoring" window for one device.
    fn monitor_window(&mut self, ctx: &egui::Context, i: usize, actions: &mut Vec<Action>) {
        let mv = &mut self.monitors[i];
        let (snap, cpu_hist, ram_hist, error, notice) = {
            let g = mv.inner.lock().unwrap_or_else(|e| e.into_inner());
            (g.snap.clone(), g.cpu_hist.clone(), g.ram_hist.clone(), g.error.clone(), g.notice.clone())
        };
        let mut open = true;
        egui::Window::new(RichText::new(format!("{} — apps and monitoring", mv.peer_name)).size(16.0).strong())
            .id(egui::Id::new(("monitor", mv.peer_id.clone())))
            .open(&mut open)
            .collapsible(false)
            .default_size(Vec2::new(640.0, 660.0))
            .default_pos(Pos2::new(260.0 + 30.0 * i as f32, 60.0 + 30.0 * i as f32))
            .show(ctx, |ui| {
                if let Some(e) = &error {
                    ui.colored_label(BAD, e);
                }
                let Some(s) = &snap else {
                    if error.is_none() {
                        ui.label(RichText::new("Connecting…").color(OFF));
                    }
                    return;
                };
                ui.horizontal_wrapped(|ui| {
                    if let Some(cpu) = s.get("cpu").and_then(|v| v.as_f64()) {
                        stat_card(ui, "CPU", format!("{cpu:.0}%"), &cpu_hist, ONLINE);
                    }
                    let used = s.get("ram_used").and_then(|v| v.as_u64()).unwrap_or(0);
                    let total = s.get("ram_total").and_then(|v| v.as_u64()).unwrap_or(0);
                    if total > 0 {
                        stat_card(
                            ui,
                            "Memory",
                            format!("{} of {}", human_size(used), human_size(total)),
                            &ram_hist,
                            RING,
                        );
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    if let Some(up) = s.get("uptime").and_then(|v| v.as_u64()) {
                        ui.label(RichText::new(format!("On for {}", uptime_text(up))).color(OFF));
                    }
                    if let Some(b) = s.get("battery").and_then(|v| v.as_f64()) {
                        ui.label(RichText::new(format!("· Battery {b:.0}%")).color(OFF));
                    }
                    if let Some(n) = s.get("cores").and_then(|v| v.as_u64()) {
                        ui.label(RichText::new(format!("· {n} CPU cores")).color(OFF));
                    }
                });
                if let Some(disks) = s.get("disks").and_then(|v| v.as_array()) {
                    for d in disks {
                        let total = d.get("total").and_then(|v| v.as_u64()).unwrap_or(0);
                        let free = d.get("free").and_then(|v| v.as_u64()).unwrap_or(0);
                        if total == 0 {
                            continue;
                        }
                        let name = d.get("name").and_then(|v| v.as_str()).unwrap_or("Disk");
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(format!("Disk {name}")).strong());
                            ui.add(
                                egui::ProgressBar::new(1.0 - free as f32 / total as f32)
                                    .desired_width(220.0)
                                    .text(format!("{} free of {}", human_size(free), human_size(total))),
                            );
                        });
                    }
                }
                if let Some(note) = s.get("note").and_then(|v| v.as_str()) {
                    ui.add_space(6.0);
                    ui.label(RichText::new(note).color(OFF));
                }
                let apps: Vec<&serde_json::Value> =
                    s.get("apps").and_then(|v| v.as_array()).map(|a| a.iter().collect()).unwrap_or_default();
                if apps.is_empty() {
                    return;
                }
                ui.add_space(8.0);
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("RUNNING APPS ({})", apps.len())).size(14.0).strong());
                    ui.add(egui::TextEdit::singleline(&mut mv.search).hint_text("Search").desired_width(160.0));
                    ui.label(RichText::new("Sort by").color(OFF));
                    if ui.selectable_label(!mv.by_cpu, "Memory").clicked() {
                        mv.by_cpu = false;
                    }
                    if ui.selectable_label(mv.by_cpu, "CPU").clicked() {
                        mv.by_cpu = true;
                    }
                });
                if let Some(n) = &notice {
                    ui.label(RichText::new(n).color(ONLINE));
                }
                let can_end = s.get("can_end").and_then(|v| v.as_bool()).unwrap_or(false);
                if !can_end {
                    ui.label(
                        RichText::new("Ending apps is off — turn on \"Allow remote control\" in that PC's Settings.")
                            .size(12.0)
                            .color(OFF),
                    );
                }
                let needle = mv.search.to_lowercase();
                let mut shown: Vec<&serde_json::Value> = apps
                    .into_iter()
                    .filter(|a| {
                        needle.is_empty()
                            || a.get("name")
                                .and_then(|n| n.as_str())
                                .is_some_and(|n| n.to_lowercase().contains(&needle))
                    })
                    .collect();
                let key = |a: &serde_json::Value, k: &str| a.get(k).and_then(|v| v.as_f64()).unwrap_or(0.0);
                if mv.by_cpu {
                    shown.sort_by(|a, b| key(b, "cpu").total_cmp(&key(a, "cpu")));
                } else {
                    shown.sort_by(|a, b| key(b, "mem").total_cmp(&key(a, "mem")));
                }
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    egui::Grid::new(("apps", mv.peer_id.clone()))
                        .num_columns(4)
                        .striped(true)
                        .spacing([18.0, 6.0])
                        .show(ui, |ui| {
                            ui.label(RichText::new("App").color(OFF));
                            ui.label(RichText::new("CPU").color(OFF));
                            ui.label(RichText::new("Memory").color(OFF));
                            ui.label("");
                            ui.end_row();
                            for a in shown {
                                let name = a.get("name").and_then(|n| n.as_str()).unwrap_or("?");
                                let count = a.get("count").and_then(|c| c.as_u64()).unwrap_or(1);
                                let label = if count > 1 { format!("{name}  ×{count}") } else { name.to_string() };
                                ui.label(RichText::new(label).strong());
                                ui.label(format!("{:.1}%", key(a, "cpu")));
                                ui.label(human_size(key(a, "mem") as u64));
                                let protected = a.get("protected").and_then(|p| p.as_bool()).unwrap_or(false);
                                if can_end && !protected {
                                    if ui.small_button("End").clicked() {
                                        mv.confirm_end = Some(name.to_string());
                                    }
                                } else {
                                    ui.label("");
                                }
                                ui.end_row();
                            }
                        });
                });
            });
        if let Some(name) = mv.confirm_end.clone() {
            egui::Window::new("End this app?")
                .id(egui::Id::new(("end-app", mv.peer_id.clone())))
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(format!("End {name} on {}? Anything unsaved in it will be lost.", mv.peer_name));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.add(primary("End app")).clicked() {
                            actions.push(Action::EndApp(mv.peer_id.clone(), name.clone()));
                            mv.confirm_end = None;
                        }
                        if ui.add(secondary("Cancel")).clicked() {
                            mv.confirm_end = None;
                        }
                    });
                });
        }
        if !open {
            actions.push(Action::CloseMonitor(mv.peer_id.clone()));
        }
    }
}

/// A small box with a big number and a line graph of the last few minutes.
fn stat_card(ui: &mut egui::Ui, title: &str, value: String, hist: &std::collections::VecDeque<f32>, color: Color32) {
    box_frame().show(ui, |ui| {
        ui.set_width(250.0);
        ui.label(RichText::new(title).size(12.0).color(OFF));
        ui.label(RichText::new(value).size(20.0).strong());
        let (rect, _) = ui.allocate_exact_size(Vec2::new(250.0, 46.0), Sense::hover());
        ui.painter().rect_filled(rect, 4.0, Color32::from_rgb(6, 10, 8));
        if hist.len() >= 2 {
            let n = hist.len();
            let points: Vec<Pos2> = hist
                .iter()
                .enumerate()
                .map(|(k, v)| {
                    let x = rect.right() - (n - 1 - k) as f32 / (HISTORY - 1) as f32 * rect.width();
                    let y = rect.bottom() - 2.0 - v.clamp(0.0, 100.0) / 100.0 * (rect.height() - 4.0);
                    Pos2::new(x, y)
                })
                .collect();
            ui.painter().add(egui::Shape::line(points, Stroke::new(1.5, color)));
        }
    });
}

fn uptime_text(secs: u64) -> String {
    let (d, h, m) = (secs / 86_400, secs / 3600 % 24, secs / 60 % 60);
    if d > 0 {
        format!("{d} d {h} h")
    } else if h > 0 {
        format!("{h} h {m} min")
    } else {
        format!("{m} min")
    }
}

/// Starts a picture stream for a screen viewer; `stop` ends it.
fn spawn_stream(
    me: Arc<Identity>,
    peer: PairedPeer,
    addr: SocketAddr,
    inner: Arc<Mutex<ScreenInner>>,
    stop: Arc<AtomicBool>,
    quality: Quality,
    monitor: usize,
) {
    let (fps, max_w, q) = quality.params();
    std::thread::spawn(move || {
        let frames = inner.clone();
        let keep = stop.clone();
        let res = net::screen_stream(
            &me,
            &peer,
            addr,
            monitor,
            fps,
            max_w,
            q,
            move |f| {
                let mut g = frames.lock().unwrap_or_else(|e| e.into_inner());
                g.mons = f.mons;
                g.jpeg = Some(f.jpeg);
                g.seq = f.seq;
                g.dims = (f.w, f.h);
                g.frames += 1;
                g.error = None;
            },
            move || !keep.load(Ordering::Relaxed),
        );
        if let Err(e) = res {
            if !stop.load(Ordering::Relaxed) {
                let mut g = inner.lock().unwrap_or_else(|e| e.into_inner());
                if !g.closed {
                    g.error = Some(e);
                }
            }
        }
    });
}

fn clamp_center(c: Pos2, zoom: f32) -> Pos2 {
    let h = 0.5 / zoom;
    Pos2::new(c.x.clamp(h, 1.0 - h), c.y.clamp(h, 1.0 - h))
}

/// The part of the picture currently shown, in fractions of the picture.
fn view_uv(sv: &ScreenView) -> egui::Rect {
    egui::Rect::from_center_size(sv.center, Vec2::splat(1.0 / sv.zoom))
}

/// Zooms by `factor`, keeping the point under `anchor` (fraction of the shown area) still.
fn zoom_by(sv: &mut ScreenView, factor: f32, anchor: Option<Vec2>) {
    let old = sv.zoom;
    let new = (old * factor).clamp(1.0, 8.0);
    if (new - old).abs() < 1e-4 {
        return;
    }
    let a = anchor.unwrap_or(Vec2::splat(0.5));
    let point = view_uv(sv).min + a / old;
    let min_new = point - a / new;
    sv.zoom = new;
    sv.center = clamp_center(min_new + Vec2::splat(0.5 / new), new);
}

#[allow(clippy::too_many_arguments)]
fn screen_toolbar(
    ui: &mut egui::Ui,
    sv: &mut ScreenView,
    live: bool,
    dims: (u32, u32),
    ctl_ready: bool,
    ctl_error: &Option<String>,
    out: &mut Vec<serde_json::Value>,
    actions: &mut Vec<Action>,
) {
    ui.horizontal_wrapped(|ui| {
        let (dot, state) = if live { (ONLINE, "LIVE") } else { (OFF, "WAITING") };
        ui.label(RichText::new(format!("● {state}")).size(12.0).strong().color(dot));
        if dims.0 > 0 {
            ui.label(RichText::new(format!("{}×{} · {:.0} fps", dims.0, dims.1, sv.fps)).size(12.0).color(OFF));
        }
        ui.separator();
        ui.label(RichText::new("Quality").size(12.0).color(OFF));
        for q in Quality::ALL {
            if ui.selectable_label(sv.quality == q, q.label()).clicked() && sv.quality != q {
                actions.push(Action::SetQuality(sv.key.clone(), q));
            }
        }
        ui.separator();
        ui.label(RichText::new("Zoom").size(12.0).color(OFF));
        if ui.small_button(" − ").clicked() {
            zoom_by(sv, 1.0 / 1.25, None);
        }
        let zoom_text = if sv.zoom <= 1.001 { "Fit".to_string() } else { format!("{:.1}×", sv.zoom) };
        ui.label(RichText::new(zoom_text).size(12.0));
        if ui.small_button(" + ").clicked() {
            zoom_by(sv, 1.25, None);
        }
        if sv.zoom > 1.001 && ui.small_button("Fit").clicked() {
            sv.zoom = 1.0;
            sv.center = Pos2::new(0.5, 0.5);
        }
        ui.separator();
        if ui.checkbox(&mut sv.control_on, "Control").changed() && !sv.control_on {
            sv.typing = false;
        }
        let label = if sv.fullscreen { "Exit fullscreen" } else { "Fullscreen" };
        if ui.add(secondary(label)).clicked() {
            actions.push(Action::SetFullscreen(sv.key.clone(), !sv.fullscreen));
        }
        if sv.mons > 1 {
            ui.separator();
            ui.label(RichText::new("Monitor").size(12.0).color(OFF));
            for m in 0..sv.mons {
                if ui.selectable_label(sv.monitor == m, format!(" {} ", m + 1)).clicked() && sv.monitor != m {
                    actions.push(Action::SwitchMonitor(sv.key.clone(), m));
                }
            }
            if ui.small_button("Show all").on_hover_text("Open every monitor of this PC").clicked() {
                actions.push(Action::ViewAllMonitors(sv.peer_id.clone()));
            }
        }
        if sv.is_pc {
            ui.separator();
            let mut on = sv.sound_stop.is_some();
            if ui.checkbox(&mut on, "🔊 Sound").on_hover_text("Hear what that device is playing").changed() {
                actions.push(Action::SetSound(sv.key.clone(), on));
            }
            let err = sv.inner.lock().unwrap_or_else(|e| e.into_inner()).sound_error.clone();
            if let Some(e) = err {
                ui.label(RichText::new(e).size(12.0).color(BAD));
            }
        }
    });
    if sv.control_on {
        ui.horizontal_wrapped(|ui| {
            let shortcuts: [(&str, &str, &[&str]); 4] = [
                ("Start / Home", "win", &[]),
                ("Alt+Tab / Recent apps", "tab", &["alt"]),
                ("Task Manager", "escape", &["ctrl", "shift"]),
                ("Esc / Back", "escape", &[]),
            ];
            for (label, key, mods) in shortcuts {
                if ui.small_button(label).clicked() {
                    out.push(serde_json::json!({ "op": "key", "k": key, "mods": mods }));
                }
            }
            let status = if let Some(e) = ctl_error {
                RichText::new(e.as_str()).color(BAD)
            } else if !ctl_ready {
                RichText::new("Connecting control…").color(OFF)
            } else if sv.typing {
                RichText::new("Your typing goes to this screen").color(ONLINE)
            } else {
                RichText::new("Click the screen to type into it · Ctrl+wheel zooms").color(OFF)
            };
            ui.label(status.size(12.0));
        });
    } else {
        ui.label(RichText::new("Wheel zooms · drag to move around when zoomed").size(12.0).color(OFF));
    }
    ui.add_space(4.0);
}

/// Draws the picture (or a status message) and returns where the picture is.
fn screen_picture(ui: &mut egui::Ui, sv: &ScreenView, error: &Option<String>, height: f32) -> Option<egui::Rect> {
    let (area, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::click_and_drag());
    ui.painter().rect_filled(area, 8.0, Color32::from_rgb(3, 5, 4));
    if let Some(tex) = &sv.tex {
        let [tw, th] = tex.size();
        let (tw, th) = (tw as f32, th as f32);
        let scale = (area.width() / tw).min(area.height() / th);
        let rect = egui::Rect::from_center_size(area.center(), Vec2::new(tw * scale, th * scale));
        egui::Image::new(egui::load::SizedTexture::new(tex.id(), rect.size())).uv(view_uv(sv)).paint_at(ui, rect);
        if sv.control_on && sv.typing {
            ui.painter().rect_stroke(rect.expand(1.0), 2.0, Stroke::new(2.0, ONLINE));
        }
        if let Some(e) = error {
            ui.painter().text(
                rect.center_bottom() - Vec2::new(0.0, 18.0),
                egui::Align2::CENTER_CENTER,
                e,
                FontId::proportional(14.0),
                BAD,
            );
        }
        return Some(rect);
    }
    let (msg, color) = match error {
        Some(e) => (e.clone(), BAD),
        None => ("Connecting to the screen…".to_string(), OFF),
    };
    ui.painter().text(area.center(), egui::Align2::CENTER_CENTER, msg, FontId::proportional(15.0), color);
    None
}

/// Zoom, pan and (with "Control" on) mouse/keyboard forwarding for one viewer.
fn screen_input(
    ctx: &egui::Context,
    sv: &mut ScreenView,
    rect: egui::Rect,
    layer: egui::LayerId,
    out: &mut Vec<serde_json::Value>,
    actions: &mut Vec<Action>,
) {
    let here = |p: Pos2| rect.contains(p) && ctx.layer_id_at(p) == Some(layer);
    let frac = |p: Pos2| {
        Vec2::new(
            ((p.x - rect.min.x) / rect.width()).clamp(0.0, 1.0),
            ((p.y - rect.min.y) / rect.height()).clamp(0.0, 1.0),
        )
    };
    let (events, hover, scroll, zoom_delta, ctrl, delta) = ctx.input(|i| {
        (
            i.events.clone(),
            i.pointer.hover_pos(),
            i.raw_scroll_delta,
            i.zoom_delta(),
            i.modifiers.ctrl,
            i.pointer.delta(),
        )
    });
    let over = hover.is_some_and(|p| here(p));
    if over {
        let mut factor = zoom_delta;
        if !sv.control_on && scroll.y != 0.0 {
            factor *= (scroll.y / 250.0).exp();
        }
        if (factor - 1.0).abs() > 1e-4 {
            zoom_by(sv, factor, hover.map(frac));
        }
    }
    for ev in events {
        let uv = view_uv(sv);
        let norm = |p: Pos2| {
            let f = frac(p);
            (uv.min.x + f.x * uv.width(), uv.min.y + f.y * uv.height())
        };
        match ev {
            egui::Event::PointerMoved(p) => {
                if sv.control_on && !sv.panning && (here(p) || sv.pressed) {
                    let n = norm(p);
                    if sv.last_move != Some(n) {
                        sv.last_move = Some(n);
                        out.push(serde_json::json!({ "op": "move", "x": n.0, "y": n.1 }));
                    }
                }
            }
            egui::Event::PointerButton { pos, button, pressed, .. } => {
                if pressed && !here(pos) {
                    if button == egui::PointerButton::Primary {
                        sv.typing = false;
                    }
                    continue;
                }
                if !sv.control_on {
                    if button == egui::PointerButton::Primary {
                        sv.panning = pressed && sv.zoom > 1.001;
                    }
                    continue;
                }
                if !pressed && !sv.pressed {
                    continue;
                }
                let b = match button {
                    egui::PointerButton::Primary => "left",
                    egui::PointerButton::Secondary => "right",
                    egui::PointerButton::Middle => "middle",
                    _ => continue,
                };
                if pressed {
                    sv.typing = true;
                }
                sv.pressed = pressed;
                let n = norm(pos);
                out.push(serde_json::json!({ "op": "button", "b": b, "down": pressed, "x": n.0, "y": n.1 }));
            }
            egui::Event::Key { key: egui::Key::Escape, pressed: true, .. } if !sv.typing && sv.fullscreen => {
                actions.push(Action::SetFullscreen(sv.key.clone(), false));
            }
            egui::Event::Text(t) if sv.control_on && sv.typing => out.push(serde_json::json!({ "op": "text", "s": t })),
            egui::Event::Paste(t) if sv.control_on && sv.typing => {
                out.push(serde_json::json!({ "op": "text", "s": t }))
            }
            egui::Event::Copy if sv.control_on && sv.typing => {
                out.push(serde_json::json!({ "op": "key", "k": "c", "mods": ["ctrl"] }))
            }
            egui::Event::Cut if sv.control_on && sv.typing => {
                out.push(serde_json::json!({ "op": "key", "k": "x", "mods": ["ctrl"] }))
            }
            egui::Event::Key { key, pressed: true, modifiers, .. } if sv.control_on && sv.typing => {
                if let Some(name) = remote_key_name(key, modifiers) {
                    let mut mods = Vec::new();
                    if modifiers.ctrl {
                        mods.push("ctrl");
                    }
                    if modifiers.alt {
                        mods.push("alt");
                    }
                    if modifiers.shift {
                        mods.push("shift");
                    }
                    out.push(serde_json::json!({ "op": "key", "k": name, "mods": mods }));
                }
            }
            _ => {}
        }
    }
    if sv.panning {
        let moved = Vec2::new(delta.x / rect.width(), delta.y / rect.height()) / sv.zoom;
        sv.center = clamp_center(sv.center - moved, sv.zoom);
    }
    if sv.control_on && over && !ctrl && scroll.y != 0.0 {
        sv.scroll_acc += scroll.y;
        let notches = (sv.scroll_acc / 40.0).trunc();
        if notches != 0.0 {
            sv.scroll_acc -= notches * 40.0;
            out.push(serde_json::json!({ "op": "scroll", "dy": notches }));
        }
    }
}

/// Name of a key to forward to the other device. Plain letters and digits are
/// skipped (they arrive as typed text) unless Ctrl or Alt is held; Ctrl+C/X/V are
/// skipped because they arrive as Copy/Cut/Paste.
fn remote_key_name(key: egui::Key, m: egui::Modifiers) -> Option<String> {
    use egui::Key as K;
    let special = match key {
        K::Enter => "enter",
        K::Backspace => "backspace",
        K::Tab => "tab",
        K::Escape => "escape",
        K::ArrowLeft => "left",
        K::ArrowRight => "right",
        K::ArrowUp => "up",
        K::ArrowDown => "down",
        K::Delete => "delete",
        K::Insert => "insert",
        K::Home => "home",
        K::End => "end",
        K::PageUp => "pageup",
        K::PageDown => "pagedown",
        _ => "",
    };
    if !special.is_empty() {
        return Some(special.into());
    }
    let name = format!("{key:?}");
    if name.len() > 1 && name.starts_with('F') && name[1..].chars().all(|c| c.is_ascii_digit()) {
        return Some(name.to_ascii_lowercase());
    }
    if !(m.ctrl || m.alt) {
        return None;
    }
    if name.len() == 1 && name.chars().all(|c| c.is_ascii_alphabetic()) {
        let l = name.to_ascii_lowercase();
        if m.ctrl && !m.alt && matches!(l.as_str(), "c" | "x" | "v") {
            return None;
        }
        return Some(l);
    }
    if let Some(d) = name.strip_prefix("Num").filter(|d| d.len() == 1) {
        return Some(d.into());
    }
    if key == K::Space {
        return Some("space".into());
    }
    None
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

fn pair_action(n: &Near) -> Action {
    Action::Pair(PairTarget {
        addr: n.addr,
        name: n.name.clone(),
        expected_id: Some(n.id.clone()),
        expected_key: Some(n.public_key.clone()),
    })
}

/// Lock / Sleep / Restart / Shut down when the device is online; Wake when it is offline and its MAC is known.
fn power_controls(ui: &mut egui::Ui, r: &Row, confirm_cmd: Option<&(String, Command)>, actions: &mut Vec<Action>) {
    // Phones can't be locked/restarted/woken remotely, so only offer this for computers.
    if matches!(r.peer.os.as_str(), "Android" | "iOS") {
        return;
    }
    let id = r.peer.id.clone();
    if let Some((cid, cmd)) = confirm_cmd {
        if *cid == id {
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("{} this device now?", cmd.label())).strong());
                if ui
                    .add(
                        egui::Button::new(
                            RichText::new(format!("Yes, {}", cmd.label().to_lowercase())).strong().color(TEXT),
                        )
                        .fill(BAD.gamma_multiply(0.85)),
                    )
                    .clicked()
                {
                    actions.push(Action::RunCommand(id.clone(), *cmd));
                }
                if ui.add(secondary("Cancel")).clicked() {
                    actions.push(Action::CancelCommand);
                }
            });
            return;
        }
    }
    if is_online(&r.status) {
        ui.horizontal(|ui| {
            if ui.add(secondary("Lock")).clicked() {
                actions.push(Action::RunCommand(id.clone(), Command::Lock));
            }
            if ui.add(secondary("Sleep")).on_hover_text("Put it to sleep. Wake it again with Wake.").clicked() {
                actions.push(Action::RunCommand(id.clone(), Command::Sleep));
            }
            if ui.add(secondary("Restart")).clicked() {
                actions.push(Action::AskCommand(id.clone(), Command::Restart));
            }
            if ui
                .add(
                    egui::Button::new(RichText::new("Shut down").strong().color(TEXT))
                        .fill(BOX2)
                        .stroke(Stroke::new(1.0, BAD)),
                )
                .clicked()
            {
                actions.push(Action::AskCommand(id.clone(), Command::Shutdown));
            }
        });
    } else if !r.peer.macs.is_empty() {
        if ui.add(primary("Wake")).clicked() {
            actions.push(Action::Wake(id));
        }
    }
}

fn remove_controls(ui: &mut egui::Ui, r: &Row, confirm: Option<&str>, actions: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        if confirm == Some(r.peer.id.as_str()) {
            ui.label("Remove this device?");
            if ui
                .add(egui::Button::new(RichText::new("Remove").strong().color(TEXT)).fill(BAD.gamma_multiply(0.8)))
                .clicked()
            {
                actions.push(Action::Remove(r.peer.id.clone()));
            }
            if ui.add(secondary("Cancel")).clicked() {
                actions.push(Action::CancelRemove);
            }
        } else if ui.add(secondary("Remove device")).clicked() {
            actions.push(Action::AskRemove(r.peer.id.clone()));
        }
    });
}

fn device_card(
    ui: &mut egui::Ui,
    r: &Row,
    confirm: Option<&str>,
    confirm_cmd: Option<&(String, Command)>,
    actions: &mut Vec<Action>,
) {
    let online = is_online(&r.status);
    let (color, text) = status_line(&r.status);
    let stats = stats_of(&r.status).cloned().unwrap_or_default();
    let resp = box_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new(r.peer.name.to_uppercase()).size(18.0).strong());
                ui.label(
                    RichText::new(format!("{} · {}", r.peer.os, r.addr.as_deref().unwrap_or("address unknown")))
                        .size(12.5),
                );
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                ui.label(RichText::new(text.to_uppercase()).size(11.5).strong().color(color));
            });
        });
        if online {
            ui.horizontal(|ui| {
                gauge(ui, stats.cpu, "CPU");
                ui.add_space(8.0);
                gauge(ui, stats.ram, "RAM");
                ui.add_space(8.0);
                gauge(ui, stats.battery, "BATTERY");
            });
        } else {
            egui::Frame::none()
                .stroke(Stroke::new(1.0, LINE))
                .rounding(Rounding::same(8.0))
                .inner_margin(Margin::symmetric(12.0, 26.0))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.vertical_centered(|ui| ui.label("Not reachable right now"));
                });
        }
        ui.label(RichText::new(format!("key {}", identity::fingerprint(&r.peer.public_key))).monospace().size(12.0));
        power_controls(ui, r, confirm_cmd, actions);
        if online && ui.add(secondary("Browse files")).clicked() {
            actions.push(Action::OpenBrowser(r.peer.id.clone()));
        }
        if online && ui.add(secondary("View screen")).clicked() {
            actions.push(Action::ViewScreen(r.peer.id.clone()));
        }
        if online && ui.add(secondary("Monitor")).clicked() {
            actions.push(Action::OpenMonitor(r.peer.id.clone()));
        }
        remove_controls(ui, r, confirm, actions);
    });
    // Teal stripe along the top of online cards.
    if online {
        let rect = resp.response.rect;
        ui.painter().line_segment(
            [Pos2::new(rect.left() + 10.0, rect.top() + 1.0), Pos2::new(rect.right() - 10.0, rect.top() + 1.0)],
            Stroke::new(3.0, ONLINE),
        );
    }
    ui.add_space(4.0);
}

fn activity(ui: &mut egui::Ui, log: &[(u64, String)]) {
    box_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        if log.is_empty() {
            ui.label("Nothing yet");
        }
        for (ago, msg) in log {
            ui.horizontal(|ui| {
                ui.label(RichText::new(fmt_ago(*ago)).monospace());
                dot(ui, ONLINE, 3.0);
                ui.label(msg);
            });
        }
    });
}

fn pair_dialog(ctx: &egui::Context, p: &PairSession, actions: &mut Vec<Action>) {
    let title = match p.role {
        Role::Outgoing => format!("Pair with {}", p.peer_name),
        Role::Incoming => format!("{} wants to pair", p.peer_name),
    };
    egui::Window::new(RichText::new(title).size(17.0).strong())
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
        .frame(box_frame().inner_margin(Margin::same(22.0)))
        .show(ctx, |ui| {
            ui.set_width(380.0);
            match &p.phase {
                Phase::Connecting => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(format!("Connecting to {}…", p.peer_name));
                    });
                    if ui.add(secondary("Cancel")).clicked() {
                        actions.push(Action::CloseSession);
                    }
                }
                Phase::Compare { code } | Phase::WaitingForOther { code } => {
                    ui.label(format!("Check that {} shows the same number:", p.peer_name));
                    egui::Frame::none()
                        .fill(BOX2)
                        .rounding(Rounding::same(12.0))
                        .inner_margin(Margin::same(14.0))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.vertical_centered(|ui| {
                                ui.label(
                                    RichText::new(format!("{} {}", &code[..3], &code[3..]))
                                        .monospace()
                                        .size(44.0)
                                        .strong(),
                                );
                            });
                        });
                    if matches!(p.phase, Phase::Compare { .. }) {
                        ui.columns(2, |c| {
                            let w = c[0].available_width();
                            if c[0].add(secondary("They don't match").min_size(Vec2::new(w, 40.0))).clicked() {
                                actions.push(Action::Answer(false));
                            }
                            let w = c[1].available_width();
                            if c[1].add(primary("Numbers match").min_size(Vec2::new(w, 40.0))).clicked() {
                                actions.push(Action::Answer(true));
                            }
                        });
                    } else {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(format!("Waiting for {} to confirm…", p.peer_name));
                        });
                        if ui.add(secondary("Cancel")).clicked() {
                            actions.push(Action::CloseSession);
                        }
                    }
                }
                Phase::Done => {
                    ui.label(RichText::new(format!("Paired with {}", p.peer_name)).color(ONLINE).strong().size(17.0));
                    ui.label("It now shows in your device list.");
                    if ui.add(primary("Done").min_size(Vec2::new(100.0, 36.0))).clicked() {
                        actions.push(Action::CloseSession);
                    }
                }
                Phase::Failed(e) => {
                    ui.label(RichText::new(e).color(BAD));
                    if ui.add(secondary("Close")).clicked() {
                        actions.push(Action::CloseSession);
                    }
                }
            }
        });
}

fn fmt_ago(secs: u64) -> String {
    match secs {
        0..=59 => format!("{secs:>3}s"),
        60..=3599 => format!("{:>3}m", secs / 60),
        _ => format!("{:>3}h", secs / 3600),
    }
}

/* ------------------------------------------------------------------ automation */

const AUTO_ACTIONS: [&str; 7] = ["Lock", "Sleep", "Restart", "Shut down", "Wake", "Open…", "End app"];

/// The automation being created or edited, in an easy-to-edit shape.
#[derive(Clone)]
struct AutoDraft {
    /// Position in the list being edited, or None for a new automation.
    index: Option<usize>,
    id: u64,
    name: String,
    enabled: bool,
    /// 0 = at a time, 1 = when idle, 2 = when Nexus starts.
    trigger: u8,
    hour: String,
    minute: String,
    days: [bool; 7],
    idle: String,
    /// Index into AUTO_ACTIONS.
    action: usize,
    text: String,
    target: Option<String>,
    error: Option<String>,
}

impl AutoDraft {
    fn new() -> Self {
        AutoDraft {
            index: None,
            id: 0,
            name: String::new(),
            enabled: true,
            trigger: 0,
            hour: "23".into(),
            minute: "00".into(),
            days: [true, true, true, true, true, false, false],
            idle: "30".into(),
            action: 1,
            text: String::new(),
            target: None,
            error: None,
        }
    }

    fn from_rule(index: usize, r: &automation::Rule) -> Self {
        let mut d = AutoDraft::new();
        d.index = Some(index);
        d.id = r.id;
        d.name = r.name.clone();
        d.enabled = r.enabled;
        d.target = r.target.clone();
        match &r.when {
            automation::When::Time { hour, minute, days } => {
                d.trigger = 0;
                d.hour = format!("{hour:02}");
                d.minute = format!("{minute:02}");
                d.days = [days.is_empty(); 7];
                for x in days {
                    if (*x as usize) < 7 {
                        d.days[*x as usize] = true;
                    }
                }
            }
            automation::When::Idle { minutes } => {
                d.trigger = 1;
                d.idle = minutes.to_string();
            }
            automation::When::Startup => d.trigger = 2,
        }
        d.action = match &r.action {
            automation::Act::Lock => 0,
            automation::Act::Sleep => 1,
            automation::Act::Restart => 2,
            automation::Act::Shutdown => 3,
            automation::Act::Wake => 4,
            automation::Act::Open { what } => {
                d.text = what.clone();
                5
            }
            automation::Act::EndApp { name } => {
                d.text = name.clone();
                6
            }
        };
        d
    }

    fn to_rule(&self) -> Result<automation::Rule, String> {
        let num = |s: &str, what: &str| s.trim().parse::<u32>().map_err(|_| format!("Write a number for {what}"));
        let when = match self.trigger {
            0 => {
                let hour = num(&self.hour, "the hour")?;
                let minute = num(&self.minute, "the minutes")?;
                if hour > 23 || minute > 59 {
                    return Err("The time must be between 00:00 and 23:59".into());
                }
                let days: Vec<u8> = (0..7u8).filter(|i| self.days[*i as usize]).collect();
                if days.is_empty() {
                    return Err("Pick at least one day".into());
                }
                let days = if days.len() == 7 { Vec::new() } else { days };
                automation::When::Time { hour: hour as u8, minute: minute as u8, days }
            }
            1 => automation::When::Idle { minutes: num(&self.idle, "the minutes")? },
            _ => automation::When::Startup,
        };
        let action = match self.action {
            0 => automation::Act::Lock,
            1 => automation::Act::Sleep,
            2 => automation::Act::Restart,
            3 => automation::Act::Shutdown,
            4 => automation::Act::Wake,
            5 => automation::Act::Open { what: self.text.clone() },
            _ => automation::Act::EndApp { name: self.text.clone() },
        };
        let target = if action.can_target() { self.target.clone() } else { None };
        Ok(automation::Rule { id: self.id, name: self.name.clone(), enabled: self.enabled, when, action, target })
    }
}

/// What the automation window asked for this frame.
enum AutoOp {
    Toggle(usize, bool),
    Delete(usize),
    Run(usize),
    Save,
}

impl NexusApp {
    /// The "Automation" window: this PC's rules, and the editor for one rule.
    fn automation_window(&mut self, ctx: &egui::Context) {
        let (rules, peers) = {
            let s = lock(&self.shared);
            (s.automations.clone(), s.paired.clone())
        };
        let computers: Vec<PairedPeer> =
            peers.iter().filter(|p| !matches!(p.os.as_str(), "Android" | "iOS")).cloned().collect();
        let mut open = true;
        let mut op: Option<AutoOp> = None;
        let mut close_editor = false;
        let mut new_draft: Option<AutoDraft> = None;
        egui::Window::new(RichText::new("Automation").size(17.0).strong())
            .id(egui::Id::new("automation"))
            .open(&mut open)
            .collapsible(false)
            .default_size(Vec2::new(620.0, 560.0))
            .default_pos(Pos2::new(240.0, 50.0))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    ui.label(
                        RichText::new(
                            "Rules this PC runs by itself, as long as Nexus is open. Results show up under Activity.",
                        )
                        .color(OFF),
                    );
                    ui.add_space(8.0);
                    if rules.is_empty() && self.auto_draft.is_none() {
                        box_frame().show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.label(RichText::new("No automations yet").strong());
                            ui.label("For example: every weekday at 23:00, put this PC to sleep.");
                        });
                    }
                    for (i, r) in rules.iter().enumerate() {
                        box_frame().show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                let mut on = r.enabled;
                                if ui.checkbox(&mut on, "").on_hover_text("On / off").changed() {
                                    op = Some(AutoOp::Toggle(i, on));
                                }
                                ui.vertical(|ui| {
                                    ui.label(RichText::new(r.title(&peers)).strong());
                                    ui.label(
                                        RichText::new(format!("{} → {}", r.when.describe(), r.describe_action(&peers)))
                                            .size(13.0)
                                            .color(OFF),
                                    );
                                });
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if ui.small_button("Delete").clicked() {
                                        op = Some(AutoOp::Delete(i));
                                    }
                                    if ui.small_button("Edit").clicked() {
                                        new_draft = Some(AutoDraft::from_rule(i, r));
                                    }
                                    if ui.small_button("Run now").clicked() {
                                        op = Some(AutoOp::Run(i));
                                    }
                                });
                            });
                        });
                        ui.add_space(4.0);
                    }
                    ui.add_space(6.0);
                    let Some(d) = self.auto_draft.as_mut() else {
                        if ui.add(primary("+  New automation")).clicked() {
                            new_draft = Some(AutoDraft::new());
                        }
                        return;
                    };
                    box_frame().show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(
                            RichText::new(if d.index.is_some() { "EDIT AUTOMATION" } else { "NEW AUTOMATION" })
                                .size(14.0)
                                .strong(),
                        );
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            ui.label("Name");
                            ui.add(egui::TextEdit::singleline(&mut d.name).hint_text("Optional").desired_width(260.0));
                        });
                        ui.add_space(6.0);
                        ui.label(RichText::new("WHEN").size(12.0).color(ONLINE));
                        ui.horizontal_wrapped(|ui| {
                            for (k, label) in
                                ["At a time", "When nobody uses this PC", "When Nexus starts"].iter().enumerate()
                            {
                                if ui.selectable_label(d.trigger == k as u8, *label).clicked() {
                                    d.trigger = k as u8;
                                }
                            }
                        });
                        match d.trigger {
                            0 => {
                                ui.horizontal(|ui| {
                                    ui.label("Time");
                                    ui.add(egui::TextEdit::singleline(&mut d.hour).desired_width(30.0));
                                    ui.label(":");
                                    ui.add(egui::TextEdit::singleline(&mut d.minute).desired_width(30.0));
                                    ui.label(RichText::new("(24-hour clock)").color(OFF));
                                });
                                ui.horizontal_wrapped(|ui| {
                                    for (k, name) in automation::DAY_NAMES.iter().enumerate() {
                                        if ui.selectable_label(d.days[k], *name).clicked() {
                                            d.days[k] = !d.days[k];
                                        }
                                    }
                                    ui.add_space(8.0);
                                    if ui.small_button("Every day").clicked() {
                                        d.days = [true; 7];
                                    }
                                    if ui.small_button("Weekdays").clicked() {
                                        d.days = [true, true, true, true, true, false, false];
                                    }
                                });
                            }
                            1 => {
                                ui.horizontal(|ui| {
                                    ui.label("After");
                                    ui.add(egui::TextEdit::singleline(&mut d.idle).desired_width(50.0));
                                    ui.label("minutes without mouse or keyboard");
                                });
                            }
                            _ => {
                                ui.label(RichText::new("Runs once each time Nexus starts.").color(OFF));
                            }
                        }
                        ui.add_space(6.0);
                        ui.label(RichText::new("DO THIS").size(12.0).color(ONLINE));
                        ui.horizontal_wrapped(|ui| {
                            for (k, label) in AUTO_ACTIONS.iter().enumerate() {
                                if ui.selectable_label(d.action == k, *label).clicked() {
                                    d.action = k;
                                }
                            }
                        });
                        match d.action {
                            0..=4 => {
                                ui.horizontal_wrapped(|ui| {
                                    ui.label("On");
                                    if d.action != 4 && ui.selectable_label(d.target.is_none(), "This PC").clicked() {
                                        d.target = None;
                                    }
                                    for p in &computers {
                                        if ui
                                            .selectable_label(
                                                d.target.as_deref() == Some(p.id.as_str()),
                                                p.name.as_str(),
                                            )
                                            .clicked()
                                        {
                                            d.target = Some(p.id.clone());
                                        }
                                    }
                                });
                                if d.action == 4 && computers.is_empty() {
                                    ui.label(
                                        RichText::new("Pair another computer first — Wake is for other computers.")
                                            .color(OFF),
                                    );
                                }
                            }
                            5 => {
                                ui.add(
                                    egui::TextEdit::singleline(&mut d.text)
                                        .hint_text("A program, file, folder or web address, e.g. notepad or https://…")
                                        .desired_width(f32::INFINITY),
                                );
                            }
                            _ => {
                                ui.add(
                                    egui::TextEdit::singleline(&mut d.text)
                                        .hint_text("App name as in Apps and monitoring, e.g. chrome.exe")
                                        .desired_width(f32::INFINITY),
                                );
                            }
                        }
                        if let Some(e) = &d.error {
                            ui.add_space(4.0);
                            ui.colored_label(BAD, e);
                        }
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if ui.add(primary("Save")).clicked() {
                                op = Some(AutoOp::Save);
                            }
                            if ui.add(secondary("Cancel")).clicked() {
                                close_editor = true;
                            }
                        });
                    });
                });
            });
        if let Some(d) = new_draft {
            self.auto_draft = Some(d);
        }
        if close_editor {
            self.auto_draft = None;
        }
        if !open {
            self.auto_open = false;
            self.auto_draft = None;
        }
        let Some(op) = op else { return };
        let mut list = rules.clone();
        match op {
            AutoOp::Run(i) => {
                if let Some(r) = rules.get(i) {
                    automation::run_rule(&self.me, &self.shared, r, " (Run now)");
                }
                return;
            }
            AutoOp::Toggle(i, on) => {
                if let Some(r) = list.get_mut(i) {
                    r.enabled = on;
                }
            }
            AutoOp::Delete(i) => {
                if i < list.len() {
                    list.remove(i);
                }
                // The open editor may point at a moved entry.
                self.auto_draft = None;
            }
            AutoOp::Save => {
                let Some(d) = self.auto_draft.as_mut() else { return };
                let rule = match d.to_rule() {
                    Ok(r) => r,
                    Err(e) => {
                        d.error = Some(e);
                        return;
                    }
                };
                match d.index {
                    Some(i) if i < list.len() => list[i] = rule,
                    _ => list.push(rule),
                }
            }
        }
        let saving = matches!(self.auto_draft, Some(_));
        match automation::replace(&self.shared, &self.dir, list) {
            Ok(_) => {
                if saving {
                    self.auto_draft = None;
                }
            }
            Err(e) => match self.auto_draft.as_mut() {
                Some(d) => d.error = Some(e),
                None => lock(&self.shared).log(format!("Could not save automations: {e}")),
            },
        }
    }
}
