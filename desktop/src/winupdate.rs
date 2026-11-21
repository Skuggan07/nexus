//! Windows Update, from any of your devices.
//!
//! * Checking which updates are waiting works for any user, so Nexus does it itself.
//! * Installing updates needs administrator rights. "Set up" (once, on the PC itself,
//!   with Windows' "Do you want to allow…" box) creates a scheduled task that runs as
//!   the system and may be started by the signed-in user. Nexus starts that task;
//!   the task writes how it went to `C:\ProgramData\NexusUpdate\result.json`.
//! * The same setup turns on Windows' own "no automatic restart while someone is
//!   signed in" setting, so Windows doesn't restart in the middle of a game.

use serde::{Deserialize, Serialize};
use std::sync::{Mutex, OnceLock};

const TASK: &str = "Nexus Windows Update";
const FOLDER: &str = r"C:\ProgramData\NexusUpdate";

/// What a PC knows about its Windows updates. Sent to other devices as JSON.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Status {
    /// "idle", "checking", "installing"
    #[serde(default)]
    pub busy: String,
    /// Updates waiting (None = not checked yet).
    #[serde(default)]
    pub waiting: Option<u32>,
    #[serde(default)]
    pub titles: Vec<String>,
    /// Windows wants a restart to finish installing.
    #[serde(default)]
    pub restart: bool,
    /// Seconds since 1970 of the last check.
    #[serde(default)]
    pub checked: u64,
    /// "Set up" has been done, so updates can be installed from other devices.
    #[serde(default)]
    pub ready: bool,
    /// Windows won't restart by itself while someone is signed in.
    #[serde(default)]
    pub no_surprise: bool,
    /// What happened last ("Installed 3 updates", or an error).
    #[serde(default)]
    pub note: String,
}

fn slot() -> &'static Mutex<Status> {
    static S: OnceLock<Mutex<Status>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(Status::default()))
}

fn with<R>(f: impl FnOnce(&mut Status) -> R) -> R {
    let mut g = slot().lock().unwrap_or_else(|e| e.into_inner());
    f(&mut g)
}

#[cfg(windows)]
fn powershell(script: &str) -> Result<String, String> {
    use std::os::windows::process::CommandExt;
    let out = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", script])
        .creation_flags(0x0800_0000)
        .output()
        .map_err(|e| format!("Could not start Windows PowerShell: {e}"))?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

#[cfg(not(windows))]
fn powershell(_script: &str) -> Result<String, String> {
    Err("Windows Update is only on Windows".into())
}

/// Reads the parts that are quick to read: setup done, no-surprise setting, last install.
fn refresh_quick() {
    let script = format!(
        "$t = [bool](Get-ScheduledTask -TaskName '{TASK}' -ErrorAction SilentlyContinue); \
         $n = (Get-ItemProperty 'HKLM:\\SOFTWARE\\Policies\\Microsoft\\Windows\\WindowsUpdate\\AU' -ErrorAction SilentlyContinue).NoAutoRebootWithLoggedOnUsers; \
         $r = ''; if (Test-Path '{FOLDER}\\result.json') {{ $r = Get-Content '{FOLDER}\\result.json' -Raw }}; \
         @{{ ready = $t; ns = ($n -eq 1); result = $r }} | ConvertTo-Json -Compress"
    );
    let Ok(out) = powershell(&script) else { return };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&out) else { return };
    let result: serde_json::Value =
        v["result"].as_str().and_then(|r| serde_json::from_str(r.trim_start_matches('\u{feff}')).ok()).unwrap_or_default();
    with(|s| {
        s.ready = v["ready"].as_bool().unwrap_or(false);
        s.no_surprise = v["ns"].as_bool().unwrap_or(false);
        match result["state"].as_str() {
            Some("installing") => s.busy = "installing".into(),
            Some("done") => {
                if s.busy == "installing" {
                    s.busy = "idle".into();
                }
                s.note = result["note"].as_str().unwrap_or("").to_string();
                if result["restart"].as_bool() == Some(true) {
                    s.restart = true;
                }
            }
            _ => {}
        }
    });
}

