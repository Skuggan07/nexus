//! "Can this PC be woken from another device?" — checks the network adapter Windows
//! uses, whether it is wired or Wi-Fi, and whether it is allowed to wake the PC with
//! a Wake-on-LAN "magic packet". `set_up` turns those settings on (Windows asks for
//! administrator permission). The result is also shared with paired devices (in the
//! signed status check) so they can tell you before you try to wake this PC.

use std::{
    sync::{Mutex, OnceLock},
    thread,
    time::Duration,
};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct WakeInfo {
    /// The adapter's name, e.g. "Ethernet" or "Wi-Fi".
    pub adapter: String,
    pub wifi: bool,
    /// "Wake on magic packet" in the adapter's power settings (None: unknown or unsupported).
    pub magic: Option<bool>,
    /// Whether Windows lets the adapter wake the PC ("Allow this device to wake the computer").
    pub armed: Option<bool>,
}

impl WakeInfo {
    /// Short code shared with paired devices: ready, wifi, off or unknown.
    pub fn code(&self) -> &'static str {
        if self.wifi {
            "wifi"
        } else if self.magic == Some(true) && self.armed == Some(true) {
            "ready"
        } else if self.magic == Some(false) || self.armed == Some(false) {
            "off"
        } else {
            "unknown"
        }
    }
}

fn slot() -> &'static Mutex<Option<WakeInfo>> {
    static S: OnceLock<Mutex<Option<WakeInfo>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(None))
}

/// The latest check, if one has finished.
pub fn current() -> Option<WakeInfo> {
    slot().lock().ok().and_then(|g| g.clone())
}

pub fn code() -> Option<String> {
    current().map(|i| i.code().to_string())
}

/// Checks now and every 10 minutes in the background.
pub fn start() {
    thread::spawn(|| loop {
        refresh();
        thread::sleep(Duration::from_secs(600));
    });
}

/// Runs the check (takes a second or two) and stores the result.
pub fn refresh() {
    if let Some(info) = check() {
        if let Ok(mut g) = slot().lock() {
            *g = Some(info);
        }
    }
}

/// Finds the adapter used for the internet (the default route) as `$a`.
const FIND_ADAPTER: &str = "$ErrorActionPreference='SilentlyContinue'\n\
$r = Get-NetRoute -DestinationPrefix '0.0.0.0/0' | Sort-Object RouteMetric | Select-Object -First 1\n\
$a = Get-NetAdapter -InterfaceIndex $r.ifIndex\n";

const CHECK: &str = "$pm = Get-NetAdapterPowerManagement -Name $a.Name\n\
$armed = @(powercfg /devicequery wake_armed) | ForEach-Object { $_.Trim() }\n\
[pscustomobject]@{ name = \"$($a.Name)\"; media = \"$($a.PhysicalMediaType)\"; \
magic = \"$($pm.WakeOnMagicPacket)\"; armed = ($armed -contains $a.InterfaceDescription) } | ConvertTo-Json -Compress\n";

const FIX: &str = "Set-NetAdapterPowerManagement -Name $a.Name -WakeOnMagicPacket Enabled -NoRestart\n\
Get-NetAdapterAdvancedProperty -Name $a.Name | Where-Object { $_.DisplayName -match 'Magic Packet|Wake on LAN|Wake-on-LAN|WOL' } | \
ForEach-Object { Set-NetAdapterAdvancedProperty -Name $a.Name -RegistryKeyword $_.RegistryKeyword -RegistryValue 1 -NoRestart }\n\
powercfg /deviceenablewake \"$($a.InterfaceDescription)\"\n";

/// Reads the check's JSON answer.
fn parse(json: &str) -> Option<WakeInfo> {
    let v: serde_json::Value = serde_json::from_str(json.trim()).ok()?;
    let name = v.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string();
    if name.is_empty() {
        return None;
    }
    let media = v.get("media").and_then(|x| x.as_str()).unwrap_or("").to_lowercase();
    let lower = name.to_lowercase();
    let wifi = media.contains("802.11") || lower.contains("wi-fi") || lower.contains("wlan") || lower.contains("wifi");
    let magic = match v.get("magic").and_then(|x| x.as_str()).unwrap_or("") {
        "Enabled" => Some(true),
        "Disabled" => Some(false),
        _ => None,
    };
    let armed = v.get("armed").and_then(|x| x.as_bool());
    Some(WakeInfo { adapter: name, wifi, magic, armed })
}

