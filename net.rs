//! TCP control channel: pairing and status checks. See PROTOCOL.md for the wire format.
//!
//! Pairing is "compare the number" with a commitment, the same scheme Bluetooth
//! uses (numeric comparison):
//!   A → B  pair_hello     { A's id, name, public key PKa }
//!   B → A  pair_hello_ack { B's id, name, PKb, commit = HMAC(Nb, PKb ‖ PKa) }
//!   A → B  nonce          { Na }
//!   B → A  nonce          { Nb }            A checks the commitment
//!   both show code = SHA-256(PKa ‖ PKb ‖ Na ‖ Nb) as 6 digits
//!   A ↔ B  decision       { accept }         after each user compares the screens
//! B commits to Nb before seeing Na, so a device in the middle cannot steer the
//! two codes to match; it has a one-in-a-million chance per try. B only accepts
//! requests while "Allow pairing" is on, at most 3 per window.
//!
//! Status check (every 5 s per paired device):
//!   A → B  ping { A's id, random nonce }
//!   B → A  pong { HMAC(X25519(B, A), nonce ‖ B's id ‖ A's id) }
//! Only the real B can answer, so "verified" means it is the device you paired with.

use crate::{
    identity::{self, Identity},
    state::{lock, Check, PairWindow, Phase, Role, SharedState, PAIR_ATTEMPTS, PAIR_WINDOW},
    store::{self, PairedPeer},
    sysstats::{self, Stats},
};
use base64::Engine as _;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{Shutdown, SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{mpsc, Arc},
    thread,
    time::{Duration, Instant},
};

pub const DEFAULT_TCP_PORT: u16 = 47801;
pub const PROTO: u8 = 2;
const CHECK_INTERVAL: Duration = Duration::from_secs(5);
const DECIDE_TIMEOUT: Duration = Duration::from_secs(120);
pub const LABEL_COMMIT: &[u8] = b"nexus-commit-v2";
pub const LABEL_SAS: &[u8] = b"nexus-sas-v2";
pub const LABEL_PING: &[u8] = b"nexus-ping-v2";
pub const LABEL_STATS: &[u8] = b"nexus-stats-v2";
pub const LABEL_CMD: &[u8] = b"nexus-cmd-v2";
pub const LABEL_CMDACK: &[u8] = b"nexus-cmdack-v2";
pub const LABEL_FS: &[u8] = b"nexus-fs-v2";
pub const LABEL_SCREEN: &[u8] = b"nexus-screen-v2";
/// Keys for the encrypted part of file and screen links, one per direction.
pub const LABEL_LINK_C2S: &[u8] = b"nexus-link-c2s-v1";
pub const LABEL_LINK_S2C: &[u8] = b"nexus-link-s2c-v1";
/// UDP port the Wake-on-LAN magic packet is sent to (9 = the usual "discard" port).
pub const WOL_PORT: u16 = 9;
/// File data travels in chunks of this many bytes (sent as hex inside a JSON line).
const FS_CHUNK: usize = 256 * 1024;
/// The longest JSON line we accept, raised from 64 KiB so a file chunk fits.
const MAX_LINE: u64 = 8 * 1024 * 1024;

/// Remote actions a paired device may ask this one to perform.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Lock,
    Sleep,
    Restart,
    Shutdown,
}

impl Command {
    pub fn wire(self) -> &'static str {
        match self {
            Command::Lock => "lock",
            Command::Sleep => "sleep",
            Command::Restart => "restart",
            Command::Shutdown => "shutdown",
        }
    }
    fn parse(s: &str) -> Option<Self> {
        match s {
            "lock" => Some(Command::Lock),
            "sleep" => Some(Command::Sleep),
            "restart" => Some(Command::Restart),
            "shutdown" => Some(Command::Shutdown),
            _ => None,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Command::Lock => "Lock",
            Command::Sleep => "Sleep",
            Command::Restart => "Restart",
            Command::Shutdown => "Shut down",
        }
    }
}

type HmacSha256 = Hmac<Sha256>;

#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "t", rename_all = "snake_case")]
enum Msg {
    PairHello {
        v: u8,
        id: String,
        name: String,
        os: String,
        public_key: String,
    },
    PairHelloAck {
        v: u8,
        id: String,
        name: String,
        os: String,
        public_key: String,
        commit: String,
    },
    Nonce {
        nonce: String,
    },
    Decision {
        accept: bool,
    },
    Ping {
        id: String,
        nonce: String,
    },
    Pong {
        mac: String,
        /// Optional live stats as compact JSON, signed separately so older versions can ignore them.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stats: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stats_mac: Option<String>,
    },
    /// "Do this" from a paired device: lock, sleep, restart or shut down.
    Command {
        id: String,
        nonce: String,
        action: String,
        mac: String,
    },
    CommandAck {
        ok: bool,
        message: String,
        mac: String,
    },
    /// Opens an authenticated file-transfer connection.
    FsHello {
        id: String,
        nonce: String,
    },
    FsReady {
        nonce: String,
        mac: String,
    },
    FsAuth {
        mac: String,
    },
    /// Opens an authenticated live screen-sharing connection (viewer → host).
    ScreenHello {
        id: String,
        nonce: String,
    },
    ScreenReady {
        nonce: String,
        mac: String,
    },
    ScreenAuth {
        mac: String,
    },
    Error {
        reason: String,
    },
}

/* ------------------------------------------------------------- crypto */

fn length_prefixed(parts: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::new();
    for p in parts {
        out.extend_from_slice(&(p.len() as u32).to_be_bytes());
        out.extend_from_slice(p);
    }
    out
}

fn hmac_of(key: &[u8], parts: &[&[u8]]) -> HmacSha256 {
    let mut m = <HmacSha256 as Mac>::new_from_slice(key).expect("HMAC takes any key length");
    m.update(&length_prefixed(parts));
    m
}

pub fn mac(key: &[u8], parts: &[&[u8]]) -> Vec<u8> {
    hmac_of(key, parts).finalize().into_bytes().to_vec()
}

/// Constant-time check of a hex MAC.
fn mac_ok(key: &[u8], parts: &[&[u8]], tag_hex: &str) -> bool {
    hex::decode(tag_hex).is_ok_and(|tag| hmac_of(key, parts).verify_slice(&tag).is_ok())
}

pub fn commitment(nb: &[u8], pkb: &[u8], pka: &[u8]) -> Vec<u8> {
    mac(nb, &[LABEL_COMMIT, pkb, pka])
}

/// The 6-digit number both screens show. A is the device that clicked Pair.
pub fn pairing_code(pka: &[u8], pkb: &[u8], na: &[u8], nb: &[u8]) -> String {
    let h = Sha256::digest(length_prefixed(&[LABEL_SAS, pka, pkb, na, nb]));
    format!("{:06}", u32::from_be_bytes([h[0], h[1], h[2], h[3]]) % 1_000_000)
}

pub fn ping_mac(shared_secret: &[u8], nonce: &[u8], responder_id: &str, requester_id: &str) -> Vec<u8> {
    mac(shared_secret, &[LABEL_PING, nonce, responder_id.as_bytes(), requester_id.as_bytes()])
}

/// Signs a command so only the paired device could have asked for it.
pub fn command_mac(ss: &[u8], nonce: &[u8], action: &str, responder_id: &str, requester_id: &str) -> Vec<u8> {
    mac(ss, &[LABEL_CMD, nonce, action.as_bytes(), responder_id.as_bytes(), requester_id.as_bytes()])
}

/// Signs the reply so the asker knows the answer really came from that device.
pub fn command_ack_mac(ss: &[u8], nonce: &[u8], message: &str) -> Vec<u8> {
    mac(ss, &[LABEL_CMDACK, nonce, message.as_bytes()])
}

fn unhex(s: &str, len: usize) -> Result<Vec<u8>, String> {
    hex::decode(s).ok().filter(|b| b.len() == len).ok_or_else(unexpected)
}

fn unexpected() -> String {
    "The other device sent something unexpected".into()
}

/* --------------------------------------------------------------- wire */

/// One direction of an encrypted link: ChaCha20-Poly1305 with a message counter as nonce.
struct Sealer {
    cipher: chacha20poly1305::ChaCha20Poly1305,
    counter: u64,
}

impl Sealer {
    fn new(key: &[u8]) -> Self {
        use chacha20poly1305::KeyInit;
        Sealer {
            cipher: chacha20poly1305::ChaCha20Poly1305::new(chacha20poly1305::Key::from_slice(&key[..32])),
            counter: 0,
        }
    }
    fn nonce(&mut self) -> [u8; 12] {
        let mut n = [0u8; 12];
        n[4..].copy_from_slice(&self.counter.to_le_bytes());
        self.counter += 1;
        n
    }
    fn seal(&mut self, plain: &[u8]) -> Result<Vec<u8>, String> {
        use chacha20poly1305::aead::Aead;
        let n = self.nonce();
        self.cipher.encrypt(chacha20poly1305::Nonce::from_slice(&n), plain).map_err(|_| "Could not encrypt".to_string())
    }
    fn open(&mut self, sealed: &[u8]) -> Result<Vec<u8>, String> {
        use chacha20poly1305::aead::Aead;
        let n = self.nonce();
        self.cipher
            .decrypt(chacha20poly1305::Nonce::from_slice(&n), sealed)
            .map_err(|_| "A message failed its security check (it was changed on the way)".to_string())
    }
}

/// The two directional keys of a link, from the shared secret and both nonces.
fn link_keys(ss: &[u8], nonce_c: &[u8], nonce_s: &[u8]) -> (Vec<u8>, Vec<u8>) {
    (mac(ss, &[LABEL_LINK_C2S, nonce_c, nonce_s]), mac(ss, &[LABEL_LINK_S2C, nonce_c, nonce_s]))
}

/// Newline-delimited JSON over TCP. After a file or screen handshake, every line is
/// encrypted: `{"x":"<base64 ciphertext+tag>"}`.
struct Conn {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    tx: Option<Sealer>,
    rx: Option<Sealer>,
}

