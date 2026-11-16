//! The window: this device, devices you have paired, devices found nearby.

use crate::{
    identity::{self, Identity},
    net::{self, PairTarget},
    state::{lock, PairSession, Phase, Role, SharedState, Status},
    store::{self, PairedPeer},
};
use eframe::egui::{self, Color32, RichText};
use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

const GOOD: Color32 = Color32::from_rgb(46, 170, 112);
const WARN: Color32 = Color32::from_rgb(214, 152, 40);
const BAD: Color32 = Color32::from_rgb(222, 82, 70);
const GREY: Color32 = Color32::from_rgb(140, 148, 162);
const ACCENT: Color32 = Color32::from_rgb(92, 128, 245);

pub struct NexusApp {
    me: Arc<Identity>,
    shared: SharedState,
    dir: PathBuf,
    tcp_port: u16,
    startup_error: Option<String>,
    manual_addr: String,
    confirm_remove: Option<String>,
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
}

impl NexusApp {
    pub fn new(
        me: Arc<Identity>,
        shared: SharedState,
        dir: PathBuf,
        tcp_port: u16,
        startup_error: Option<String>,
    ) -> Self {
        Self { me, shared, dir, tcp_port, startup_error, manual_addr: String::new(), confirm_remove: None }
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

impl eframe::App for NexusApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Copy what we need, then release the lock before drawing.
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
                s.log.iter().take(40).map(|(t, m)| (t.elapsed().as_secs(), m.clone())).collect();
            (rows, nearby, window, s.pair.clone(), log)
        };
        let mut actions = Vec::new();

        egui::TopBottomPanel::top("me").show(ctx, |ui| {
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Nexus").size(22.0).strong().color(ACCENT));
                ui.separator();
                ui.vertical(|ui| {
                    ui.label(RichText::new(&self.me.name).strong().size(15.0));
                    ui.label(
                        RichText::new(format!(
                            "This PC · {} · key {} · port {}",
                            identity::os_label(),
                            identity::fingerprint(&self.me.public_hex()),
                            self.tcp_port
                        ))
                        .small()
                        .weak(),
                    );
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| match window {
                    Some((secs, tries)) => {
                        if ui.button("Stop").clicked() {
                            actions.push(Action::CloseWindow);
                        }
                        ui.vertical(|ui| {
                            ui.label(RichText::new("Pairing allowed").strong().color(GOOD));
                            ui.label(
                                RichText::new(format!(
                                    "Tap Pair on the other device · {}:{:02} left · {tries} requests",
                                    secs / 60,
                                    secs % 60
                                ))
                                .small()
                                .weak(),
                            );
                        });
                    }
                    None => {
                        let b = egui::Button::new(RichText::new("Allow pairing").strong())
                            .min_size(egui::vec2(130.0, 30.0));
                        if ui.add(b).clicked() {
                            actions.push(Action::OpenWindow);
                        }
                    }
                });
            });
            ui.add_space(10.0);
        });

        egui::TopBottomPanel::bottom("log").resizable(false).show(ctx, |ui| {
            egui::CollapsingHeader::new(format!("Activity ({})", log.len())).default_open(false).show(ui, |ui| {
                egui::ScrollArea::vertical().max_height(140.0).show(ui, |ui| {
                    for (ago, msg) in &log {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(fmt_ago(*ago)).monospace().weak().small());
                            ui.label(msg);
                        });
                    }
                    if log.is_empty() {
                        ui.label(RichText::new("Nothing yet").weak());
                    }
                });
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                if let Some(e) = &self.startup_error {
                    ui.colored_label(BAD, e);
                    ui.add_space(6.0);
                }

                ui.heading(format!("My devices ({})", rows.len()));
                ui.add_space(4.0);
                if rows.is_empty() {
                    ui.label(
                        RichText::new(
                            "No paired devices yet. Click “Allow pairing” on one device, then “Pair” on the other.",
                        )
                        .weak(),
                    );
                }
                for r in &rows {
                    card(ui, |ui| {
                        ui.horizontal(|ui| {
                            let (dot, text, detail) = status_text(&r.status);
                            ui.label(RichText::new("●").color(dot).size(16.0));
                            ui.vertical(|ui| {
                                ui.label(RichText::new(&r.peer.name).strong().size(15.0));
                                ui.label(
                                    RichText::new(format!(
                                        "{} · {} · key {}",
                                        r.peer.os,
                                        r.addr.as_deref().unwrap_or("address unknown"),
                                        identity::fingerprint(&r.peer.public_key)
                                    ))
                                    .small()
                                    .weak(),
                                );
                            });
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if self.confirm_remove.as_deref() == Some(r.peer.id.as_str()) {
                                    if ui.button("Cancel").clicked() {
                                        actions.push(Action::CancelRemove);
                                    }
                                    if ui.button(RichText::new("Remove").color(BAD)).clicked() {
                                        actions.push(Action::Remove(r.peer.id.clone()));
                                    }
                                    ui.label("Remove this device?");
                                } else {
                                    if ui.small_button("Remove").clicked() {
                                        actions.push(Action::AskRemove(r.peer.id.clone()));
                                    }
                                    ui.add_space(8.0);
                                    ui.with_layout(egui::Layout::top_down(egui::Align::Max), |ui| {
                                        ui.label(RichText::new(text).color(dot).strong());
                                        if let Some(d) = detail {
                                            ui.label(RichText::new(d).small().weak());
                                        }
                                    });
                                }
                            });
                        });
                    });
                }

                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    ui.heading(format!("Nearby ({})", nearby.len()));
                    ui.label(RichText::new("Nexus devices on this network that are not paired").weak());
                });
                ui.add_space(4.0);
                if nearby.is_empty() {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(RichText::new("Looking for other devices running Nexus…").weak());
                    });
                }
                for n in &nearby {
                    card(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("●").color(ACCENT).size(16.0));
                            ui.vertical(|ui| {
                                ui.label(RichText::new(&n.name).strong().size(15.0));
                                ui.label(
                                    RichText::new(format!(
                                        "{} · {} · key {}",
                                        n.os,
                                        n.addr.ip(),
                                        identity::fingerprint(&n.public_key)
                                    ))
                                    .small()
                                    .weak(),
                                );
                            });
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.add_enabled(session.is_none(), egui::Button::new("Pair")).clicked() {
                                    actions.push(Action::Pair(PairTarget {
                                        addr: n.addr,
                                        name: n.name.clone(),
                                        expected_id: Some(n.id.clone()),
                                        expected_key: Some(n.public_key.clone()),
                                    }));
                                }
                            });
                        });
                    });
                }

                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Not listed?").weak());
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut self.manual_addr)
                            .hint_text("192.168.1.31")
                            .desired_width(150.0),
                    );
                    let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if (ui.button("Pair by IP address").clicked() || enter)
                        && !self.manual_addr.trim().is_empty()
                        && session.is_none()
                    {
                        actions.push(Action::PairByAddress);
                    }
                });
            });
        });

        if let Some(p) = &session {
            pair_dialog(ctx, p, &mut actions);
        }

        for a in actions {
            self.apply(a);
        }

        // Redraw at most once a second when idle; faster only while pairing.
        let busy = session.is_some() || window.is_some();
        ctx.request_repaint_after(Duration::from_millis(if busy { 250 } else { 1000 }));
    }
}

