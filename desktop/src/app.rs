//! The window. Two modes the user switches between at the top:
//!   Simple   — one list, one button per device (the default)
//!   Detailed — sidebar, summary tiles, device cards with CPU/RAM/battery gauges
//! Look: black background, dark green boxes, teal buttons, dark orange gauges, white text.

use crate::{
    identity::{self, Identity},
    net::{self, PairTarget},
    state::{lock, PairSession, Phase, Role, SharedState, Status},
    store::{self, PairedPeer},
    sysstats::Stats,
};
use eframe::egui::{self, Color32, FontId, Margin, Pos2, RichText, Rounding, Sense, Stroke, TextStyle, Vec2};
use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

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
    details: Option<String>,
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
            details: None,
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
            }
        }
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

        for a in actions {
            self.apply(a);
        }
        let fast = session.is_some() || window.is_some();
        ctx.request_repaint_after(Duration::from_millis(if fast { 250 } else { 1000 }));
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
                ui.label(RichText::new(format!("{online} of {} devices online", rows.len())));
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
                ui.label(RichText::new("Coming next: Remote screen · Files · Monitoring · Security").size(12.0));
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
            ui.columns(cols, |c| {
                for (i, r) in rows.iter().enumerate() {
                    device_card(&mut c[i % cols], r, self.confirm_remove.as_deref(), actions);
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

    fn settings(&self, ui: &mut egui::Ui) {
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
            ui.label(
                "Only devices you paired by comparing the 6-digit number are trusted. Every status check is a signed \
                 challenge that only the real device can answer, and the CPU, RAM and battery numbers travel inside it.",
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
                remove_controls(ui, r, self.confirm_remove.as_deref(), actions);
            });
        if !open {
            actions.push(Action::ShowDetails(None));
        }
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

fn device_card(ui: &mut egui::Ui, r: &Row, confirm: Option<&str>, actions: &mut Vec<Action>) {
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