#[cfg(windows)]
fn powershell(args: &[&str]) -> std::io::Result<std::process::Output> {
    use std::os::windows::process::CommandExt;
    std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass"])
        .args(args)
        .creation_flags(0x0800_0000) // no console window
        .output()
}

#[cfg(windows)]
fn check() -> Option<WakeInfo> {
    let script = format!("{FIND_ADAPTER}{CHECK}");
    let out = powershell(&["-Command", &script]).ok()?;
    parse(&String::from_utf8_lossy(&out.stdout))
}

#[cfg(not(windows))]
fn check() -> Option<WakeInfo> {
    None
}

static BUSY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn note_slot() -> &'static Mutex<Option<String>> {
    static S: OnceLock<Mutex<Option<String>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(None))
}

/// Whether "Set up waking" or "Check again" is running.
pub fn busy() -> bool {
    BUSY.load(std::sync::atomic::Ordering::Relaxed)
}

/// The outcome of the last "Set up waking", for Settings.
pub fn note() -> Option<String> {
    note_slot().lock().ok().and_then(|g| g.clone())
}

/// Runs `set_up` (or just a new check) on its own thread.
pub fn run_in_background(fix: bool) {
    if BUSY.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    thread::spawn(move || {
        let msg = if fix {
            match set_up() {
                Ok(()) => match current().map(|i| i.code()) {
                    Some("ready") => "Done — this PC can now be woken from your other devices.".to_string(),
                    Some("wifi") => "Done, but this PC uses Wi-Fi, so waking may still not work.".to_string(),
                    _ => "Done. If waking still fails, turn on Wake-on-LAN in this PC's BIOS.".to_string(),
                },
                Err(e) => e,
            }
        } else {
            refresh();
            String::new()
        };
        if let Ok(mut g) = note_slot().lock() {
            *g = (!msg.is_empty()).then_some(msg);
        }
        BUSY.store(false, std::sync::atomic::Ordering::Relaxed);
    });
}

/// Turns on waking for this PC's network adapter. Windows shows its "Do you want to
/// allow this app to make changes?" box first. Blocks until it is done.
#[cfg(windows)]
pub fn set_up() -> Result<(), String> {
    use base64::Engine as _;
    let script = format!("{FIND_ADAPTER}{FIX}");
    let utf16: Vec<u8> = script.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    let encoded = base64::engine::general_purpose::STANDARD.encode(utf16);
    let outer = format!(
        "$p = Start-Process powershell -Verb RunAs -WindowStyle Hidden -Wait -PassThru \
         -ArgumentList '-NoProfile','-ExecutionPolicy','Bypass','-EncodedCommand','{encoded}'; exit $p.ExitCode"
    );
    let out = powershell(&["-Command", &outer]).map_err(|e| format!("Could not start Windows PowerShell: {e}"))?;
    refresh();
    if !out.status.success() || !out.stderr.is_empty() {
        return Err("Windows didn't allow the change (you need to click Yes in the box that pops up)".into());
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn set_up() -> Result<(), String> {
    Err("Setting up waking only works on Windows".into())
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn reads_the_check() {
        let i = parse(r#"{"name":"Ethernet","media":"802.3","magic":"Enabled","armed":true}"#).unwrap();
        assert_eq!(i.code(), "ready");
        let w = parse(r#"{"name":"Wi-Fi","media":"Native 802.11","magic":"Enabled","armed":true}"#).unwrap();
        assert_eq!(w.code(), "wifi");
        let o = parse(r#"{"name":"Ethernet","media":"802.3","magic":"Disabled","armed":false}"#).unwrap();
        assert_eq!(o.code(), "off");
        assert!(parse("").is_none());
    }
}