impl Conn {
    fn new(s: TcpStream, timeout: Duration) -> Result<Self, String> {
        s.set_read_timeout(Some(timeout)).map_err(|e| e.to_string())?;
        s.set_write_timeout(Some(Duration::from_secs(10))).map_err(|e| e.to_string())?;
        let _ = s.set_nodelay(true);
        let writer = s.try_clone().map_err(|e| e.to_string())?;
        Ok(Self { reader: BufReader::new(s), writer, tx: None, rx: None })
    }
    /// From now on every line in both directions is encrypted.
    fn secure(&mut self, send_key: &[u8], recv_key: &[u8]) {
        self.tx = Some(Sealer::new(send_key));
        self.rx = Some(Sealer::new(recv_key));
    }
    fn write_line(&mut self, text: String) -> Result<(), String> {
        let mut line = match &mut self.tx {
            Some(tx) => {
                let sealed = tx.seal(text.as_bytes())?;
                serde_json::json!({ "x": base64::engine::general_purpose::STANDARD.encode(sealed) }).to_string()
            }
            None => text,
        };
        line.push('\n');
        self.writer.write_all(line.as_bytes()).map_err(|_| "Connection lost".to_string())
    }
    fn read_line(&mut self) -> Result<String, String> {
        let mut line = String::new();
        let n = self
            .reader
            .by_ref()
            .take(MAX_LINE)
            .read_line(&mut line)
            .map_err(|_| "No answer from the other device".to_string())?;
        if n == 0 {
            return Err("The other device closed the connection".into());
        }
        let Some(rx) = &mut self.rx else { return Ok(line) };
        let v: serde_json::Value = serde_json::from_str(&line).map_err(|_| "Garbled message".to_string())?;
        // The handshake's own refusal (e.g. a failed security check) arrives unencrypted.
        if v.get("t").and_then(|t| t.as_str()) == Some("error") {
            return Ok(line);
        }
        let Some(x) = v.get("x").and_then(|x| x.as_str()) else {
            return Err(
                "The other device doesn't encrypt yet — update Nexus on both devices (build 32 or newer)".into()
            );
        };
        let sealed = base64::engine::general_purpose::STANDARD.decode(x).map_err(|_| "Garbled message".to_string())?;
        String::from_utf8(rx.open(&sealed)?).map_err(|_| "Garbled message".to_string())
    }
    fn send(&mut self, m: &Msg) -> Result<(), String> {
        let text = serde_json::to_string(m).map_err(|e| e.to_string())?;
        self.write_line(text)
    }
    /// Reads one message. An `error` message from the other side becomes `Err(reason)`.
    fn recv(&mut self) -> Result<Msg, String> {
        let line = self.read_line()?;
        match serde_json::from_str(&line).map_err(|_| "Garbled message".to_string())? {
            Msg::Error { reason } => Err(reason),
            m => Ok(m),
        }
    }
    /// A JSON line (requests, file data, frames) after both sides have authenticated.
    fn send_val(&mut self, v: &serde_json::Value) -> Result<(), String> {
        self.write_line(v.to_string())
    }
    fn recv_val(&mut self) -> Result<serde_json::Value, String> {
        let line = self.read_line()?;
        serde_json::from_str(&line).map_err(|_| "Garbled message".to_string())
    }
}

fn send_msg(w: &mut TcpStream, m: &Msg) -> Result<(), String> {
    let mut line = serde_json::to_string(m).map_err(|e| e.to_string())?;
    line.push('\n');
    w.write_all(line.as_bytes()).map_err(|_| "Connection lost".to_string())
}

/// Reads one message. An `error` message from the other side becomes `Err(reason)`.
fn read_msg(r: &mut BufReader<TcpStream>) -> Result<Msg, String> {
    let mut line = String::new();
    let n = r.take(MAX_LINE).read_line(&mut line).map_err(|_| "No answer from the other device".to_string())?;
    if n == 0 {
        return Err("The other device closed the connection".into());
    }
    match serde_json::from_str(&line).map_err(|_| "Garbled message".to_string())? {
        Msg::Error { reason } => Err(reason),
        m => Ok(m),
    }
}

/* ------------------------------------------------------------- server */

/// Listens on `preferred` (or any free port if taken) and returns the port used.
pub fn start_server(me: Arc<Identity>, shared: SharedState, dir: PathBuf, preferred: u16) -> Result<u16, String> {
    let listener = TcpListener::bind(("0.0.0.0", preferred))
        .or_else(|_| TcpListener::bind(("0.0.0.0", 0)))
        .map_err(|e| format!("Cannot open a TCP port: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    thread::Builder::new()
        .name("nexus-server".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let (me, shared, dir) = (me.clone(), shared.clone(), dir.clone());
                thread::spawn(move || handle(stream, &me, &shared, &dir));
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(port)
}

fn handle(stream: TcpStream, me: &Arc<Identity>, shared: &SharedState, dir: &Path) {
    let Ok(mut c) = Conn::new(stream, Duration::from_secs(15)) else { return };
    match c.recv() {
        Ok(Msg::Ping { id, nonce }) => {
            let key = lock(shared).paired.iter().find(|p| p.id == id).map(|p| p.public_key.clone());
            let reply = match (key.as_deref().and_then(identity::decode_public), hex::decode(&nonce)) {
                (Some(pk), Ok(n)) => {
                    let ss = me.secret.diffie_hellman(&pk);
                    let stats = sysstats::latest_json();
                    let stats_mac = stats
                        .as_ref()
                        .map(|j| hex::encode(mac(ss.as_bytes(), &[LABEL_STATS, n.as_slice(), j.as_bytes()])));
                    Msg::Pong { mac: hex::encode(ping_mac(ss.as_bytes(), &n, &me.id, &id)), stats, stats_mac }
                }
                _ => Msg::Error { reason: format!("{} no longer trusts this device. Pair again.", me.name) },
            };
            let _ = c.send(&reply);
        }
        Ok(Msg::Command { id, nonce, action, mac: tag }) => host_command(c, me, shared, id, nonce, action, tag),
        Ok(Msg::FsHello { id, nonce }) => fs_serve(c, me, shared, id, nonce),
        Ok(Msg::ScreenHello { id, nonce }) => screen_serve(c, me, shared, dir, id, nonce),
        Ok(Msg::PairHello { v, id, name, os, public_key }) => {
            host_pair(c, me, shared, dir, v, id, name, os, public_key)
        }
        _ => {}
    }
}

/* ----------------------------------------------------------- file transfer */

/// One entry in a directory listing.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    pub name: String,
    pub dir: bool,
    pub size: u64,
}

/// The folder this device shares. Everything is resolved under here; paths can't escape it.
/// Override with NEXUS_FS_ROOT (used by the tests).
pub fn fs_root() -> PathBuf {
    if let Ok(d) = std::env::var("NEXUS_FS_ROOT") {
        return PathBuf::from(d);
    }
    directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf()).unwrap_or_else(|| PathBuf::from("."))
}

/// Joins a client-supplied relative path to the shared root and refuses anything that escapes it.
fn fs_resolve(root: &Path, rel: &str) -> Result<PathBuf, String> {
    let mut p = root.to_path_buf();
    for part in rel.split('/') {
        match part {
            "" | "." => {}
            ".." => return Err("That path is not allowed".into()),
            _ => p.push(part),
        }
    }
    // Canonicalize when it exists, then re-check it is still inside the root.
    let canon_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    if let Ok(c) = p.canonicalize() {
        if !c.starts_with(&canon_root) {
            return Err("That path is not allowed".into());
        }
        return Ok(c);
    }
    Ok(p)
}

/// Serves file requests to a paired device over one authenticated connection.
fn fs_serve(mut c: Conn, me: &Identity, shared: &SharedState, id: String, nonce_c_hex: String) {
    let setup = (|| -> Result<(), String> {
        let peer = lock(shared).paired.iter().find(|p| p.id == id).cloned();
        let (peer, nonce_c) = match (peer, hex::decode(&nonce_c_hex)) {
            (Some(p), Ok(n)) => (p, n),
            _ => return Err(format!("{} does not trust this device", me.name)),
        };
        let pk = identity::decode_public(&peer.public_key).ok_or("Stored key is damaged")?;
        let ss = me.secret.diffie_hellman(&pk).as_bytes().to_vec();
        let nonce_s: [u8; 16] = rand::random();
        let mac_s = mac(&ss, &[LABEL_FS, &nonce_c, &nonce_s, me.id.as_bytes(), id.as_bytes()]);
        c.send(&Msg::FsReady { nonce: hex::encode(nonce_s), mac: hex::encode(mac_s) })?;
        let Msg::FsAuth { mac: mac_c } = c.recv()? else { return Err(unexpected()) };
        if !mac_ok(&ss, &[LABEL_FS, &nonce_s, &nonce_c, id.as_bytes(), me.id.as_bytes()], &mac_c) {
            return Err("The other device failed the file-transfer security check".into());
        }
        let (c2s, s2c) = link_keys(&ss, &nonce_c, &nonce_s);
        c.secure(&s2c, &c2s);
        Ok(())
    })();
    if let Err(e) = setup {
        let _ = c.send(&Msg::Error { reason: e });
        return;
    }
    let who = peer_name(shared, &id);
    lock(shared).log(format!("{who} opened a file connection"));
    let root = fs_root();
    // Trusted request loop. Each op is a plain JSON line.
    while let Ok(v) = c.recv_val() {
        let op = v.get("op").and_then(|x| x.as_str()).unwrap_or("");
        let result = match op {
            "list" => fs_do_list(&mut c, &root, &v),
            "get" => fs_do_get(&mut c, &root, &v),
            "put" => fs_do_put(&mut c, &root, &v),
            "bye" => break,
            _ => Err("Unknown request".into()),
        };
        if let Err(e) = result {
            let _ = c.send_val(&serde_json::json!({ "error": e }));
        }
    }
}

fn peer_name(shared: &SharedState, id: &str) -> String {
    lock(shared).paired.iter().find(|p| p.id == id).map(|p| p.name.clone()).unwrap_or_else(|| "A device".into())
}

/* ------------------------------------------------------------ screen share */

/// Shared handshake for a screen session; returns the X25519 shared secret.
fn screen_auth_host(
    c: &mut Conn,
    me: &Identity,
    shared: &SharedState,
    id: &str,
    nonce_c_hex: &str,
) -> Result<(), String> {
    let peer = lock(shared).paired.iter().find(|p| p.id == id).cloned();
    let (peer, nonce_c) = match (peer, hex::decode(nonce_c_hex)) {
        (Some(p), Ok(n)) => (p, n),
        _ => return Err(format!("{} does not trust this device", me.name)),
    };
    let pk = identity::decode_public(&peer.public_key).ok_or("Stored key is damaged")?;
    let ss = me.secret.diffie_hellman(&pk).as_bytes().to_vec();
    let nonce_s: [u8; 16] = rand::random();
    let mac_s = mac(&ss, &[LABEL_SCREEN, &nonce_c, &nonce_s, me.id.as_bytes(), id.as_bytes()]);
    c.send(&Msg::ScreenReady { nonce: hex::encode(nonce_s), mac: hex::encode(mac_s) })?;
    let Msg::ScreenAuth { mac: mac_c } = c.recv()? else { return Err(unexpected()) };
    if !mac_ok(&ss, &[LABEL_SCREEN, &nonce_s, &nonce_c, id.as_bytes(), me.id.as_bytes()], &mac_c) {
        return Err("The other device failed the screen-sharing security check".into());
    }
    let (c2s, s2c) = link_keys(&ss, &nonce_c, &nonce_s);
    c.secure(&s2c, &c2s);
    Ok(())
}

/// Streams the primary display to a paired viewer, frame by frame, until it disconnects.
fn screen_serve(mut c: Conn, me: &Arc<Identity>, shared: &SharedState, dir: &Path, id: String, nonce_c_hex: String) {
    if let Err(e) = screen_auth_host(&mut c, me, shared, &id, &nonce_c_hex) {
        let _ = c.send(&Msg::Error { reason: e });
        return;
    }
    // The viewer's first line says what it wants: a picture stream ("start", with its
    // preferred frame rate and size) or a remote-control link ("control").
    let start = match c.recv_val() {
        Ok(v) => v,
        Err(_) => return,
    };
    match start.get("op").and_then(|o| o.as_str()) {
        Some("control") => return control_serve(c, shared, &id),
        Some("monitor") => return monitor_serve(c, shared, &id),
        Some("auto") => return auto_serve(c, me, shared, dir, &id),
        Some("clip") => return clip_serve(c, shared, &id, &start),
        Some("clipfiles") => return clipfiles_serve(c, shared, dir, &id, &start),
        Some("audio") => return audio_serve(c, shared, &id),
        _ => {}
    }
    let fps = start.get("fps").and_then(|x| x.as_u64()).unwrap_or(8).clamp(1, 30);
    let max_w = start.get("maxw").and_then(|x| x.as_u64()).unwrap_or(1280).clamp(320, 3840) as u32;
    let quality = start.get("quality").and_then(|x| x.as_u64()).unwrap_or(55).clamp(20, 90) as u8;
    let monitor = start.get("monitor").and_then(|x| x.as_u64()).unwrap_or(0).min(16) as usize;
    let who = peer_name(shared, &id);
    lock(shared).log(format!("{who} is viewing this screen"));
    // Don't let this PC go to sleep (or its screen turn off) while someone is watching it.
    crate::screen::keep_display_awake(true);
    let gap = Duration::from_millis(1000 / fps);
    let mut seq: u64 = 0;
    let mut waiting_since: Option<Instant> = None;
    loop {
        let started = Instant::now();
        match crate::screen::capture_jpeg(monitor, max_w, quality) {
            Ok((w, h, jpeg, mon, mons)) => {
                let data = base64::engine::general_purpose::STANDARD.encode(&jpeg);
                let frame = serde_json::json!({
                    "op": "frame", "w": w, "h": h, "seq": seq, "data": data, "mon": mon, "mons": mons,
                });
                if c.send_val(&frame).is_err() {
                    break; // viewer closed the window
                }
                seq += 1;
                waiting_since = None;
            }
            Err(e) => {
                // The screen can't be read for a moment (the lock screen, a UAC prompt, a
                // monitor waking up). Keep the link open and try again instead of stopping.
                if waiting_since.map_or(true, |t| t.elapsed() >= Duration::from_secs(1)) {
                    if c.send_val(&serde_json::json!({ "op": "wait", "why": e })).is_err() {
                        break;
                    }
                    waiting_since = Some(Instant::now());
                }
                thread::sleep(Duration::from_millis(300));
            }
        }
        let spent = started.elapsed();
        if spent < gap {
            thread::sleep(gap - spent);
        }
    }
    crate::screen::keep_display_awake(false);
    lock(shared).log(format!("{who} stopped viewing this screen"));
}

/// Puts text from a paired device on this PC's clipboard (if shared clipboard is on).
fn clip_serve(mut c: Conn, shared: &SharedState, id: &str, start: &serde_json::Value) {
    let who = peer_name(shared, id);
    let text = start.get("text").and_then(|t| t.as_str()).unwrap_or("");
    let on = lock(shared).share_clipboard;
    let reply = if !on {
        serde_json::json!({ "error": "Shared clipboard is turned off on this PC" })
    } else {
        match crate::clipboard::receive(shared, text) {
            Ok(()) => {
                lock(shared).log(format!("Clipboard from {who}"));
                serde_json::json!({ "op": "ok" })
            }
            Err(e) => serde_json::json!({ "error": e }),
        }
    };
    let _ = c.send_val(&reply);
}

/// Sends what this PC is playing to a paired viewer until it disconnects.
fn audio_serve(mut c: Conn, shared: &SharedState, id: &str) {
    let who = peer_name(shared, id);
    let (stream, rate, rx) = match crate::audio::start_capture() {
        Ok(x) => x,
        Err(e) => {
            let _ = c.send_val(&serde_json::json!({ "error": e }));
            return;
        }
    };
    if c.send_val(&serde_json::json!({ "op": "audio", "rate": rate, "channels": 2 })).is_err() {
        return;
    }
    lock(shared).log(format!("{who} is listening to this PC's sound"));
    loop {
        let msg = match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(samples) => {
                let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
                serde_json::json!({ "pcm": base64::engine::general_purpose::STANDARD.encode(&bytes) })
            }
            // Nothing is playing: Windows sends no sound, so say so to keep the link open.
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => serde_json::json!({ "op": "quiet" }),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        };
        if c.send_val(&msg).is_err() {
            break; // the viewer turned sound off
        }
    }
    drop(stream);
    lock(shared).log(format!("{who} stopped listening"));
}