/// The current status (refreshing the quick parts first).
pub fn status() -> Status {
    refresh_quick();
    with(|s| s.clone())
}

/// Looks for waiting updates in the background (takes from seconds to a few minutes).
pub fn check() {
    let start = with(|s| {
        if s.busy == "checking" || s.busy == "installing" {
            return false;
        }
        s.busy = "checking".into();
        true
    });
    if !start {
        return;
    }
    std::thread::spawn(|| {
        let script = "$ErrorActionPreference = 'Stop'; \
            try { \
              $s = New-Object -ComObject Microsoft.Update.Session; \
              $r = $s.CreateUpdateSearcher().Search(\"IsInstalled=0 and IsHidden=0 and Type='Software'\"); \
              $t = @(); foreach ($u in $r.Updates) { $t += [string]$u.Title }; \
              $reboot = (New-Object -ComObject Microsoft.Update.SystemInfo).RebootRequired; \
              @{ count = $r.Updates.Count; titles = $t; reboot = [bool]$reboot } | ConvertTo-Json -Compress \
            } catch { @{ error = $_.Exception.Message } | ConvertTo-Json -Compress }";
        let result = powershell(script);
        let now = crate::store::now_unix();
        with(|s| {
            s.busy = "idle".into();
            s.checked = now;
            match result.ok().and_then(|o| serde_json::from_str::<serde_json::Value>(&o).ok()) {
                Some(v) if v.get("error").is_none() => {
                    s.waiting = v["count"].as_u64().map(|n| n as u32);
                    s.titles = match &v["titles"] {
                        serde_json::Value::Array(a) => a.iter().filter_map(|x| x.as_str().map(String::from)).collect(),
                        serde_json::Value::String(t) => vec![t.clone()],
                        _ => Vec::new(),
                    };
                    s.restart = v["reboot"].as_bool().unwrap_or(false);
                    s.note = String::new();
                }
                Some(v) => s.note = format!("Could not check: {}", v["error"].as_str().unwrap_or("unknown error")),
                None => s.note = "Could not check for updates".into(),
            }
        });
    });
}

/// Starts installing the waiting updates (needs "Set up" first).
#[cfg(windows)]
pub fn install() -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    refresh_quick();
    if !with(|s| s.ready) {
        return Err("Press \"Set up\" in Nexus's Update center on that PC first (once)".into());
    }
    let out = std::process::Command::new("schtasks.exe")
        .args(["/run", "/tn", TASK])
        .creation_flags(0x0800_0000)
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err("Windows did not let Nexus start installing. Press \"Set up\" again on that PC.".into());
    }
    with(|s| {
        s.busy = "installing".into();
        s.note = "Installing updates…".into();
    });
    Ok(())
}

#[cfg(not(windows))]
pub fn install() -> Result<(), String> {
    Err("Windows Update is only on Windows".into())
}

/// The script the system task runs: download and install everything waiting.
const INSTALL_SCRIPT: &str = r#"$out = 'C:\ProgramData\NexusUpdate\result.json'
function Save($o) { $o | ConvertTo-Json -Compress | Set-Content -Path $out -Encoding UTF8 }
Save @{ state = 'installing' }
try {
  $s = New-Object -ComObject Microsoft.Update.Session
  $r = $s.CreateUpdateSearcher().Search("IsInstalled=0 and IsHidden=0 and Type='Software'")
  if ($r.Updates.Count -eq 0) { Save @{ state = 'done'; note = 'Nothing to install - up to date'; restart = $false }; exit }
  $list = New-Object -ComObject Microsoft.Update.UpdateColl
  foreach ($u in $r.Updates) { if (-not $u.EulaAccepted) { $u.AcceptEula() }; [void]$list.Add($u) }
  $d = $s.CreateUpdateDownloader(); $d.Updates = $list; [void]$d.Download()
  $i = $s.CreateUpdateInstaller(); $i.Updates = $list; $res = $i.Install()
  $ok = 0; for ($k = 0; $k -lt $list.Count; $k++) { if ($res.GetUpdateResult($k).ResultCode -eq 2) { $ok++ } }
  Save @{ state = 'done'; note = "Installed $ok of $($list.Count) updates"; restart = [bool]$res.RebootRequired }
} catch { Save @{ state = 'done'; note = "Install failed: $($_.Exception.Message)"; restart = $false } }
"#;