fn pair_dialog(ctx: &egui::Context, p: &PairSession, actions: &mut Vec<Action>) {
    let title = match p.role {
        Role::Outgoing => format!("Pair with {}", p.peer_name),
        Role::Incoming => format!("{} wants to pair", p.peer_name),
    };
    egui::Window::new(title)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            ui.set_width(360.0);
            match &p.phase {
                Phase::Connecting => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(format!("Connecting to {}…", p.peer_name));
                    });
                    ui.add_space(8.0);
                    if ui.button("Cancel").clicked() {
                        actions.push(Action::CloseSession);
                    }
                }
                Phase::Compare { code } | Phase::WaitingForOther { code } => {
                    ui.label(format!("Check that {} shows the same number:", p.peer_name));
                    ui.add_space(6.0);
                    ui.vertical_centered(|ui| {
                        ui.label(
                            RichText::new(format!("{} {}", &code[..3], &code[3..])).monospace().size(40.0).strong(),
                        );
                    });
                    ui.add_space(8.0);
                    if matches!(p.phase, Phase::Compare { .. }) {
                        ui.horizontal(|ui| {
                            if ui
                                .add(
                                    egui::Button::new(RichText::new("Numbers match").strong().color(Color32::WHITE))
                                        .fill(ACCENT),
                                )
                                .clicked()
                            {
                                actions.push(Action::Answer(true));
                            }
                            if ui.button("They don't match").clicked() {
                                actions.push(Action::Answer(false));
                            }
                        });
                    } else {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(format!("Waiting for {} to confirm…", p.peer_name));
                        });
                        if ui.button("Cancel").clicked() {
                            actions.push(Action::CloseSession);
                        }
                    }
                }
                Phase::Done => {
                    ui.label(RichText::new(format!("Paired with {}", p.peer_name)).color(GOOD).strong().size(16.0));
                    ui.label("It now appears under My devices.");
                    ui.add_space(8.0);
                    if ui.button("Done").clicked() {
                        actions.push(Action::CloseSession);
                    }
                }
                Phase::Failed(e) => {
                    ui.colored_label(BAD, e);
                    ui.add_space(8.0);
                    if ui.button("Close").clicked() {
                        actions.push(Action::CloseSession);
                    }
                }
            }
        });
}

fn card(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::group(ui.style()).inner_margin(egui::Margin::same(12.0)).rounding(egui::Rounding::same(8.0)).show(
        ui,
        |ui| {
            ui.set_width(ui.available_width());
            add(ui);
        },
    );
    ui.add_space(6.0);
}

fn status_text(s: &Status) -> (Color32, String, Option<String>) {
    match s {
        Status::Verified { rtt_ms } => (GOOD, "Online · verified".into(), Some(format!("{rtt_ms} ms round trip"))),
        Status::Checking => (WARN, "On the network · checking".into(), None),
        Status::NotAnswering(r) => (WARN, "On the network · not answering".into(), Some(r.clone())),
        Status::IdentityChanged => (
            BAD,
            "Key changed · not trusted".into(),
            Some("Another device may be using this name. Remove and pair again only if you reinstalled it.".into()),
        ),
        Status::Offline => (GREY, "Offline".into(), None),
    }
}

fn fmt_ago(secs: u64) -> String {
    match secs {
        0..=59 => format!("{secs:>3}s"),
        60..=3599 => format!("{:>3}m", secs / 60),
        _ => format!("{:>3}h", secs / 3600),
    }
}