/// Plays a paired PC's sound on this PC's speakers, on its own thread, until `stop`
/// is set or the link ends. `on_error` is told why it stopped if it failed.
pub fn audio_session<E>(
    me: Arc<Identity>,
    peer: PairedPeer,
    addr: SocketAddr,
    stop: Arc<std::sync::atomic::AtomicBool>,
    on_error: E,
) where
    E: Fn(String) + Send + 'static,
{
    thread::spawn(move || {
        let run = || -> Result<(), String> {
            let mut c = screen_open(&me, &peer, addr)?;
            c.send_val(&serde_json::json!({ "op": "audio" }))?;
            let head = c.recv_val()?;
            if let Some(e) = fs_err(&head) {
                return Err(e);
            }
            if head.get("op").and_then(|o| o.as_str()) != Some("audio") {
                return Err("Update Nexus on that device to hear its sound".into());
            }
            let rate = head.get("rate").and_then(|r| r.as_u64()).unwrap_or(48_000) as u32;
            let player = crate::audio::Player::new()?;
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                let v = c.recv_val()?;
                if let Some(e) = fs_err(&v) {
                    return Err(e);
                }
                if let Some(d) = v.get("pcm").and_then(|x| x.as_str()) {
                    let bytes =
                        base64::engine::general_purpose::STANDARD.decode(d).map_err(|_| "Garbled sound".to_string())?;
                    let samples: Vec<i16> = bytes.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect();
                    player.push(rate, &samples);
                }
            }
            Ok(())
        };
        if let Err(e) = run() {
            if !stop.load(std::sync::atomic::Ordering::Relaxed) {
                on_error(e);
            }
        }
    });
}

/// One file or folder in a copied-files transfer.
pub struct ClipItem {
    /// Path inside the copy, with "/" between folders, e.g. "Photos/beach.jpg".
    pub rel: String,
    pub abs: PathBuf,
    pub size: u64,
    pub dir: bool,
}

const CLIP_CHUNK: usize = 256 * 1024;

