//! App list and monitoring for a paired viewer: CPU, memory, disks, uptime and
//! battery, plus the running apps (grouped by program name). Ending an app is
//! allowed only while "Allow remote control" is on.

use serde_json::{json, Value};
use std::collections::HashMap;
use sysinfo::{Disks, System};

/// Programs that keep Windows running; ending them would crash or log out the PC.
const PROTECTED: &[&str] = &[
    "system",
    "system idle process",
    "registry",
    "smss.exe",
    "csrss.exe",
    "wininit.exe",
    "winlogon.exe",
    "services.exe",
    "lsass.exe",
    "svchost.exe",
    "dwm.exe",
    "fontdrvhost.exe",
    "memory compression",
];

pub struct Monitor {
    sys: System,
}

fn round1(v: f32) -> f32 {
    (v * 10.0).round() / 10.0
}

pub fn is_protected(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    PROTECTED.contains(&n.as_str()) || n.starts_with("nexus")
}

impl Monitor {
    pub fn new() -> Self {
        let mut sys = System::new();
        sys.refresh_cpu();
        sys.refresh_processes();
        Monitor { sys }
    }

    /// Current figures and the running apps, biggest memory users first.
    pub fn snapshot(&mut self, can_end: bool) -> Value {
        self.sys.refresh_cpu();
        self.sys.refresh_memory();
        self.sys.refresh_processes();
        let cores = self.sys.cpus().len().max(1) as f32;
        let mut groups: HashMap<String, (f32, u64, u32)> = HashMap::new();
        for p in self.sys.processes().values() {
            let name = p.name().to_string();
            if name.is_empty() {
                continue;
            }
            let g = groups.entry(name).or_default();
            g.0 += p.cpu_usage();
            g.1 += p.memory();
            g.2 += 1;
        }
        let mut apps: Vec<(String, f32, u64, u32)> =
            groups.into_iter().map(|(n, (cpu, mem, count))| (n, cpu / cores, mem, count)).collect();
        apps.sort_by(|a, b| b.2.cmp(&a.2));
        apps.truncate(200);
        let apps: Vec<Value> = apps
            .into_iter()
            .map(|(name, cpu, mem, count)| {
                json!({
                    "name": name,
                    "cpu": round1(cpu.clamp(0.0, 100.0)),
                    "mem": mem,
                    "count": count,
                    "protected": is_protected(&name),
                })
            })
            .collect();
        let disks: Vec<Value> = Disks::new_with_refreshed_list()
            .list()
            .iter()
            .filter(|d| d.total_space() > 0)
            .map(|d| {
                json!({
                    "name": d.mount_point().display().to_string(),
                    "total": d.total_space(),
                    "free": d.available_space(),
                })
            })
            .collect();
        json!({
            "op": "snapshot",
            "cpu": round1(self.sys.global_cpu_info().cpu_usage()),
            "ram_used": self.sys.used_memory(),
            "ram_total": self.sys.total_memory(),
            "uptime": System::uptime(),
            "battery": crate::sysstats::battery_percent(),
            "cores": cores as u32,
            "disks": disks,
            "apps": apps,
            "can_end": can_end,
        })
    }

    /// Ends every process of a program. Returns how many were ended.
    pub fn end_app(&mut self, name: &str) -> Result<usize, String> {
        if is_protected(name) {
            return Err(format!("{name} keeps Windows running and cannot be ended from Nexus"));
        }
        self.sys.refresh_processes();
        let me = sysinfo::get_current_pid().ok();
        let mut ended = 0;
        for p in self.sys.processes().values() {
            if p.name() == name && Some(p.pid()) != me && p.kill() {
                ended += 1;
            }
        }
        if ended == 0 {
            Err(format!("Could not end {name} (it may need administrator rights or has already closed)"))
        } else {
            Ok(ended)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::is_protected;

    #[test]
    fn protects_core_windows_programs() {
        assert!(is_protected("csrss.exe"));
        assert!(is_protected("SvcHost.exe"));
        assert!(is_protected("nexus-windows.exe"));
        assert!(!is_protected("chrome.exe"));
    }
}
