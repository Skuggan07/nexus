//! Long-term device identity: an X25519 key pair kept in the data folder.
//! The device ID is derived from the public key, so an ID cannot be claimed
//! without the matching key.

use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fs, path::Path};
use x25519_dalek::{PublicKey, StaticSecret};

pub struct Identity {
    pub id: String,
    pub name: String,
    pub secret: StaticSecret,
    pub public: PublicKey,
}

#[derive(Serialize, Deserialize)]
struct Stored {
    secret: String,
}

impl Identity {
    /// Loads `identity.json` from `dir`, or creates a new key pair there.
    /// `name` overrides the computer name shown to other devices.
    pub fn load_or_create(dir: &Path, name: Option<String>) -> Result<Self, String> {
        let path = dir.join("identity.json");
        let existing = fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<Stored>(&s).ok())
            .and_then(|st| hex::decode(st.secret).ok())
            .and_then(|b| <[u8; 32]>::try_from(b).ok());

        let secret = match existing {
            Some(bytes) => StaticSecret::from(bytes),
            None => {
                let s = StaticSecret::random_from_rng(OsRng);
                let stored = Stored { secret: hex::encode(s.to_bytes()) };
                fs::create_dir_all(dir).map_err(|e| format!("Cannot create {}: {e}", dir.display()))?;
                fs::write(&path, serde_json::to_string_pretty(&stored).unwrap())
                    .map_err(|e| format!("Cannot write {}: {e}", path.display()))?;
                s
            }
        };
        let public = PublicKey::from(&secret);
        let id = id_for(&hex::encode(public.as_bytes()))?;
        Ok(Self { id, name: name.unwrap_or_else(computer_name), secret, public })
    }

    pub fn public_hex(&self) -> String {
        hex::encode(self.public.as_bytes())
    }
}

/// Device ID: first 8 bytes of SHA-256(public key), as hex.
pub fn id_for(public_key_hex: &str) -> Result<String, String> {
    let bytes = hex::decode(public_key_hex).map_err(|_| "Malformed public key".to_string())?;
    if bytes.len() != 32 {
        return Err("Malformed public key".into());
    }
    Ok(hex::encode(&Sha256::digest(&bytes)[..8]))
}

pub fn decode_public(public_key_hex: &str) -> Option<PublicKey> {
    let bytes: [u8; 32] = hex::decode(public_key_hex).ok()?.try_into().ok()?;
    Some(PublicKey::from(bytes))
}

/// Short, human-comparable fingerprint such as `3F9A 0C21 77DE 51B0`.
pub fn fingerprint(public_key_hex: &str) -> String {
    let digest = hex::decode(public_key_hex).map(|b| Sha256::digest(&b)).unwrap_or_default();
    hex::encode_upper(&digest[..8])
        .as_bytes()
        .chunks(4)
        .map(|c| std::str::from_utf8(c).unwrap_or(""))
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn computer_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "Nexus device".into())
}

pub fn os_label() -> String {
    match std::env::consts::OS {
        "windows" => "Windows",
        "macos" => "macOS",
        "linux" => "Linux",
        other => other,
    }
    .to_string()
}