/// Sends files copied on this PC to a paired PC, which puts them on its clipboard.
pub(crate) fn clipfiles_send(
    me: &Identity,
    peer: &PairedPeer,
    addr: SocketAddr,
    items: &[ClipItem],
) -> Result<(), String> {
    let mut c = screen_open(me, peer, addr)?;
    let list: Vec<serde_json::Value> =
        items.iter().map(|i| serde_json::json!({ "path": i.rel, "size": i.size, "dir": i.dir })).collect();
    c.send_val(&serde_json::json!({ "op": "clipfiles", "items": list }))?;
    let ready = c.recv_val()?;
    if let Some(e) = fs_err(&ready) {
        return Err(e);
    }
    let mut buf = vec![0u8; CLIP_CHUNK];
    for i in items.iter().filter(|i| !i.dir) {
        let mut f = std::fs::File::open(&i.abs).map_err(|e| format!("Cannot open {}: {e}", i.abs.display()))?;
        loop {
            let n = f.read(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            let data = base64::engine::general_purpose::STANDARD.encode(&buf[..n]);
            c.send_val(&serde_json::json!({ "data": data }))?;
        }
        c.send_val(&serde_json::json!({ "next": true }))?;
    }
    c.send_val(&serde_json::json!({ "done": true }))?;
    let done = c.recv_val()?;
    match fs_err(&done) {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// Receives files copied on a paired PC and puts them on this PC's clipboard.
fn clipfiles_serve(mut c: Conn, shared: &SharedState, dir: &Path, id: &str, start: &serde_json::Value) {
    let who = peer_name(shared, id);
    let result = (|| -> Result<usize, String> {
        if !lock(shared).share_clipboard {
            return Err("Shared clipboard is turned off on this PC".into());
        }
        let items = start.get("items").and_then(|x| x.as_array()).cloned().unwrap_or_default();
        let (mut files, mut dirs, mut tops) = (Vec::new(), Vec::new(), Vec::<String>::new());
        let mut total = 0u64;
        for it in &items {
            let rel = it.get("path").and_then(|x| x.as_str()).unwrap_or("");
            let safe = crate::clipboard::safe_rel(rel).ok_or("A file name was not allowed")?;
            let top = safe.split('/').next().unwrap_or("").to_string();
            if !tops.contains(&top) {
                tops.push(top);
            }
            if it.get("dir").and_then(|x| x.as_bool()) == Some(true) {
                dirs.push(safe);
            } else {
                total += it.get("size").and_then(|x| x.as_u64()).unwrap_or(0);
                files.push(safe);
            }
        }
        if files.is_empty() && dirs.is_empty() {
            return Err("Nothing to paste".into());
        }
        if total > crate::clipboard::MAX_FILES_BYTES || items.len() > crate::clipboard::MAX_FILES_COUNT {
            return Err("Too much for the clipboard — use Browse files instead".into());
        }
        let stage = crate::clipboard::new_stage(dir)?;
        lock(shared).log(format!("Receiving copied files from {who}…"));
        c.send_val(&serde_json::json!({ "op": "ready" }))?;
        for d in &dirs {
            std::fs::create_dir_all(stage.join(d)).map_err(|e| e.to_string())?;
        }
        for rel in &files {
            let path = stage.join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let mut f = std::fs::File::create(&path).map_err(|e| format!("Cannot create {}: {e}", path.display()))?;
            loop {
                let v = c.recv_val()?;
                if v.get("next").is_some() {
                    break;
                }
                let d = v.get("data").and_then(|x| x.as_str()).ok_or_else(unexpected)?;
                let bytes =
                    base64::engine::general_purpose::STANDARD.decode(d).map_err(|_| "Garbled file data".to_string())?;
                f.write_all(&bytes).map_err(|e| e.to_string())?;
            }
        }
        if c.recv_val()?.get("done").is_none() {
            return Err(unexpected());
        }
        let top_paths: Vec<PathBuf> = tops.iter().map(|t| stage.join(t)).collect();
        crate::clipboard::receive_files(shared, &top_paths)?;
        Ok(files.len())
    })();
    match result {
        Ok(n) => {
            lock(shared).log(format!("{n} copied file{} from {who} — ready to paste", if n == 1 { "" } else { "s" }));
            let _ = c.send_val(&serde_json::json!({ "op": "ok" }));
        }
        Err(e) => {
            lock(shared).log(format!("Copied files from {who} did not arrive: {e}"));
            let _ = c.send_val(&serde_json::json!({ "error": e }));
        }
    }
}

/// Sends copied text to a paired device's clipboard.
pub(crate) fn clip_send(me: &Identity, peer: &PairedPeer, addr: SocketAddr, text: &str) -> Result<(), String> {
    let mut c = screen_open(me, peer, addr)?;
    c.send_val(&serde_json::json!({ "op": "clip", "text": text }))?;
    let answer = c.recv_val()?;
    match fs_err(&answer) {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// The automation list as sent to a paired device.
fn auto_listing(shared: &SharedState) -> serde_json::Value {
    let s = lock(shared);
    let devices: Vec<serde_json::Value> =
        s.paired.iter().map(|p| serde_json::json!({ "id": p.id, "name": p.name, "os": p.os })).collect();
    serde_json::json!({
        "op": "rules",
        "rules": s.automations,
        "devices": devices,
        "can_edit": s.allow_control,
    })
}

/// Lets a paired device read, change and test this PC's automations. Changing or
/// running them needs "Allow remote control", since a rule can open programs.
fn auto_serve(mut c: Conn, me: &Arc<Identity>, shared: &SharedState, dir: &Path, id: &str) {
    let who = peer_name(shared, id);
    lock(shared).log(format!("{who} opened Automation for this PC"));
    let locked = serde_json::json!({
        "error": "Changing automations needs \"Allow remote control\" turned on in this PC's Settings"
    });
    while let Ok(v) = c.recv_val() {
        let op = v.get("op").and_then(|o| o.as_str()).unwrap_or("").to_string();
        let reply = match op.as_str() {
            "bye" => break,
            "list" => auto_listing(shared),
            "save" | "run" if !lock(shared).allow_control => locked.clone(),
            "save" => {
                let parsed: Result<Vec<crate::automation::Rule>, String> =
                    serde_json::from_value(v.get("rules").cloned().unwrap_or_default())
                        .map_err(|_| "The automations could not be read".to_string());
                match parsed.and_then(|rules| crate::automation::replace(shared, dir, rules)) {
                    Ok(_) => {
                        lock(shared).log(format!("{who} changed the automations"));
                        auto_listing(shared)
                    }
                    Err(e) => serde_json::json!({ "error": e }),
                }
            }
            "run" => {
                let rid = v.get("id").and_then(|x| x.as_u64()).unwrap_or(0);
                let rule = lock(shared).automations.iter().find(|r| r.id == rid).cloned();
                match rule {
                    Some(r) => {
                        crate::automation::run_rule(me, shared, &r, &format!(" (run now by {who})"));
                        serde_json::json!({ "op": "ran", "id": rid })
                    }
                    None => serde_json::json!({ "error": "That automation no longer exists" }),
                }
            }
            _ => continue,
        };
        if c.send_val(&reply).is_err() {
            break;
        }
    }
}

/// Answers "snapshot" and "end" requests from a paired viewer's monitor window.
fn monitor_serve(mut c: Conn, shared: &SharedState, id: &str) {
    let who = peer_name(shared, id);
    lock(shared).log(format!("{who} opened the monitor for this PC"));
    let mut mon = crate::monitor::Monitor::new();
    while let Ok(v) = c.recv_val() {
        let reply = match v.get("op").and_then(|o| o.as_str()) {
            Some("bye") => break,
            Some("snapshot") => mon.snapshot(lock(shared).allow_control),
            Some("end") => {
                let name = v.get("name").and_then(|n| n.as_str()).unwrap_or("");
                if !lock(shared).allow_control {
                    serde_json::json!({
                        "error": "Ending apps needs \"Allow remote control\" turned on in this PC's Settings"
                    })
                } else {
                    match mon.end_app(name) {
                        Ok(n) => {
                            lock(shared).log(format!("{who} ended {name}"));
                            serde_json::json!({ "op": "ended", "name": name, "count": n })
                        }
                        Err(e) => serde_json::json!({ "error": e }),
                    }
                }
            }
            _ => continue,
        };
        if c.send_val(&reply).is_err() {
            break;
        }
    }
    lock(shared).log(format!("{who} closed the monitor"));
}

/// Opens a monitor link to a paired device on its own thread. It asks for a fresh
/// snapshot every 2 seconds and passes each answer (or error) to `on_update`. Send
/// `{"op":"end","name":…}` on the returned sender to end an app; drop it to close.
pub fn monitor_session<U>(
    me: Arc<Identity>,
    peer: PairedPeer,
    addr: SocketAddr,
    on_update: U,
) -> std::sync::mpsc::Sender<serde_json::Value>
where
    U: Fn(Result<serde_json::Value, String>) + Send + 'static,
{
    use std::sync::mpsc::{channel, RecvTimeoutError};
    let (tx, rx) = channel::<serde_json::Value>();
    thread::spawn(move || {
        let run = || -> Result<(), String> {
            let mut c = screen_open(&me, &peer, addr)?;
            c.send_val(&serde_json::json!({ "op": "monitor" }))?;
            // Snapshot errors replace the picture; answers to commands (such as a
            // failed "end") are passed on as they are, so they show as a notice.
            let ask = |c: &mut Conn, v: &serde_json::Value, raw: bool| -> Result<(), String> {
                c.send_val(v)?;
                let answer = c.recv_val()?;
                on_update(match fs_err(&answer) {
                    Some(e) if !raw => Err(e),
                    _ => Ok(answer),
                });
                Ok(())
            };
            let snapshot = serde_json::json!({ "op": "snapshot" });
            ask(&mut c, &snapshot, false)?;
            loop {
                match rx.recv_timeout(Duration::from_secs(2)) {
                    Ok(cmd) => {
                        ask(&mut c, &cmd, true)?;
                        ask(&mut c, &snapshot, false)?;
                    }
                    Err(RecvTimeoutError::Timeout) => ask(&mut c, &snapshot, false)?,
                    Err(RecvTimeoutError::Disconnected) => {
                        let _ = c.send_val(&serde_json::json!({ "op": "bye" }));
                        return Ok(());
                    }
                }
            }
        };
        if let Err(e) = run() {
            on_update(Err(e));
        }
    });
    tx
}

/// Applies mouse and keyboard events from a paired viewer until it disconnects.
/// The viewer pings every few seconds so the link stays open while idle.
fn control_serve(mut c: Conn, shared: &SharedState, id: &str) {
    if !lock(shared).allow_control {
        let _ = c.send_val(&serde_json::json!({
            "error": "Remote control is turned off on this PC (Settings → Allow remote control)"
        }));
        return;
    }
    if c.send_val(&serde_json::json!({ "op": "ready" })).is_err() {
        return;
    }
    let who = peer_name(shared, id);
    lock(shared).log(format!("{who} is controlling this PC"));
    while let Ok(v) = c.recv_val() {
        match v.get("op").and_then(|o| o.as_str()) {
            Some("bye") => break,
            Some("ping") | None => {}
            Some(_) => {
                if !lock(shared).allow_control {
                    break; // turned off mid-session
                }
                crate::input::apply(&v);
            }
        }
    }
    lock(shared).log(format!("{who} stopped controlling this PC"));
}

/// Opens a remote-control link to a paired device on its own thread and returns a
/// sender for input events (see `input.rs` for their shape). `on_status` reports
/// when the link is ready, or why it failed. Dropping the sender closes the link.
pub fn control_session<S>(
    me: Arc<Identity>,
    peer: PairedPeer,
    addr: SocketAddr,
    on_status: S,
) -> std::sync::mpsc::Sender<serde_json::Value>
where
    S: Fn(Result<(), String>) + Send + 'static,
{
    use std::sync::mpsc::{channel, RecvTimeoutError};
    let (tx, rx) = channel::<serde_json::Value>();
    thread::spawn(move || {
        let run = || -> Result<(), String> {
            let mut c = screen_open(&me, &peer, addr)?;
            c.send_val(&serde_json::json!({ "op": "control" }))?;
            let answer = c.recv_val()?;
            if let Some(e) = fs_err(&answer) {
                return Err(e);
            }
            on_status(Ok(()));
            loop {
                match rx.recv_timeout(Duration::from_secs(4)) {
                    Ok(v) => c.send_val(&v)?,
                    Err(RecvTimeoutError::Timeout) => c.send_val(&serde_json::json!({ "op": "ping" }))?,
                    Err(RecvTimeoutError::Disconnected) => {
                        let _ = c.send_val(&serde_json::json!({ "op": "bye" }));
                        return Ok(());
                    }
                }
            }
        };
        if let Err(e) = run() {
            on_status(Err(e));
        }
    });
    tx
}

/// Opens an authenticated screen connection to a paired device (we are the viewer).
fn screen_open(me: &Identity, peer: &PairedPeer, addr: SocketAddr) -> Result<Conn, String> {
    let pk = identity::decode_public(&peer.public_key).ok_or("Stored key is damaged")?;
    let ss = me.secret.diffie_hellman(&pk).as_bytes().to_vec();
    let stream = TcpStream::connect_timeout(&addr, Duration::from_secs(4)).map_err(|_| "Not reachable".to_string())?;
    let mut c = Conn::new(stream, Duration::from_secs(20))?;
    let nonce_c: [u8; 16] = rand::random();
    c.send(&Msg::ScreenHello { id: me.id.clone(), nonce: hex::encode(nonce_c) })?;
    let Msg::ScreenReady { nonce: nonce_s_hex, mac: mac_s } = c.recv()? else { return Err(unexpected()) };
    let nonce_s = hex::decode(&nonce_s_hex).map_err(|_| unexpected())?;
    if !mac_ok(&ss, &[LABEL_SCREEN, &nonce_c, &nonce_s, peer.id.as_bytes(), me.id.as_bytes()], &mac_s) {
        return Err("The device failed the screen-sharing security check".into());
    }
    let mac_c = mac(&ss, &[LABEL_SCREEN, &nonce_s, &nonce_c, me.id.as_bytes(), peer.id.as_bytes()]);
    c.send(&Msg::ScreenAuth { mac: hex::encode(mac_c) })?;
    let (c2s, s2c) = link_keys(&ss, &nonce_c, &nonce_s);
    c.secure(&c2s, &s2c);
    Ok(c)
}

/// One decoded screen frame handed back to the viewer UI.
pub struct Frame {
    pub w: u32,
    pub h: u32,
    pub seq: u64,
    pub jpeg: Vec<u8>,
    /// Which monitor this is (0 = main) and how many the other PC has.
    pub mon: usize,
    pub mons: usize,
}

/// Connects to a paired device and calls `on_frame` for every screen frame until
/// `keep_going` returns false or the connection ends. Runs on its own thread.
pub fn screen_stream<F, G>(
    me: &Identity,
    peer: &PairedPeer,
    addr: SocketAddr,
    monitor: usize,
    fps: u64,
    max_w: u32,
    quality: u8,
    mut on_frame: F,
    keep_going: G,
) -> Result<(), String>
where
    F: FnMut(Frame),
    G: Fn() -> bool,
{
    let mut c = screen_open(me, peer, addr)?;
    c.send_val(&serde_json::json!({
        "op": "start", "fps": fps, "maxw": max_w, "quality": quality, "monitor": monitor,
    }))?;
    while keep_going() {
        let v = c.recv_val()?;
        if let Some(e) = fs_err(&v) {
            return Err(e);
        }
        let (Some(data), Some(w), Some(h), Some(seq)) = (
            v.get("data").and_then(|x| x.as_str()),
            v.get("w").and_then(|x| x.as_u64()),
            v.get("h").and_then(|x| x.as_u64()),
            v.get("seq").and_then(|x| x.as_u64()),
        ) else {
            continue;
        };
        let jpeg =
            base64::engine::general_purpose::STANDARD.decode(data).map_err(|_| "Garbled screen frame".to_string())?;
        let mon = v.get("mon").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
        let mons = v.get("mons").and_then(|x| x.as_u64()).unwrap_or(1).max(1) as usize;
        on_frame(Frame { w: w as u32, h: h as u32, seq, jpeg, mon, mons });
    }
    let _ = c.send_val(&serde_json::json!({ "op": "bye" }));
    Ok(())
}

fn fs_path_arg<'a>(v: &'a serde_json::Value) -> &'a str {
    v.get("path").and_then(|x| x.as_str()).unwrap_or("")
}

fn fs_do_list(c: &mut Conn, root: &Path, v: &serde_json::Value) -> Result<(), String> {
    let dir = fs_resolve(root, fs_path_arg(v))?;
    let mut entries = Vec::new();
    for e in std::fs::read_dir(&dir).map_err(|e| format!("Cannot open that folder: {e}"))?.flatten() {
        let md = match e.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        entries.push(Entry {
            name: e.file_name().to_string_lossy().into_owned(),
            dir: md.is_dir(),
            size: if md.is_dir() { 0 } else { md.len() },
        });
    }
    entries.sort_by(|a, b| (b.dir, a.name.to_lowercase()).cmp(&(a.dir, b.name.to_lowercase())));
    c.send_val(&serde_json::json!({ "entries": entries }))
}

fn fs_do_get(c: &mut Conn, root: &Path, v: &serde_json::Value) -> Result<(), String> {
    let path = fs_resolve(root, fs_path_arg(v))?;
    let mut f = std::fs::File::open(&path).map_err(|e| format!("Cannot open that file: {e}"))?;
    let size = f.metadata().map(|m| m.len()).unwrap_or(0);
    c.send_val(&serde_json::json!({ "size": size }))?;
    let mut buf = vec![0u8; FS_CHUNK];
    loop {
        let n = f.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        c.send_val(&serde_json::json!({ "data": hex::encode(&buf[..n]) }))?;
    }
    c.send_val(&serde_json::json!({ "eof": true }))
}

fn fs_do_put(c: &mut Conn, root: &Path, v: &serde_json::Value) -> Result<(), String> {
    // Uploads always land in a "Nexus Inbox" folder under the shared root, by base name only.
    let name = v.get("name").and_then(|x| x.as_str()).unwrap_or("");
    let base = std::path::Path::new(name).file_name().and_then(|n| n.to_str()).filter(|n| !n.is_empty());
    let base = base.ok_or("Bad file name")?;
    let inbox = root.join("Nexus Inbox");
    std::fs::create_dir_all(&inbox).map_err(|e| format!("Cannot create the inbox: {e}"))?;
    let dest = inbox.join(base);
    let mut f = std::fs::File::create(&dest).map_err(|e| format!("Cannot save the file: {e}"))?;
    c.send_val(&serde_json::json!({ "ok": true }))?;
    loop {
        let frame = c.recv_val()?;
        if frame.get("end").and_then(|x| x.as_bool()) == Some(true) {
            break;
        }
        if let Some(d) = frame.get("data").and_then(|x| x.as_str()) {
            let bytes = hex::decode(d).map_err(|_| "Garbled file data".to_string())?;
            f.write_all(&bytes).map_err(|e| e.to_string())?;
        }
    }
    f.flush().map_err(|e| e.to_string())?;
    c.send_val(&serde_json::json!({ "saved": dest.file_name().unwrap_or_default().to_string_lossy() }))
}

/// Runs a lock/restart/shutdown asked for by a verified, paired device.
fn host_command(
    mut c: Conn,
    me: &Identity,
    shared: &SharedState,
    id: String,
    nonce: String,
    action: String,
    tag: String,
) {
    let outcome = (|| -> Result<(Command, Vec<u8>), String> {
        let peer = lock(shared).paired.iter().find(|p| p.id == id).cloned();
        let (peer, n) = match (peer, hex::decode(&nonce)) {
            (Some(p), Ok(n)) => (p, n),
            _ => return Err(format!("{} does not trust this device", me.name)),
        };
        let pk = identity::decode_public(&peer.public_key).ok_or("Stored key is damaged")?;
        let ss = me.secret.diffie_hellman(&pk);
        if !mac_ok(ss.as_bytes(), &[LABEL_CMD, &n, action.as_bytes(), me.id.as_bytes(), id.as_bytes()], &tag) {
            return Err("Command failed its security check".into());
        }
        let cmd = Command::parse(&action).ok_or("Unknown command")?;
        Ok((cmd, ss.as_bytes().to_vec()))
    })();

    match outcome {
        Ok((cmd, ss)) => {
            let n = hex::decode(&nonce).unwrap_or_default();
            // Reply before acting: restart and shutdown end this process.
            let _ = c.send(&Msg::CommandAck {
                ok: true,
                message: "ok".into(),
                mac: hex::encode(command_ack_mac(&ss, &n, "ok")),
            });
            let _ = c.writer.flush();
            {
                let mut s = lock(shared);
                let who = s
                    .paired
                    .iter()
                    .find(|p| p.id == id)
                    .map(|p| p.name.clone())
                    .unwrap_or_else(|| "A paired device".into());
                s.log(format!("{who} asked to {}", cmd.label().to_lowercase()));
            }
            thread::sleep(Duration::from_millis(150));
            if let Err(e) = run_action(cmd) {
                lock(shared).log(format!("Could not {}: {e}", cmd.label().to_lowercase()));
            }
        }
        Err(e) => {
            let _ = c.send(&Msg::Error { reason: e });
        }
    }
}

/// Carries out a power command on this computer. Spawns and returns at once so
/// the reply has already been sent; restart and shutdown then take this app down with them.
pub(crate) fn run_action(cmd: Command) -> Result<(), String> {
    use std::process::Command as Proc;
    #[cfg(windows)]
    if cmd == Command::Sleep {
        use std::os::windows::process::CommandExt;
        // .NET turns on the shutdown privilege that sleeping needs, and "Suspend" means
        // sleep (rundll32 SetSuspendState would hibernate). CREATE_NO_WINDOW hides the console.
        return Proc::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-WindowStyle",
                "Hidden",
                "-Command",
                "Add-Type -AssemblyName System.Windows.Forms; [System.Windows.Forms.Application]::SetSuspendState('Suspend', $false, $false)",
            ])
            .creation_flags(0x0800_0000)
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string());
    }
    #[cfg(windows)]
    let (program, args): (&str, &[&str]) = match cmd {
        Command::Lock => ("rundll32.exe", &["user32.dll,LockWorkStation"]),
        Command::Sleep => unreachable!("sleep is handled above"),
        Command::Restart => ("shutdown", &["/r", "/t", "0"]),
        Command::Shutdown => ("shutdown", &["/s", "/t", "0"]),
    };
    #[cfg(target_os = "macos")]
    let (program, args): (&str, &[&str]) = match cmd {
        Command::Lock => ("pmset", &["displaysleepnow"]),
        Command::Sleep => ("pmset", &["sleepnow"]),
        Command::Restart => ("osascript", &["-e", "tell application \"System Events\" to restart"]),
        Command::Shutdown => ("osascript", &["-e", "tell application \"System Events\" to shut down"]),
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let (program, args): (&str, &[&str]) = match cmd {
        Command::Lock => ("loginctl", &["lock-session"]),
        Command::Sleep => ("systemctl", &["suspend"]),
        Command::Restart => ("systemctl", &["reboot"]),
        Command::Shutdown => ("systemctl", &["poweroff"]),
    };
    Proc::new(program).args(args).spawn().map(|_| ()).map_err(|e| e.to_string())
}

#[allow(clippy::too_many_arguments)]
fn host_pair(
    mut c: Conn,
    me: &Identity,
    shared: &SharedState,
    dir: &Path,
    v: u8,
    peer_id: String,
    peer_name: String,
    peer_os: String,
    pka_hex: String,
) {
    // Checks that need no user involvement come first.
    let early = (|| -> Result<u64, String> {
        if v != PROTO {
            return Err(format!("{} runs a different Nexus version. Update both devices.", me.name));
        }
        if identity::id_for(&pka_hex)? != peer_id || peer_id == me.id {
            return Err("The device's key does not match its ID".into());
        }
        let mut s = lock(shared);
        if s.open_window().is_none() {
            return Err(format!("{} is not accepting pairing. Click “Allow pairing” on it first.", me.name));
        }
        let sid = s
            .begin_pair(Role::Incoming, &peer_name)
            .ok_or_else(|| format!("{} is busy pairing with another device", me.name))?;
        s.window.as_mut().expect("checked above").attempts_left -= 1;
        s.log(format!("{peer_name} asked to pair"));
        Ok(sid)
    })();
    let sid = match early {
        Ok(sid) => sid,
        Err(e) => {
            let _ = c.send(&Msg::Error { reason: e.clone() });
            lock(shared).log(format!("Turned down a pairing request from {peer_name}: {e}"));
            return;
        }
    };

    let exchange = (|| -> Result<String, String> {
        let pka = unhex(&pka_hex, 32)?;
        let pkb = me.public.as_bytes().to_vec();
        let nb: [u8; 32] = rand::random();
        c.send(&Msg::PairHelloAck {
            v: PROTO,
            id: me.id.clone(),
            name: me.name.clone(),
            os: identity::os_label(),
            public_key: hex::encode(&pkb),
            commit: hex::encode(commitment(&nb, &pkb, &pka)),
        })?;
        let Msg::Nonce { nonce } = c.recv()? else { return Err(unexpected()) };
        let na = unhex(&nonce, 32)?;
        c.send(&Msg::Nonce { nonce: hex::encode(nb) })?;
        Ok(pairing_code(&pka, &pkb, &na, &nb))
    })();

    let result = match exchange {
        Ok(code) => agree(shared, sid, c, code, &peer_name),
        Err(e) => {
            let _ = c.send(&Msg::Error { reason: e.clone() });
            Err(e)
        }
    };

    let mut s = lock(shared);
    match result {
        Ok(addr) => {
            let last_addr = s.seen.get(&peer_id).map(|x| SocketAddr::new(addr, x.tcp_port).to_string());
            save_peer(
                &mut s,
                dir,
                PairedPeer {
                    id: peer_id,
                    name: peer_name.clone(),
                    os: peer_os,
                    public_key: pka_hex,
                    paired_at: store::now_unix(),
                    last_addr,
                    macs: Vec::new(),
                },
            );
            s.window = None;
            if let Some(p) = s.session(sid) {
                p.phase = Phase::Done;
            }
        }
        Err(e) => {
            s.log(format!("Pairing with {peer_name} failed: {e}"));
            if let Some(p) = s.session(sid) {
                p.phase = Phase::Failed(e);
            }
        }
    }
}

/// Shows the code, then waits for both users to confirm. Returns the peer's IP.
fn agree(shared: &SharedState, sid: u64, c: Conn, code: String, peer_name: &str) -> Result<std::net::IpAddr, String> {
    let Conn { reader, mut writer, .. } = c;
    let peer_ip = writer.peer_addr().map_err(|e| e.to_string())?.ip();
    let _ = reader.get_ref().set_read_timeout(Some(DECIDE_TIMEOUT + Duration::from_secs(10)));
    let closer = writer.try_clone().map_err(|e| e.to_string())?;

    // The other side's answer can arrive at any time, so read it on its own thread.
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut r = reader;
        let _ = tx.send(read_msg(&mut r));
    });

    let result = (|| -> Result<std::net::IpAddr, String> {
        if let Some(p) = lock(shared).session(sid) {
            p.phase = Phase::Compare { code: code.clone() };
        }
        let deadline = Instant::now() + DECIDE_TIMEOUT;
        let (mut sent, mut remote) = (false, None::<bool>);
        loop {
            if Instant::now() > deadline {
                let _ = send_msg(&mut writer, &Msg::Error { reason: "Nobody confirmed in time".into() });
                return Err("Nobody confirmed in time".to_string());
            }
            // A closed dialog counts as "cancel".
            let local = lock(shared).session(sid).map_or(Some(false), |p| p.local);
            if let (Some(l), false) = (local, sent) {
                send_msg(&mut writer, &Msg::Decision { accept: l })?;
                sent = true;
                if !l {
                    return Err("You cancelled the pairing".into());
                }
                if remote.is_none() {
                    if let Some(p) = lock(shared).session(sid) {
                        p.phase = Phase::WaitingForOther { code: code.clone() };
                    }
                }
            }
            if remote.is_none() {
                match rx.recv_timeout(Duration::from_millis(100)) {
                    Ok(Ok(Msg::Decision { accept })) => remote = Some(accept),
                    Ok(Ok(_)) => return Err(unexpected()),
                    Ok(Err(e)) => return Err(e),
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => return Err("Connection lost".into()),
                }
            } else {
                thread::sleep(Duration::from_millis(100));
            }
            if remote == Some(false) {
                return Err(format!("{peer_name} said the numbers don't match, or cancelled"));
            }
            if sent && remote == Some(true) {
                return Ok(peer_ip);
            }
        }
    })();
    let _ = closer.shutdown(Shutdown::Both); // also ends the reader thread
    result
}

