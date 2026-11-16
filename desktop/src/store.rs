//! Where Nexus keeps its files, and the list of paired devices.

use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PairedPeer {
    pub id: String,
    pub name: String,
    pub os: String,
    pub public_key: String,
    pub paired_at: u64,
    /// Last address the device answered on, as `ip:port`.
    #[serde(default)]
    pub last_addr: Option<String>,
}

/// `%APPDATA%\Nexus\Nexus\data` on Windows. Override with `NEXUS_DATA_DIR`
/// (handy for running two copies on one PC while testing).
pub fn data_dir() -> PathBuf {
    if let Ok(d) = std::env::var("NEXUS_DATA_DIR") {
        return PathBuf::from(d);
    }
    directories::ProjectDirs::from("app", "Nexus", "Nexus")
        .map(|p| p.data_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from(".nexus"))
}

pub fn load_peers(dir: &Path) -> Vec<PairedPeer> {
    fs::read_to_string(dir.join("peers.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

/// Writes to a temporary file first so a crash never leaves a half-written list.
pub fn save_peers(dir: &Path, peers: &[PairedPeer]) -> Result<(), String> {
    let tmp = dir.join("peers.json.tmp");
    let json = serde_json::to_string_pretty(peers).map_err(|e| e.to_string())?;
    fs::write(&tmp, json).map_err(|e| e.to_string())?;
    fs::rename(&tmp, dir.join("peers.json")).map_err(|e| e.to_string())
}

pub fn now_unix() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Remembers whether the window was last in Simple or Detailed mode.
pub fn load_detailed(dir: &Path) -> bool {
    fs::read_to_string(dir.join("prefs.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.get("detailed").and_then(|d| d.as_bool()))
        .unwrap_or(false)
}

pub fn save_detailed(dir: &Path, detailed: bool) {
    let _ = fs::write(dir.join("prefs.json"), serde_json::json!({ "detailed": detailed }).to_string());
}
