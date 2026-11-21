//! Disk health warning: every 10 minutes this PC looks at its drives and notes
//! any that are almost full, or that Windows says are failing. The warnings
//! travel to paired devices inside the signed status check, so every device
//! can show them.

use std::{
    sync::{Mutex, OnceLock},
    thread,
    time::Duration,
};

const GB: u64 = 1024 * 1024 * 1024;
/// How often the drives are looked at.
const EVERY: Duration = Duration::from_secs(600);

fn slot() -> &'static Mutex<Vec<String>> {
    static S: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(Vec::new()))
}

/// What a warning is about, without the changing numbers ("Drive C: is almost full").
pub fn key(w: &str) -> &str {
    w.split(" — ").next().unwrap_or(w)
}

/// This PC's current disk warnings (empty when everything is fine).
pub fn warnings() -> Vec<String> {
    slot().lock().map(|g| g.clone()).unwrap_or_default()
}

/// "3.1 GB", "512 MB".
fn size(b: u64) -> String {
    if b >= 100 * GB {
        format!("{} GB", b / GB)
    } else if b >= GB {
        format!("{:.1} GB", b as f64 / GB as f64)
    } else {
        format!("{} MB", b / (1024 * 1024))
    }
}

/// A drive is almost full when less than 10 % or less than 5 GB is free.
pub fn almost_full(free: u64, total: u64) -> bool {
    total >= GB && (free < 5 * GB || free.saturating_mul(10) < total)
}

/// One line from Windows about a physical disk: "name|health".
/// Health can come as a word or as a number (0 healthy, 1 warning, 2 unhealthy).
pub fn health_warning(line: &str) -> Option<String> {
    let (name, health) = line.rsplit_once('|')?;
    let name = if name.trim().is_empty() { "A disk" } else { name.trim() };
    match health.trim().to_ascii_lowercase().as_str() {
        "1" | "warning" => Some(format!("{name} may be starting to fail — back up your files")),
        "2" | "unhealthy" => Some(format!("{name} is failing — back up your files now")),
        _ => None,
    }
}

fn check() -> Vec<String> {
    let mut out = Vec::new();
    for d in sysinfo::Disks::new_with_refreshed_list().list() {
        if d.is_removable() {
            continue;
        }
        let (free, total) = (d.available_space(), d.total_space());
        if almost_full(free, total) {
            let name = d.mount_point().display().to_string();
            let name = name.trim_end_matches('\\');
            out.push(format!("Drive {name} is almost full — {} free of {}", size(free), size(total)));
        }
    }
    out.extend(physical_health().lines().filter_map(health_warning));
    out
}

#[cfg(windows)]
fn physical_health() -> String {
    use std::os::windows::process::CommandExt;
    std::process::Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Get-PhysicalDisk -ErrorAction SilentlyContinue | ForEach-Object { \"$($_.FriendlyName)|$($_.HealthStatus)\" }",
        ])
        .creation_flags(0x0800_0000)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

#[cfg(not(windows))]
fn physical_health() -> String {
    String::new()
}

/// Starts looking at the drives in the background; new warnings go into the log.
pub fn start(shared: crate::state::SharedState) {
    let _ = thread::Builder::new().name("nexus-disks".into()).spawn(move || {
        thread::sleep(Duration::from_secs(20));
        loop {
            let now = check();
            let before = warnings();
            for w in now.iter().filter(|w| !before.iter().any(|b| key(b) == key(w))) {
                crate::state::lock(&shared).log(format!("Disk warning: {w}"));
            }
            if let Ok(mut g) = slot().lock() {
                *g = now;
            }
            thread::sleep(EVERY);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_and_failing() {
        assert!(almost_full(4 * GB, 500 * GB));
        assert!(almost_full(40 * GB, 500 * GB));
        assert!(!almost_full(60 * GB, 500 * GB));
        assert!(!almost_full(100, 200)); // tiny partitions are ignored
        assert!(health_warning("Samsung SSD|Healthy").is_none());
        assert!(health_warning("Samsung SSD|0").is_none());
        assert!(health_warning("Samsung SSD|Warning").unwrap().contains("Samsung SSD"));
        assert!(health_warning("WD|2").unwrap().contains("failing"));
    }
}