fn save_peer(s: &mut crate::state::Shared, dir: &Path, peer: PairedPeer) {
    let name = peer.name.clone();
    s.paired.retain(|p| p.id != peer.id);
    s.paired.push(peer);
    if let Err(e) = store::save_peers(dir, &s.paired) {
        s.log(format!("Could not save the paired list: {e}"));
    }
    s.log(format!("Paired with {name}"));
}

/* ------------------------------------------------------------- client */

#[derive(Clone, Debug)]
pub struct PairTarget {
    pub addr: SocketAddr,
    pub name: String,
    /// Set when the device was found by discovery; checked against what answers.
    pub expected_id: Option<String>,
    pub expected_key: Option<String>,
}

/// Turns on "Allow pairing" for 2 minutes.
pub fn open_pairing_window(shared: &SharedState) {
    let mut s = lock(shared);
    s.window = Some(PairWindow { expires: Instant::now() + PAIR_WINDOW, attempts_left: PAIR_ATTEMPTS });
    s.log("Pairing allowed for 2 minutes");
}

/// Pairs in the background. Progress shows up in `shared.pair`.
pub fn pair_with(me: Arc<Identity>, shared: SharedState, dir: PathBuf, target: PairTarget) {
    let Some(sid) = lock(&shared).begin_pair(Role::Outgoing, &target.name) else { return };
    thread::spawn(move || {
        let result = pair_client(&me, &shared, sid, &target);
        let mut s = lock(&shared);
        match result {
            Ok(peer) => {
                save_peer(&mut s, &dir, peer);
                if let Some(p) = s.session(sid) {
                    p.phase = Phase::Done;
                }
            }
            Err(e) => {
                s.log(format!("Pairing with {} failed: {e}", target.name));
                if let Some(p) = s.session(sid) {
                    p.phase = Phase::Failed(e);
                }
            }
        }
    });
}

