// No console window in release builds on Windows.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod discovery;
mod identity;
mod input;
mod net;
mod screen;
mod state;
mod store;
mod sysstats;
mod update;

use eframe::egui;
use std::sync::{Arc, Mutex};

fn main() -> eframe::Result<()> {
    // Started by the updater: give the previous version a moment to close and free its ports.
    if std::env::args().any(|a| a == update::AFTER_UPDATE) {
        std::thread::sleep(std::time::Duration::from_millis(1500));
    }
    update::cleanup();
    let dir = store::data_dir();
    let _ = std::fs::create_dir_all(&dir);
    let me = match identity::Identity::load_or_create(&dir, std::env::var("NEXUS_NAME").ok()) {
        Ok(me) => Arc::new(me),
        Err(e) => {
            eprintln!("Nexus could not start: {e}");
            std::process::exit(1);
        }
    };
    let shared = Arc::new(Mutex::new(state::Shared::new(store::load_peers(&dir))));
    state::lock(&shared).allow_control = store::load_allow_control(&dir);

    let preferred = std::env::var("NEXUS_TCP_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(net::DEFAULT_TCP_PORT);
    let mut problems = Vec::new();
    let tcp_port = net::start_server(me.clone(), shared.clone(), dir.clone(), preferred).unwrap_or_else(|e| {
        problems.push(e);
        0
    });
    if let Err(e) = discovery::start(me.clone(), shared.clone(), dir.clone(), tcp_port) {
        problems.push(e);
    }
    sysstats::start_sampler();
    net::start_status_checker(me.clone(), shared.clone(), dir.clone());
    state::lock(&shared).log(format!("Nexus started as {} (data in {})", me.name, dir.display()));
    let startup_error = (!problems.is_empty()).then(|| problems.join("\n"));

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Nexus")
            .with_inner_size([860.0, 600.0])
            .with_min_inner_size([560.0, 400.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Nexus",
        options,
        Box::new(move |_cc| Ok(Box::new(app::NexusApp::new(me, shared, dir, tcp_port, startup_error)))),
    )
}
