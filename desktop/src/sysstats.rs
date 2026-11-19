//! This device's CPU, RAM and battery, sampled every few seconds in the background.
//! Paired devices receive the latest sample inside the signed status check.

use serde::{Deserialize, Serialize};
use std::{
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
}

static LATEST: OnceLock<Mutex<Option<String>>> = OnceLock::new();

fn slot() -> &'static Mutex<Option<String>> {
    LATEST.get_or_init(|| Mutex::new(None))
}

/// The latest sample as compact JSON, exactly as it is sent and signed.
pub fn latest_json() -> Option<String> {
    slot().lock().ok().and_then(|g| g.clone())
}

pub fn start_sampler() {
    let _ = thread::Builder::new().name("nexus-stats".into()).spawn(|| {
        let mut sys = sysinfo::System::new();
        sys.refresh_cpu();
        loop {
            thread::sleep(Duration::from_secs(3));
            sys.refresh_cpu();
            sys.refresh_memory();
            let total = sys.total_memory();
            let s = Stats {
                cpu: Some(round1(sys.global_cpu_info().cpu_usage())),
                ram: (total > 0).then(|| round1(sys.used_memory() as f32 / total as f32 * 100.0)),
                battery: battery_percent(),
                wake: crate::wakeup::code(),
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