pub(crate) fn pair_client(
    me: &Identity,
    shared: &SharedState,
    sid: u64,
    target: &PairTarget,
) -> Result<PairedPeer, String> {
    let stream = TcpStream::connect_timeout(&target.addr, Duration::from_secs(4)).map_err(|_| {
        format!("Could not reach {}. Check that Nexus is open there and allowed through the firewall.", target.addr)
    })?;
    let mut c = Conn::new(stream, Duration::from_secs(15))?;
    let pka = me.public.as_bytes().to_vec();
    c.send(&Msg::PairHello {
        v: PROTO,
        id: me.id.clone(),
        name: me.name.clone(),
        os: identity::os_label(),
        public_key: hex::encode(&pka),
    })?;
    let Msg::PairHelloAck { v, id: peer_id, name, os, public_key, commit } = c.recv()? else {
        return Err(unexpected());
    };
    if v != PROTO {
        return Err(format!("{name} runs a different Nexus version. Update both devices."));
    }
    if identity::id_for(&public_key)? != peer_id {
        return Err(format!("{name}'s key does not match its ID"));
    }
    if target.expected_id.as_ref().is_some_and(|e| e != &peer_id) {
        return Err("A different device answered at that address".into());
    }
    if target.expected_key.as_ref().is_some_and(|k| k != &public_key) {
        return Err(format!("{name}'s key differs from the one it announced"));
    }
    if let Some(p) = lock(shared).session(sid) {
        p.peer_name = name.clone();
    }
    let pkb = unhex(&public_key, 32)?;
    let na: [u8; 32] = rand::random();
    c.send(&Msg::Nonce { nonce: hex::encode(na) })?;
    let Msg::Nonce { nonce } = c.recv()? else { return Err(unexpected()) };
    let nb = unhex(&nonce, 32)?;
    if !mac_ok(&nb, &[LABEL_COMMIT, pkb.as_slice(), pka.as_slice()], &commit) {
        let _ = c.send(&Msg::Error { reason: "Commitment check failed".into() });
        return Err(format!("{name} failed a security check. Pairing stopped."));
    }
    let code = pairing_code(&pka, &pkb, &na, &nb);
    agree(shared, sid, c, code, &name)?;
    Ok(PairedPeer {
        id: peer_id,
        name,
        os,
        public_key,
        paired_at: store::now_unix(),
        last_addr: Some(target.addr.to_string()),
        macs: Vec::new(),
    })
}

/// Signed challenge to a paired device. Returns the round-trip time in ms and its live stats, if it sent any.
pub(crate) fn ping(me: &Identity, peer: &PairedPeer, addr: SocketAddr) -> Result<(u128, Option<Stats>), String> {
    let pk = identity::decode_public(&peer.public_key).ok_or("Stored key is damaged")?;
    let start = Instant::now();
    let stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2)).map_err(|_| "Not reachable".to_string())?;
    let mut c = Conn::new(stream, Duration::from_secs(3))?;
    let nonce: [u8; 16] = rand::random();
    c.send(&Msg::Ping { id: me.id.clone(), nonce: hex::encode(nonce) })?;
    let Msg::Pong { mac, stats, stats_mac } = c.recv()? else { return Err(unexpected()) };
    let rtt = start.elapsed().as_millis();
    let ss = me.secret.diffie_hellman(&pk);
    let parts: [&[u8]; 4] = [LABEL_PING, &nonce, peer.id.as_bytes(), me.id.as_bytes()];
    if !mac_ok(ss.as_bytes(), &parts, &mac) {
        return Err("Answered, but failed the identity check".into());
    }
    // Stats are only trusted when their own signature checks out.
    let stats = match (stats, stats_mac) {
        (Some(j), Some(m)) if mac_ok(ss.as_bytes(), &[LABEL_STATS, nonce.as_slice(), j.as_bytes()], &m) => {
            serde_json::from_str::<Stats>(&j).ok()
        }
        _ => None,
    };
    Ok((rtt, stats))
}

