//! LAN discovery: every 2 seconds each device broadcasts a small UDP
//! announcement, and listens for everyone else's. Announcements carry no
//! secrets; trust comes only from pairing.

use crate::{
    identity::{self, Identity},
    state::{lock, Seen, SharedState},
};
use serde::{Deserialize, Serialize};
use socket2::{Domain, Protocol, Socket, Type};
use std::{
    net::{Ipv4Addr, SocketAddr, UdpSocket},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

pub const PORT: u16 = 47800;
const INTERVAL: Duration = Duration::from_secs(2);

#[derive(Serialize, Deserialize)]
struct Announce {
    app: String,
    v: u8,
    id: String,
    name: String,
    os: String,
    public_key: String,
    tcp_port: u16,
}

pub fn start(me: Arc<Identity>, shared: SharedState, tcp_port: u16) -> Result<(), String> {
    let sock = bind().map_err(|e| format!("Discovery could not use UDP port {PORT}: {e}"))?;
    let tx = sock.try_clone().map_err(|e| e.to_string())?;

    let announce = serde_json::to_vec(&Announce {
        app: "nexus".into(),
        v: 1,
        id: me.id.clone(),
        name: me.name.clone(),
        os: identity::os_label(),
        public_key: me.public_hex(),
        tcp_port,
    })
    .expect("announcement serializes");

    thread::Builder::new()
        .name("nexus-announce".into())
        .spawn(move || loop {
            for target in broadcast_targets() {
                let _ = tx.send_to(&announce, target);
            }
            thread::sleep(INTERVAL);
        })
        .map_err(|e| e.to_string())?;

    thread::Builder::new()
        .name("nexus-listen".into())
        .spawn(move || {
            let mut buf = [0u8; 2048];
            loop {
                let (n, from) = match sock.recv_from(&mut buf) {
                    Ok(r) => r,
                    // Windows reports ICMP "port unreachable" as a receive error; ignore it.
                    Err(_) => {
                        thread::sleep(Duration::from_millis(50));
                        continue;
                    }
                };
                let Ok(a) = serde_json::from_slice::<Announce>(&buf[..n]) else { continue };
                if a.app != "nexus" || a.id == me.id {
                    continue;
                }
                // The ID must belong to the announced key, or the packet is ignored.
                if identity::id_for(&a.public_key).ok().as_deref() != Some(a.id.as_str()) {
                    continue;
                }
                let mut s = lock(&shared);
                let returning = s.seen.get(&a.id).map_or(true, |x| !x.fresh());
                if returning {
                    let what = if s.is_paired(&a.id) { "is back on the network" } else { "found nearby" };
                    s.log(format!("{} {what} ({})", a.name, from.ip()));
                }
                s.seen.insert(
                    a.id.clone(),
                    Seen {
                        id: a.id,
                        name: a.name,
                        os: a.os,
                        public_key: a.public_key,
                        addr: from.ip(),
                        tcp_port: a.tcp_port,
                        last_seen: Instant::now(),
                    },
                );
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn bind() -> std::io::Result<UdpSocket> {
    let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    s.set_reuse_address(true)?; // lets a second copy on the same PC share the port
    s.set_broadcast(true)?;
    s.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, PORT)).into())?;
    Ok(s.into())
}

/// 255.255.255.255 plus the directed broadcast address of every IPv4
/// interface, because Windows sends the global broadcast out of only one adapter.
fn broadcast_targets() -> Vec<SocketAddr> {
    let mut v = vec![SocketAddr::from((Ipv4Addr::BROADCAST, PORT))];
    if let Ok(ifaces) = if_addrs::get_if_addrs() {
        for i in ifaces {
            if i.is_loopback() {
                continue;
            }
            if let if_addrs::IfAddr::V4(a) = i.addr {
                let b = Ipv4Addr::from(u32::from(a.ip) | !u32::from(a.netmask));
                let t = SocketAddr::from((b, PORT));
                if !v.contains(&t) {
                    v.push(t);
                }
            }
        }
    }
    v
}
