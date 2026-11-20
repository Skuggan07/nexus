//! Nicknames, icons and groups, shared between all paired devices.
//!
//! Every device keeps labels for itself and for each device it is paired with, each
//! stamped with when it was last changed. Whenever labels change, and whenever a
//! paired device comes online, they are sent over (`{op:"labels"}`) and the newest
//! version of each device's labels wins. So a group made on one device shows up on
//! all of them, and each device can be put in a group too.

use crate::{
    identity::Identity,
    net,
    state::{lock, SharedState},
    store::{self, PairedPeer},
};
use serde::{Deserialize, Serialize};
use std::{path::Path, sync::Arc};

/// This PC's own nickname, icon and group, and when they last changed.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Labels {
    #[serde(default)]
    pub nick: String,
    #[serde(default)]
    pub icon: String,
    #[serde(default)]
    pub group: String,
    /// Seconds since 1970 when they last changed (0 = never).
    #[serde(default)]
    pub t: u64,
}

pub fn load_mine(dir: &Path) -> Labels {
    std::fs::read_to_string(dir.join("labels.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

pub fn save_mine(dir: &Path, l: &Labels) -> Result<(), String> {
    let text = serde_json::to_string_pretty(l).map_err(|e| e.to_string())?;
    let tmp = dir.join("labels.json.tmp");
    std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, dir.join("labels.json")).map_err(|e| e.to_string())
}

/// Labels made before they were shared have no time yet; give them the oldest
/// possible one so they still reach devices that have none.
pub fn upgrade(peers: &mut [PairedPeer]) {
    for p in peers {
        if p.labels_t == 0 && !(p.nick.is_empty() && p.icon.is_empty() && p.group.is_empty()) {
            p.labels_t = 1;
        }
    }
}

fn entry(id: &str, nick: &str, icon: &str, group: &str, t: u64) -> serde_json::Value {
    serde_json::json!({ "id": id, "nick": nick, "icon": icon, "group": group, "t": t })
}

/// Everything this PC knows: its own labels and those of every paired device.
pub fn all(me: &Identity, shared: &SharedState) -> Vec<serde_json::Value> {
    let s = lock(shared);
    let mut out = vec![entry(&me.id, &s.my_labels.nick, &s.my_labels.icon, &s.my_labels.group, s.my_labels.t)];
    for p in &s.paired {
        if p.labels_t > 0 {
            out.push(entry(&p.id, &p.nick, &p.icon, &p.group, p.labels_t));
        }
    }
    out
}

fn text(v: &serde_json::Value, k: &str, max: usize) -> String {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("").trim().chars().take(max).collect()
}

/// Takes in labels from another device; newer ones replace ours. True if anything changed.
pub fn merge(me: &Identity, shared: &SharedState, dir: &Path, entries: &[serde_json::Value]) -> bool {
    let mut s = lock(shared);
    let (mut mine, mut peers) = (false, false);
    for e in entries {
        let Some(id) = e.get("id").and_then(|x| x.as_str()) else { continue };
        let t = e.get("t").and_then(|x| x.as_u64()).unwrap_or(0);
        let (nick, icon, group) = (text(e, "nick", 40), text(e, "icon", 8), text(e, "group", 30));
        if id == me.id {
            if t > s.my_labels.t {
                s.my_labels = Labels { nick, icon, group, t };
                mine = true;
            }
        } else if let Some(p) = s.paired.iter_mut().find(|p| p.id == id) {
            if t > p.labels_t {
                p.nick = nick;
                p.icon = icon;
                p.group = group;
                p.labels_t = t;
                peers = true;
            }
        }
    }
    if mine {
        let _ = save_mine(dir, &s.my_labels);
    }
    if peers {
        let _ = store::save_peers(dir, &s.paired);
    }
    mine || peers
}

/// Sends our labels to one device.
pub fn share_with(me: &Identity, shared: &SharedState, peer: &PairedPeer, addr: std::net::SocketAddr) {
    let entries = all(me, shared);
    let _ = net::labels_send(me, peer, addr, &entries);
}

/// Sends our labels to every paired device that is online (in the background).
pub fn share_all(me: Arc<Identity>, shared: SharedState) {
    std::thread::spawn(move || {
        for (peer, addr) in crate::clipboard::online_targets(&shared) {
            share_with(&me, &shared, &peer, addr);
        }
    });
}