/// Asks a paired device to lock, restart or shut down. Returns when it has acknowledged.
pub(crate) fn send_command(me: &Identity, peer: &PairedPeer, addr: SocketAddr, cmd: Command) -> Result<(), String> {
    let pk = identity::decode_public(&peer.public_key).ok_or("Stored key is damaged")?;
    let ss = me.secret.diffie_hellman(&pk);
    let stream = TcpStream::connect_timeout(&addr, Duration::from_secs(3)).map_err(|_| "Not reachable".to_string())?;
    let mut c = Conn::new(stream, Duration::from_secs(5))?;
    let nonce: [u8; 16] = rand::random();
    c.send(&Msg::Command {
        id: me.id.clone(),
        nonce: hex::encode(nonce),
        action: cmd.wire().into(),
        mac: hex::encode(command_mac(ss.as_bytes(), &nonce, cmd.wire(), &peer.id, &me.id)),
    })?;
    match c.recv()? {
        Msg::CommandAck { ok, message, mac: tag } => {
            if !mac_ok(ss.as_bytes(), &[LABEL_CMDACK, nonce.as_slice(), message.as_bytes()], &tag) {
                return Err("The device answered, but the reply failed its security check".into());
            }
            if ok {
                Ok(())
            } else {
                Err(message)
            }
        }
        _ => Err(unexpected()),
    }
}

/// Opens an authenticated file-transfer connection to a paired device.
fn fs_open(me: &Identity, peer: &PairedPeer, addr: SocketAddr) -> Result<Conn, String> {
    let pk = identity::decode_public(&peer.public_key).ok_or("Stored key is damaged")?;
    let ss = me.secret.diffie_hellman(&pk).as_bytes().to_vec();
    let stream = TcpStream::connect_timeout(&addr, Duration::from_secs(4)).map_err(|_| "Not reachable".to_string())?;
    let mut c = Conn::new(stream, Duration::from_secs(30))?;
    let nonce_c: [u8; 16] = rand::random();
    c.send(&Msg::FsHello { id: me.id.clone(), nonce: hex::encode(nonce_c) })?;
    let Msg::FsReady { nonce: nonce_s_hex, mac: mac_s } = c.recv()? else { return Err(unexpected()) };
    let nonce_s = hex::decode(&nonce_s_hex).map_err(|_| unexpected())?;
    if !mac_ok(&ss, &[LABEL_FS, &nonce_c, &nonce_s, peer.id.as_bytes(), me.id.as_bytes()], &mac_s) {
        return Err("The device failed the file-transfer security check".into());
    }
    let mac_c = mac(&ss, &[LABEL_FS, &nonce_s, &nonce_c, me.id.as_bytes(), peer.id.as_bytes()]);
    c.send(&Msg::FsAuth { mac: hex::encode(mac_c) })?;
    let (c2s, s2c) = link_keys(&ss, &nonce_c, &nonce_s);
    c.secure(&c2s, &s2c);
    Ok(c)
}

/// Reads an error reply, if the value is one.
fn fs_err(v: &serde_json::Value) -> Option<String> {
    v.get("error").and_then(|x| x.as_str()).map(|s| s.to_string())
}

/// Lists a folder on a paired device.
pub fn fs_list(me: &Identity, peer: &PairedPeer, addr: SocketAddr, path: &str) -> Result<Vec<Entry>, String> {
    let mut c = fs_open(me, peer, addr)?;
    c.send_val(&serde_json::json!({ "op": "list", "path": path }))?;
    let v = c.recv_val()?;
    if let Some(e) = fs_err(&v) {
        return Err(e);
    }
    let entries = v.get("entries").cloned().ok_or_else(unexpected)?;
    let entries: Vec<Entry> = serde_json::from_value(entries).map_err(|_| unexpected())?;
    let _ = c.send_val(&serde_json::json!({ "op": "bye" }));
    Ok(entries)
}

/// Downloads a file from a paired device to `dest`.
pub fn fs_get(me: &Identity, peer: &PairedPeer, addr: SocketAddr, path: &str, dest: &Path) -> Result<(), String> {
    let mut c = fs_open(me, peer, addr)?;
    c.send_val(&serde_json::json!({ "op": "get", "path": path }))?;
    let head = c.recv_val()?;
    if let Some(e) = fs_err(&head) {
        return Err(e);
    }
    let mut f = std::fs::File::create(dest).map_err(|e| format!("Cannot create {}: {e}", dest.display()))?;
    loop {
        let frame = c.recv_val()?;
        if let Some(e) = fs_err(&frame) {
            return Err(e);
        }
        if frame.get("eof").and_then(|x| x.as_bool()) == Some(true) {
            break;
        }
        if let Some(d) = frame.get("data").and_then(|x| x.as_str()) {
            let bytes = hex::decode(d).map_err(|_| "Garbled file data".to_string())?;
            f.write_all(&bytes).map_err(|e| e.to_string())?;
        }
    }
    f.flush().map_err(|e| e.to_string())?;
    let _ = c.send_val(&serde_json::json!({ "op": "bye" }));
    Ok(())
}

/// Uploads a local file to a paired device's inbox. Returns the saved name.
pub fn fs_put(me: &Identity, peer: &PairedPeer, addr: SocketAddr, src: &Path) -> Result<String, String> {
    let name = src.file_name().and_then(|n| n.to_str()).ok_or("Bad file name")?;
    let mut f = std::fs::File::open(src).map_err(|e| format!("Cannot open {}: {e}", src.display()))?;
    let mut c = fs_open(me, peer, addr)?;
    c.send_val(&serde_json::json!({ "op": "put", "name": name }))?;
    let ack = c.recv_val()?;
    if let Some(e) = fs_err(&ack) {
        return Err(e);
    }
    let mut buf = vec![0u8; FS_CHUNK];
    loop {
        let n = f.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        c.send_val(&serde_json::json!({ "data": hex::encode(&buf[..n]) }))?;
    }
    c.send_val(&serde_json::json!({ "end": true }))?;
    let done = c.recv_val()?;
    if let Some(e) = fs_err(&done) {
        return Err(e);
    }
    Ok(done.get("saved").and_then(|x| x.as_str()).unwrap_or(name).to_string())
}

/// Builds and broadcasts a Wake-on-LAN "magic packet" for each known MAC of a device.
/// The device must have Wake-on-LAN enabled in its BIOS/network card for this to wake it.
pub fn wake(peer: &PairedPeer) -> Result<(), String> {
    let macs: Vec<[u8; 6]> = peer.macs.iter().filter_map(|m| parse_mac(m)).collect();
    if macs.is_empty() {
        return Err("No network address on file for this device yet. Connect it once while it is on.".into());
    }
    let sock = std::net::UdpSocket::bind(("0.0.0.0", 0)).map_err(|e| e.to_string())?;
    sock.set_broadcast(true).map_err(|e| e.to_string())?;
    let mut targets: Vec<SocketAddr> = vec![SocketAddr::from(([255, 255, 255, 255], WOL_PORT))];
    if let Ok(ifaces) = if_addrs::get_if_addrs() {
        for i in ifaces {
            if i.is_loopback() {
                continue;
            }
            if let if_addrs::IfAddr::V4(a) = i.addr {
                let b = std::net::Ipv4Addr::from(u32::from(a.ip) | !u32::from(a.netmask));
                let t = SocketAddr::from((b, WOL_PORT));
                if !targets.contains(&t) {
                    targets.push(t);
                }
            }
        }
    }
    for m in &macs {
        let mut packet = vec![0xFFu8; 6];
        for _ in 0..16 {
            packet.extend_from_slice(m);
        }
        for t in &targets {
            let _ = sock.send_to(&packet, t);
        }
    }
    Ok(())
}

fn parse_mac(s: &str) -> Option<[u8; 6]> {
    let hex: String = s.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    let bytes = hex::decode(hex).ok()?;
    <[u8; 6]>::try_from(bytes.as_slice()).ok().filter(|b| b.iter().any(|&x| x != 0))
}

/// This computer's MAC addresses (colon form), for Wake-on-LAN by paired devices.
pub fn local_macs() -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(iter) = mac_address::MacAddressIterator::new() {
        for m in iter {
            let s = m.to_string();
            if parse_mac(&s).is_some() && !out.contains(&s) {
                out.push(s);
            }
        }
    }
    out
}

/// Checks every paired device every few seconds.
pub fn start_status_checker(me: Arc<Identity>, shared: SharedState, dir: PathBuf) {
    let _ = thread::Builder::new().name("nexus-status".into()).spawn(move || loop {
        let targets: Vec<(PairedPeer, Option<SocketAddr>)> = {
            let s = lock(&shared);
            s.paired
                .iter()
                .map(|p| {
                    let addr = s
                        .seen
                        .get(&p.id)
                        .filter(|x| x.fresh())
                        .map(|x| SocketAddr::new(x.addr, x.tcp_port))
                        .or_else(|| p.last_addr.as_deref().and_then(|a| a.parse().ok()));
                    (p.clone(), addr)
                })
                .collect()
        };
        for (p, addr) in targets {
            let Some(addr) = addr else { continue };
            let result = ping(&me, &p, addr);
            let mut s = lock(&shared);
            let was_ok = matches!(s.checks.get(&p.id), Some(Check::Ok { .. }));
            match result {
                Ok((rtt_ms, stats)) => {
                    if !was_ok {
                        s.log(format!("{} is online and verified ({rtt_ms} ms)", p.name));
                    }
                    if s.waking.remove(&p.id).is_some() {
                        s.log(format!("{} woke up ✓", p.name));
                    }
                    s.wake_failed.remove(&p.id);
                    if let Some(w) = stats.as_ref().and_then(|x| x.wake.clone()) {
                        s.wake_hints.insert(p.id.clone(), w);
                    }
                    // Learn its network card addresses for Wake (in case its announcements don't arrive).
                    let macs = stats.as_ref().map(|x| x.macs.clone()).unwrap_or_default();
                    if !macs.is_empty() {
                        let changed = s.paired.iter_mut().find(|x| x.id == p.id).is_some_and(|x| {
                            let c = x.macs != macs;
                            x.macs = macs;
                            c
                        });
                        if changed {
                            let _ = store::save_peers(&dir, &s.paired);
                        }
                    }
                    s.checks.insert(p.id.clone(), Check::Ok { rtt_ms, at: Instant::now(), stats });
                    let a = addr.to_string();
                    let changed = s.paired.iter_mut().find(|x| x.id == p.id).is_some_and(|x| {
                        let c = x.last_addr.as_deref() != Some(a.as_str());
                        x.last_addr = Some(a);
                        c
                    });
                    if changed {
                        let _ = store::save_peers(&dir, &s.paired);
                    }
                }
                Err(reason) => {
                    if was_ok {
                        s.log(format!("{} stopped answering", p.name));
                    }
                    s.checks.insert(p.id.clone(), Check::Failed { reason, at: Instant::now() });
                }
            }
        }
        thread::sleep(CHECK_INTERVAL);
    });
}

