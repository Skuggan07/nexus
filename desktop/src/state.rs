//! State shared between the network threads and the window.

use crate::{store::PairedPeer, sysstats::Stats};
use std::{
    collections::{HashMap, VecDeque},
    net::IpAddr,
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant},
};

/// A device counts as present on the network if it announced itself this recently.
pub const ONLINE_WINDOW: Duration = Duration::from_secs(7);
/// How long a status check result stays valid.
pub const CHECK_VALID: Duration = Duration::from_secs(15);
/// A phone saving battery in the background is checked about once a minute, so its
/// last answer counts for longer.
pub const IDLE_CHECK_EVERY: Duration = Duration::from_secs(60);
pub const IDLE_CHECK_VALID: Duration = Duration::from_secs(150);
/// How long "Allow pairing" stays open, and how many requests it accepts.
pub const PAIR_WINDOW: Duration = Duration::from_secs(120);
pub const PAIR_ATTEMPTS: u8 = 3;

pub type SharedState = Arc<Mutex<Shared>>;

/// Locks the shared state, recovering from a poisoned lock instead of crashing.
pub fn lock(s: &SharedState) -> MutexGuard<'_, Shared> {
    s.lock().unwrap_or_else(|p| p.into_inner())
}

#[derive(Clone, Debug)]
pub struct Seen {
    pub id: String,
    pub name: String,
    pub os: String,
    pub public_key: String,
    pub addr: IpAddr,
    pub tcp_port: u16,
    pub last_seen: Instant,
}

impl Seen {
    pub fn fresh(&self) -> bool {
        self.last_seen.elapsed() < ONLINE_WINDOW
    }
}

#[derive(Clone, Debug)]
pub enum Check {
    Ok { rtt_ms: u128, at: Instant, stats: Option<Stats> },
    Failed { reason: String, at: Instant },
}

pub struct PairWindow {
    pub expires: Instant,
    pub attempts_left: u8,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Role {
    /// We clicked Pair on another device.
    Outgoing,
    /// Another device asked to pair while "Allow pairing" was on.
    Incoming,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Phase {
    Connecting,
    /// Both screens show `code`; waiting for this user's answer.
    Compare {
        code: String,
    },
    /// This user confirmed; waiting for the other device.
    WaitingForOther {
        code: String,
    },
    Done,
    Failed(String),
}

/// One pairing in progress. The window shows it; the network thread drives it.
#[derive(Clone, Debug)]
pub struct PairSession {
    pub id: u64,
    pub role: Role,
    pub peer_name: String,
    pub phase: Phase,
    /// Set by the window: Some(true) = numbers match, Some(false) = cancel.
    pub local: Option<bool>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    /// Answered a signed challenge just now.
    Verified {
        rtt_ms: u128,
        stats: Option<Stats>,
    },
    /// Announcing itself on the network; first check still running.
    Checking,
    /// Announcing itself but the challenge failed.
    NotAnswering(String),
    /// Announces a different key than the one we paired with.
    IdentityChanged,
    Offline,
}

pub struct Shared {
    pub seen: HashMap<String, Seen>,
    pub paired: Vec<PairedPeer>,
    pub checks: HashMap<String, Check>,
    pub window: Option<PairWindow>,
    pub pair: Option<PairSession>,
    pub log: VecDeque<(Instant, String)>,
    /// Whether paired devices may control this PC's mouse and keyboard.
    pub allow_control: bool,
    /// This PC's automation rules (see automation.rs).
    pub automations: Vec<crate::automation::Rule>,
    /// Whether copied text is shared with paired devices (Settings).
    pub share_clipboard: bool,
    /// The last clipboard text sent or received, so it isn't sent back and forth.
    pub clip_last: String,
    /// What each PC said about being woken (ready, wifi, off, unknown), by device id.
    pub wake_hints: HashMap<String, String>,
    /// Devices we just sent a wake signal to, and when.
    pub waking: HashMap<String, Instant>,
    /// Devices that did not wake up after the last try.
    pub wake_failed: std::collections::HashSet<String>,
    next_session: u64,
}

impl Shared {
    pub fn new(paired: Vec<PairedPeer>) -> Self {
        Self {
            seen: HashMap::new(),
            paired,
            checks: HashMap::new(),
            window: None,
            pair: None,
            log: VecDeque::new(),
            allow_control: true,
            automations: Vec::new(),
            share_clipboard: true,
            clip_last: String::new(),
            wake_hints: HashMap::new(),
            waking: HashMap::new(),
            wake_failed: std::collections::HashSet::new(),
            next_session: 1,
        }
    }

    pub fn log(&mut self, msg: impl Into<String>) {
        self.log.push_front((Instant::now(), msg.into()));
        self.log.truncate(200);
    }

    pub fn is_paired(&self, id: &str) -> bool {
        self.paired.iter().any(|p| p.id == id)
    }

    pub fn status(&self, p: &PairedPeer) -> Status {
        let seen = self.seen.get(&p.id).filter(|s| s.fresh());
        if seen.is_some_and(|s| s.public_key != p.public_key) {
            return Status::IdentityChanged;
        }
        match self.checks.get(&p.id) {
            Some(Check::Ok { rtt_ms, at, stats })
                if at.elapsed()
                    < if stats.as_ref().is_some_and(|s| s.idle) { IDLE_CHECK_VALID } else { CHECK_VALID } =>
            {
                Status::Verified { rtt_ms: *rtt_ms, stats: stats.clone() }
            }
            Some(Check::Failed { reason, at }) if at.elapsed() < CHECK_VALID && seen.is_some() => {
                Status::NotAnswering(reason.clone())
            }
            _ if seen.is_some() => Status::Checking,
            _ => Status::Offline,
        }
    }

    /// Open pairing window, if it has not expired or run out of attempts.
    pub fn open_window(&self) -> Option<&PairWindow> {
        self.window.as_ref().filter(|w| w.expires > Instant::now() && w.attempts_left > 0)
    }

    /// Starts a pairing session, unless one is already running.
    pub fn begin_pair(&mut self, role: Role, peer_name: &str) -> Option<u64> {
        if self.pair.as_ref().is_some_and(|p| !matches!(p.phase, Phase::Done | Phase::Failed(_))) {
            return None;
        }
        let id = self.next_session;
        self.next_session += 1;
        self.pair = Some(PairSession { id, role, peer_name: peer_name.into(), phase: Phase::Connecting, local: None });
        Some(id)
    }

    /// The session with this id, if the user has not closed it.
    pub fn session(&mut self, id: u64) -> Option<&mut PairSession> {
        self.pair.as_mut().filter(|p| p.id == id)
    }
}
