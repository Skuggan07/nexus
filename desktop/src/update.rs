//! In-app updates. Asks GitHub for the newest Nexus release; if it is newer than
//! this build, downloads the new Windows app next to this one, swaps the files
//! (Windows allows renaming a running program, just not deleting it) and restarts.
//! Settings and paired devices live in the data folder, so they are kept.

use std::{
    fs::{self, File},
    io::{Read, Write},
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

const LATEST: &str = "https://api.github.com/repos/Skuggan07/nexus/releases/latest";
const ASSET: &str = "nexus-windows.exe";
/// Passed to the new version so it waits for this one to close before starting.
pub const AFTER_UPDATE: &str = "--after-update";

/// This app's build number, set by the GitHub build (0 for a local build).
pub fn build() -> u32 {
    option_env!("NEXUS_BUILD").and_then(|s| s.parse().ok()).unwrap_or(0)
}

#[derive(Clone, Default)]
pub enum State {
    #[default]
    Idle,
    Checking,
    UpToDate,
    Available {
        build: u32,
        url: String,
        size: u64,
    },
    Downloading {
        build: u32,
        done: u64,
        total: u64,
    },
    Failed(String),
}

pub type Shared = Arc<Mutex<State>>;

/// The one update state of this app (the Settings screen and requests from other
/// devices share it).
pub fn global() -> &'static Shared {
    static S: std::sync::OnceLock<Shared> = std::sync::OnceLock::new();
    S.get_or_init(Shared::default)
}

/// Asked by another device: if there is a newer Nexus, download it and restart into
/// it. Returns the build it is updating to.
pub fn update_remote() -> Result<u32, String> {
    if matches!(*lock(global()), State::Downloading { .. }) {
        return Err("Already updating".into());
    }
    match latest()? {
        Some((b, url, size)) if b > build() => {
            set(global(), State::Available { build: b, url, size });
            install(global());
            Ok(b)
        }
        _ => Err(format!("Already the newest version (build {})", build())),
    }
}

pub fn lock(s: &Shared) -> MutexGuard<'_, State> {
    s.lock().unwrap_or_else(|e| e.into_inner())
}

fn set(s: &Shared, v: State) {
    *lock(s) = v;
}

/// Looks for a newer release in the background.
pub fn check(state: &Shared) {
    if matches!(*lock(state), State::Checking | State::Downloading { .. }) {
        return;
    }
    set(state, State::Checking);
    let st = state.clone();
    std::thread::spawn(move || {
        let next = match latest() {
            Ok(Some((b, url, size))) if b > build() => State::Available { build: b, url, size },
            Ok(_) => State::UpToDate,
            Err(e) => State::Failed(e),
        };
        set(&st, next);
    });
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(10)).timeout_read(Duration::from_secs(30)).build()
}

/// The newest release's build number and the Windows download, if any.
fn latest() -> Result<Option<(u32, String, u64)>, String> {
    let resp = agent()
        .get(LATEST)
        .set("User-Agent", "Nexus-updater")
        .set("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| format!("Could not reach GitHub: {e}"))?;
    let v: serde_json::Value =
        serde_json::from_reader(resp.into_reader()).map_err(|_| "GitHub sent an unexpected answer".to_string())?;
    let Some(n) = v["tag_name"].as_str().and_then(|t| t.strip_prefix("build-")).and_then(|n| n.parse::<u32>().ok())
    else {
        return Ok(None);
    };
    let Some(asset) = v["assets"].as_array().and_then(|a| a.iter().find(|x| x["name"] == ASSET)) else {
        return Ok(None);
    };
    let url = asset["browser_download_url"].as_str().unwrap_or("").to_string();
    if url.is_empty() {
        return Ok(None);
    }
    Ok(Some((n, url, asset["size"].as_u64().unwrap_or(0))))
}

/// Downloads and installs the available update, then restarts Nexus.
pub fn install(state: &Shared) {
    let (b, url, size) = match &*lock(state) {
        State::Available { build, url, size } => (*build, url.clone(), *size),
        _ => return,
    };
    set(state, State::Downloading { build: b, done: 0, total: size });
    let st = state.clone();
    std::thread::spawn(move || {
        if let Err(e) = download_and_swap(&st, b, &url, size) {
            set(&st, State::Failed(e));
        }
    });
}

fn download_and_swap(st: &Shared, b: u32, url: &str, size: u64) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = exe.parent().ok_or("Cannot find the Nexus folder")?.to_path_buf();
    let part = dir.join("nexus-update.part");
    let resp =
        agent().get(url).set("User-Agent", "Nexus-updater").call().map_err(|e| format!("Download failed: {e}"))?;
    let mut reader = resp.into_reader();
    let mut file =
        File::create(&part).map_err(|e| format!("Cannot save the update next to Nexus ({}): {e}", dir.display()))?;
    let mut buf = vec![0u8; 64 * 1024];
    let mut done = 0u64;
    loop {
        let n = reader.read(&mut buf).map_err(|e| format!("Download failed: {e}"))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).map_err(|e| format!("Cannot save the update: {e}"))?;
        done += n as u64;
        set(st, State::Downloading { build: b, done, total: size });
    }
    drop(file);
    let mut head = [0u8; 2];
    let looks_right = File::open(&part).and_then(|mut f| f.read_exact(&mut head)).is_ok() && &head == b"MZ";
    if done < 500_000 || (size > 0 && done != size) || !looks_right {
        let _ = fs::remove_file(&part);
        return Err("The download was incomplete — try again".into());
    }
    let old = exe.with_extension("old.exe");
    let _ = fs::remove_file(&old);
    fs::rename(&exe, &old).map_err(|e| format!("Could not replace Nexus: {e}"))?;
    if let Err(e) = fs::rename(&part, &exe) {
        let _ = fs::rename(&old, &exe);
        return Err(format!("Could not replace Nexus: {e}"));
    }
    std::process::Command::new(&exe)
        .arg(AFTER_UPDATE)
        .spawn()
        .map_err(|e| format!("Updated, but could not restart ({e}). Start Nexus again."))?;
    std::process::exit(0);
}

/// Removes the previous version left behind by an update (it may still be closing).
pub fn cleanup() {
    let Ok(exe) = std::env::current_exe() else { return };
    let old = exe.with_extension("old.exe");
    if !old.exists() {
        return;
    }
    std::thread::spawn(move || {
        for _ in 0..30 {
            if fs::remove_file(&old).is_ok() || !old.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    });
}