/* -------------------------------------------------------------- tests */

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sealed_lines_match_the_standard_and_reject_changes() {
        let key = [7u8; 32];
        let (mut a, mut b) = (Sealer::new(&key), Sealer::new(&key));
        // ChaCha20-Poly1305 of "hello" with counters 0 and 1 (checked against Python's cryptography).
        let first = a.seal(b"hello").unwrap();
        assert_eq!(hex::encode(&first), "5fe145600f713b52169600ba59cc38ed54e2412b48");
        let mut second = a.seal(b"hello").unwrap();
        assert_eq!(hex::encode(&second), "d2365c0c4a4837e572827d7cdd6e098edd6fe6a85f");
        assert_eq!(b.open(&first).unwrap(), b"hello");
        second[0] ^= 1;
        assert!(b.open(&second).is_err());
    }
    use crate::state::Shared;
    use std::sync::Mutex;
    use x25519_dalek::{PublicKey, StaticSecret};

    /// Same vectors as mobile/test/protocol_test.dart, made by tools/vectors.py.
    #[test]
    fn matches_reference_vectors() {
        let (ska, skb) = (StaticSecret::from([1u8; 32]), StaticSecret::from([2u8; 32]));
        let (pka, pkb) = (PublicKey::from(&ska), PublicKey::from(&skb));
        assert_eq!(hex::encode(pka.as_bytes()), "a4e09292b651c278b9772c569f5fa9bb13d906b46ab68c9df9dc2b4409f8a209");
        assert_eq!(hex::encode(pkb.as_bytes()), "ce8d3ad1ccb633ec7b70c17814a5c76ecd029685050d344745ba05870e587d59");
        let (id_a, id_b) = (
            identity::id_for(&hex::encode(pka.as_bytes())).unwrap(),
            identity::id_for(&hex::encode(pkb.as_bytes())).unwrap(),
        );
        assert_eq!(id_a, "1a92f23852dc908d");
        assert_eq!(id_b, "cfa570c653bd212b");
        let na: Vec<u8> = (0..32).collect();
        let nb: Vec<u8> = (32..64).collect();
        assert_eq!(
            hex::encode(commitment(&nb, pkb.as_bytes(), pka.as_bytes())),
            "9c0b7658fab2384bcc81d1279ce676053ed9a0d14a0b44e2875ded18404c9d9a"
        );
        assert_eq!(pairing_code(pka.as_bytes(), pkb.as_bytes(), &na, &nb), "991548");
        let dh = ska.diffie_hellman(&pkb);
        assert_eq!(hex::encode(dh.as_bytes()), "2ed76ab549b1e73c031eb49c9448f0798aea81b698279a0c3dc3e49fbfc4b953");
        let nonce: Vec<u8> = (0..16).collect();
        assert_eq!(
            hex::encode(ping_mac(dh.as_bytes(), &nonce, &id_b, &id_a)),
            "a0076a3d95367e6269cde514a4ad868319a11736f150dffccee35f1d4f2f0027"
        );
        let stats = br#"{"cpu":12.5,"ram":40.0,"battery":null}"#;
        assert_eq!(
            hex::encode(mac(dh.as_bytes(), &[LABEL_STATS, nonce.as_slice(), stats.as_slice()])),
            "c907b8868f50ad65d246b1ea19b8c3cbb77290a2c185394f93b4901741739935"
        );
        let parsed: Stats = serde_json::from_slice(stats).unwrap();
        assert_eq!(parsed.cpu, Some(12.5));
        assert_eq!(parsed.battery, None);
        assert_eq!(
            hex::encode(command_mac(dh.as_bytes(), &nonce, "restart", &id_b, &id_a)),
            "cc23f5b317576fbab9eaeb6679cccb8505c71920ff2e9f84994ee520f2b3719a"
        );
        assert_eq!(
            hex::encode(command_ack_mac(dh.as_bytes(), &nonce, "ok")),
            "0d7987eada42979766a460a96ea74e2981dedf85e6ecb4af3573d20fdfb63344"
        );
        let nonce_s: Vec<u8> = (16..32).collect();
        assert_eq!(
            hex::encode(mac(dh.as_bytes(), &[LABEL_FS, &nonce, &nonce_s, id_b.as_bytes(), id_a.as_bytes()])),
            "181f683e3bdb4c9808f7692d7a3d86b9f717386ee0871f8eff9dda6d2abbb64c"
        );
        assert_eq!(
            hex::encode(mac(dh.as_bytes(), &[LABEL_FS, &nonce_s, &nonce, id_a.as_bytes(), id_b.as_bytes()])),
            "dafce46c82f3601de254baba89d4351921cf0b2d4d386cb6e1bbec3cf7650f96"
        );
        assert_eq!(
            hex::encode(mac(dh.as_bytes(), &[LABEL_SCREEN, &nonce, &nonce_s, id_b.as_bytes(), id_a.as_bytes()])),
            "af1f2124658e278983f98683cf08c52941038f0445b62a1b25700967de1c8883"
        );
        assert_eq!(
            hex::encode(mac(dh.as_bytes(), &[LABEL_SCREEN, &nonce_s, &nonce, id_a.as_bytes(), id_b.as_bytes()])),
            "7d886b0f703a31a451e390d80e7c010d1fb29db902e6ebbad2951cedd08cc87f"
        );
    }

    #[test]
    fn wake_needs_a_known_mac() {
        let peer = PairedPeer {
            id: "x".into(),
            name: "PC".into(),
            os: "Windows".into(),
            public_key: "00".into(),
            paired_at: 0,
            last_addr: None,
            macs: vec![],
        };
        assert!(wake(&peer).is_err());
        let ok = PairedPeer { macs: vec!["aa:bb:cc:dd:ee:ff".into()], ..peer };
        // A known MAC gets past the "no address" check; sending may or may not reach anything.
        assert!(parse_mac(&ok.macs[0]).is_some());
    }

    struct Node {
        me: Arc<Identity>,
        shared: SharedState,
        addr: SocketAddr,
    }

    fn node(name: &str) -> Node {
        let dir = std::env::temp_dir().join(format!("nexus-test-{name}-{}", rand::random::<u32>()));
        std::fs::create_dir_all(&dir).unwrap();
        let me = Arc::new(Identity::load_or_create(&dir, Some(name.into())).unwrap());
        let shared: SharedState = Arc::new(Mutex::new(Shared::new(vec![])));
        let port = start_server(me.clone(), shared.clone(), dir, 0).unwrap();
        Node { me, shared, addr: SocketAddr::from(([127, 0, 0, 1], port)) }
    }

    /// Plays the user: answers the comparison with `accept`, and reports the code it saw.
    fn auto_answer(shared: SharedState, accept: bool) -> thread::JoinHandle<Option<String>> {
        thread::spawn(move || {
            for _ in 0..200 {
                {
                    let mut s = lock(&shared);
                    if let Some(p) = s.pair.as_mut() {
                        if let Phase::Compare { code } = &p.phase {
                            let code = code.clone();
                            p.local = Some(accept);
                            return Some(code);
                        }
                    }
                }
                thread::sleep(Duration::from_millis(20));
            }
            None
        })
    }

    fn target(n: &Node) -> PairTarget {
        PairTarget {
            addr: n.addr,
            name: n.me.name.clone(),
            expected_id: Some(n.me.id.clone()),
            expected_key: Some(n.me.public_hex()),
        }
    }

    fn wait<F: Fn() -> bool>(f: F) -> bool {
        (0..100).any(|_| {
            thread::sleep(Duration::from_millis(30));
            f()
        })
    }

    #[test]
    fn pairs_when_both_confirm_then_verifies() {
        let (host, guest) = (node("host"), node("guest"));
        open_pairing_window(&host.shared);
        let (h, g) = (auto_answer(host.shared.clone(), true), auto_answer(guest.shared.clone(), true));
        let sid = lock(&guest.shared).begin_pair(Role::Outgoing, "host").unwrap();
        let peer = pair_client(&guest.me, &guest.shared, sid, &target(&host)).expect("pairing works");
        let (hc, gc) = (h.join().unwrap(), g.join().unwrap());
        assert!(hc.is_some() && hc == gc, "both screens showed the same code: {hc:?} {gc:?}");
        assert_eq!(peer.id, host.me.id);
        assert!(wait(|| lock(&host.shared).is_paired(&guest.me.id)), "host saved the guest");
        ping(&guest.me, &peer, host.addr).expect("verified ping");
    }

    #[test]
    fn nothing_is_saved_when_one_side_rejects() {
        let (host, guest) = (node("host2"), node("guest2"));
        open_pairing_window(&host.shared);
        let (_h, _g) = (auto_answer(host.shared.clone(), false), auto_answer(guest.shared.clone(), true));
        let sid = lock(&guest.shared).begin_pair(Role::Outgoing, "host2").unwrap();
        assert!(pair_client(&guest.me, &guest.shared, sid, &target(&host)).is_err());
        thread::sleep(Duration::from_millis(300));
        assert!(!lock(&host.shared).is_paired(&guest.me.id));
    }

    #[test]
    fn refuses_when_pairing_is_not_allowed() {
        let (host, guest) = (node("host3"), node("guest3"));
        let sid = lock(&guest.shared).begin_pair(Role::Outgoing, "host3").unwrap();
        let err = pair_client(&guest.me, &guest.shared, sid, &target(&host)).unwrap_err();
        assert!(err.contains("not accepting"), "{err}");
    }

    #[test]
    fn ping_fails_against_another_device() {
        let (host, guest, other) = (node("host4"), node("guest4"), node("other4"));
        open_pairing_window(&host.shared);
        let (_h, _g) = (auto_answer(host.shared.clone(), true), auto_answer(guest.shared.clone(), true));
        let sid = lock(&guest.shared).begin_pair(Role::Outgoing, "host4").unwrap();
        let peer = pair_client(&guest.me, &guest.shared, sid, &target(&host)).unwrap();
        assert!(ping(&guest.me, &peer, other.addr).is_err());
    }
}
