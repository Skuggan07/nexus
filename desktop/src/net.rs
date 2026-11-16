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

fn unhex(s: &str, len: usize) -> Result<Vec<u8>, String> {
    hex::decode(s).ok().filter(|b| b.len() == len).ok_or_else(unexpected)
}

fn unexpected() -> String {
    "The other device sent something unexpected".into()
}

/* --------------------------------------------------------------- wire */

/// Newline-delimited JSON over TCP.
struct Conn {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
}

impl Conn {
    fn new(s: TcpStream, timeout: Duration) -> Result<Self, String> {
        s.set_read_timeout(Some(timeout)).map_err(|e| e.to_string())?;
        s.set_write_timeout(Some(Duration::from_secs(10))).map_err(|e| e.to_string())?;
        let _ = s.set_nodelay(true);
        let writer = s.try_clone().map_err(|e| e.to_string())?;
        Ok(Self { reader: BufReader::new(s), writer })
    }
    fn send(&mut self, m: &Msg) -> Result<(), String> {
        send_msg(&mut self.writer, m)
    }
    fn recv(&mut self) -> Result<Msg, String> {
        read_msg(&mut self.reader)
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
    let n = r.take(64 * 1024).read_line(&mut line).map_err(|_| "No answer from the other device".to_string())?;
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

fn handle(stream: TcpStream, me: &Identity, shared: &SharedState, dir: &Path) {
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
        Ok(Msg::PairHello { v, id, name, os, public_key }) => {
            host_pair(c, me, shared, dir, v, id, name, os, public_key)
        }
        _ => {}
    }
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
    let Conn { reader, mut writer } = c;
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
