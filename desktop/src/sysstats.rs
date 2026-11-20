//! This device's CPU, RAM and battery, sampled every few seconds in the background.
//! Paired devices receive the latest sample inside the signed status check.
//!
//! It also keeps track of how long the PC is used: a moment counts as use when the
//! mouse or keyboard was touched in the last 5 minutes. Kept in `usage.json`.

use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    thread,
    time::Duration,
};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Stats {
    /// Percent, 0–100. `None` when the system does not tell (for example on Android).
    pub cpu: Option<f32>,
    pub ram: Option<f32>,
    pub battery: Option<f32>,
    /// Whether this PC can be woken from another device: ready, wifi, off or unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wake: Option<String>,
    /// This PC's network card addresses, so paired devices can wake it. Also in the
    /// network announcements, but some phones never receive those.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub macs: Vec<String>,
    /// A phone that is saving battery in the background: check on it less often.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub idle: bool,
    /// Who is signed in on this PC.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    /// Seconds since this device last started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uptime: Option<u64>,
    /// Seconds this PC has been used today, and in total since Nexus started counting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub today: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
}

/// How long this PC has been used.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Usage {
    /// The local date "today" refers to.
    #[serde(default)]
    day: String,
    #[serde(default)]
    today: u64,
    #[serde(default)]
    total: u64,
}

fn load_usage(dir: &Path) -> Usage {
    std::fs::read_to_string(dir.join("usage.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn save_usage(dir: &Path, u: &Usage) {
    if let Ok(t) = serde_json::to_string(u) {
        let tmp = dir.join("usage.json.tmp");
        if std::fs::write(&tmp, t).is_ok() {
            let _ = std::fs::rename(&tmp, dir.join("usage.json"));
        }
    }
}

/// Today's local date, like "2026-11-20".
fn local_day() -> String {
    let key = crate::automation::local_now().3;
    key.split(' ').next().unwrap_or_default().to_string()
}

static LATEST: OnceLock<Mutex<Option<String>>> = OnceLock::new();

fn slot() -> &'static Mutex<Option<String>> {
    LATEST.get_or_init(|| Mutex::new(None))
}

/// The latest sample as compact JSON, exactly as it is sent and signed.
pub fn latest_json() -> Option<String> {
    slot().lock().ok().and_then(|g| g.clone())
}

pub fn start_sampler(dir: PathBuf) {
    let _ = thread::Builder::new().name("nexus-stats".into()).spawn(move || {
        let mut sys = sysinfo::System::new();
        sys.refresh_cpu();
        let macs = crate::net::local_macs();
        let user = std::env::var("USERNAME").ok().filter(|u| !u.is_empty());
        let mut usage = load_usage(&dir);
        let mut ticks = 0u32;
        loop {
            thread::sleep(Duration::from_secs(3));
            let day = local_day();
            if usage.day != day {
                usage.day = day;
                usage.today = 0;
            }
            if crate::automation::idle_time() < Duration::from_secs(300) {
                usage.today += 3;
                usage.total += 3;
            }
            ticks += 1;
            if ticks % 20 == 0 {
                save_usage(&dir, &usage);
            }
            sys.refresh_cpu();
            sys.refresh_memory();
            let total = sys.total_memory();
            let s = Stats {
                cpu: Some(round1(sys.global_cpu_info().cpu_usage())),
                ram: (total > 0).then(|| round1(sys.used_memory() as f32 / total as f32 * 100.0)),
                battery: battery_percent(),
                wake: crate::wakeup::code(),
                macs: macs.clone(),
                idle: false,
                user: user.clone(),
                uptime: Some(sysinfo::System::uptime()),
                today: Some(usage.today),
                total: Some(usage.total),
            };
            if let (Ok(json), Ok(mut g)) = (serde_json::to_string(&s), slot().lock()) {
                *g = Some(json);
            }
        }
    });
}

fn round1(v: f32) -> f32 {
    (v.clamp(0.0, 100.0) * 10.0).round() / 10.0
}

#[cfg(windows)]
pub fn battery_percent() -> Option<f32> {
    use windows_sys::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
    let mut st: SYSTEM_POWER_STATUS = unsafe { std::mem::zeroed() };
    if unsafe { GetSystemPowerStatus(&mut st) } == 0 {
        return None;
    }
    // 128 = no system battery (a desktop PC); 255 = unknown.
    if st.BatteryFlag == 128 || st.BatteryLifePercent == 255 {
        None
    } else {
        Some(st.BatteryLifePercent as f32)
    }
}

#[cfg(not(windows))]
pub fn battery_percent() -> Option<f32> {
    None
}