/// One-time setup on this PC (Windows asks "Do you want to allow…" first):
/// the install task, its folder, and no surprise restarts. Blocks until done.
#[cfg(windows)]
pub fn set_up() -> Result<(), String> {
    use base64::Engine as _;
    let b64 = |s: &str| base64::engine::general_purpose::STANDARD.encode(s.as_bytes());
    // The task runs as the system and may be started by any signed-in user (GRGX for
    // Authenticated Users); only administrators can change it.
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo><Description>Installs Windows updates when asked from Nexus.</Description><SecurityDescriptor>D:(A;;FA;;;BA)(A;;FA;;;SY)(A;;GRGX;;;AU)</SecurityDescriptor></RegistrationInfo>
  <Principals><Principal id="Author"><UserId>S-1-5-18</UserId><RunLevel>HighestAvailable</RunLevel></Principal></Principals>
  <Settings><MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy><DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries><StopIfGoingOnBatteries>false</StopIfGoingOnBatteries><ExecutionTimeLimit>PT4H</ExecutionTimeLimit><Enabled>true</Enabled></Settings>
  <Actions Context="Author"><Exec><Command>powershell.exe</Command><Arguments>-NoProfile -NonInteractive -ExecutionPolicy Bypass -File "{FOLDER}\install.ps1"</Arguments></Exec></Actions>
</Task>"#
    );
    let script = format!(
        "$ErrorActionPreference = 'Stop'; \
         $d = '{FOLDER}'; New-Item -ItemType Directory -Force -Path $d | Out-Null; \
         icacls $d /inheritance:r /grant:r '*S-1-5-32-544:(OI)(CI)F' '*S-1-5-18:(OI)(CI)F' '*S-1-5-32-545:(OI)(CI)RX' | Out-Null; \
         [IO.File]::WriteAllText(\"$d\\install.ps1\", [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{}'))); \
         $xml = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{}')); \
         Register-ScheduledTask -TaskName '{TASK}' -Xml $xml -Force | Out-Null; \
         $k = 'HKLM:\\SOFTWARE\\Policies\\Microsoft\\Windows\\WindowsUpdate\\AU'; New-Item -Path $k -Force | Out-Null; \
         Set-ItemProperty -Path $k -Name NoAutoRebootWithLoggedOnUsers -Type DWord -Value 1",
        b64(INSTALL_SCRIPT),
        b64(&xml)
    );
    let utf16: Vec<u8> = script.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    let encoded = base64::engine::general_purpose::STANDARD.encode(utf16);
    let outer = format!(
        "$p = Start-Process powershell -Verb RunAs -WindowStyle Hidden -Wait -PassThru \
         -ArgumentList '-NoProfile','-ExecutionPolicy','Bypass','-EncodedCommand','{encoded}'; exit $p.ExitCode"
    );
    use std::os::windows::process::CommandExt;
    let out = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", &outer])
        .creation_flags(0x0800_0000)
        .output()
        .map_err(|e| format!("Could not start Windows PowerShell: {e}"))?;
    refresh_quick();
    if !out.status.success() || !with(|s| s.ready) {
        return Err("Windows didn't allow the change (click Yes in the box that pops up)".into());
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn set_up() -> Result<(), String> {
    Err("Windows Update is only on Windows".into())
}

static SETTING_UP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Runs `set_up` on its own thread; the outcome lands in the status note.
pub fn set_up_in_background() {
    if SETTING_UP.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    std::thread::spawn(|| {
        let note = match set_up() {
            Ok(()) => "Set up — updates can now be installed from any of your devices, and Windows won't restart \
                       by itself while you're signed in."
                .to_string(),
            Err(e) => e,
        };
        with(|s| s.note = note);
        SETTING_UP.store(false, std::sync::atomic::Ordering::Relaxed);
    });
}

pub fn setting_up() -> bool {
    SETTING_UP.load(std::sync::atomic::Ordering::Relaxed)
}
