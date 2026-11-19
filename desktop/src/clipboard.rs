//! Shared clipboard: text copied on this PC is sent to the paired devices that are
//! online, and text they send is put on this PC's clipboard. Each transfer uses its
//! own authenticated screen link (`{op:"clip", text}`), so only paired devices can
//! read or change the clipboard. It can be turned off in Settings.

use crate::{
    identity::Identity,
    net,
    state::{lock, SharedState, Status},
};
use std::{net::SocketAddr, sync::Arc, thread, time::Duration};

/// Longest text that is shared (about a million characters).
pub const MAX_TEXT: usize = 1_000_000;

/// Watches this PC's clipboard and sends new text to the paired devices that are online.
pub fn start(me: Arc<Identity>, shared: SharedState) {
    thread::spawn(move || {
        let mut seq = sys::sequence();
        loop {
            thread::sleep(Duration::from_millis(700));
            let now = sys::sequence();
            if now == seq {
                continue;
            }
            seq = now;
            if !lock(&shared).share_clipboard {
                continue;
            }
            let Some(text) = sys::get_text() else { continue };
            if text.trim().is_empty() || text.len() > MAX_TEXT {
                continue;
            }
            {
                let mut s = lock(&shared);
                // Text that just arrived from another device is not sent back.
                if s.clip_last == text {
                    continue;
                }
                s.clip_last = text.clone();
            }
            send_to_all(&me, &shared, &text);
        }
    });
}

/// Paired devices that are online right now, with an address to reach them.
fn online_targets(shared: &SharedState) -> Vec<(crate::store::PairedPeer, SocketAddr)> {
    let s = lock(shared);
    s.paired
        .iter()
        .filter(|p| matches!(s.status(p), Status::Verified { .. } | Status::Checking))
        .filter_map(|p| {
            let addr = s
                .seen
                .get(&p.id)
                .filter(|x| x.fresh())
                .map(|x| SocketAddr::new(x.addr, x.tcp_port))
                .or_else(|| p.last_addr.as_deref().and_then(|a| a.parse().ok()))?;
            Some((p.clone(), addr))
        })
        .collect()
}

fn send_to_all(me: &Identity, shared: &SharedState, text: &str) {
    let mut sent = Vec::new();
    for (peer, addr) in online_targets(shared) {
        match net::clip_send(me, &peer, addr, text) {
            Ok(()) => sent.push(peer.name.clone()),
            Err(e) => lock(shared).log(format!("Could not send the clipboard to {}: {e}", peer.name)),
        }
    }
    if !sent.is_empty() {
        lock(shared).log(format!("Copied text sent to {}", sent.join(", ")));
    }
}

/// Puts text from another device on this PC's clipboard.
pub fn receive(shared: &SharedState, text: &str) -> Result<(), String> {
    if text.is_empty() || text.len() > MAX_TEXT {
        return Err("Nothing to paste".into());
    }
    // Remember it first so the watcher doesn't send it straight back.
    lock(shared).clip_last = text.to_string();
    sys::set_text(text)
}

#[cfg(windows)]
mod sys {
    use windows_sys::Win32::{
        Foundation::GlobalFree,
        System::{
            DataExchange::{
                CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardSequenceNumber,
                IsClipboardFormatAvailable, OpenClipboard, SetClipboardData,
            },
            Memory::{GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE},
        },
    };

    const CF_UNICODETEXT: u32 = 13;

    /// Changes every time anything is copied.
    pub fn sequence() -> u32 {
        unsafe { GetClipboardSequenceNumber() }
    }

    /// Opens the clipboard, retrying briefly because other apps hold it for a moment.
    fn open() -> bool {
        for _ in 0..10 {
            if unsafe { OpenClipboard(std::ptr::null_mut()) } != 0 {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        false
    }

    pub fn get_text() -> Option<String> {
        unsafe {
            if IsClipboardFormatAvailable(CF_UNICODETEXT) == 0 || !open() {
                return None;
            }
            let mut out = None;
            let h = GetClipboardData(CF_UNICODETEXT);
            if !h.is_null() {
                let p = GlobalLock(h as _) as *const u16;
                if !p.is_null() {
                    let max = GlobalSize(h as _) / 2;
                    let mut n = 0;
                    while n < max && *p.add(n) != 0 {
                        n += 1;
                    }
                    out = Some(String::from_utf16_lossy(std::slice::from_raw_parts(p, n)));
                    GlobalUnlock(h as _);
                }
            }
            CloseClipboard();
            out
        }
    }

    pub fn set_text(text: &str) -> Result<(), String> {
        let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        unsafe {
            if !open() {
                return Err("The clipboard is busy, try again".into());
            }
            EmptyClipboard();
            let h = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2);
            if h.is_null() {
                CloseClipboard();
                return Err("Not enough memory for the text".into());
            }
            let p = GlobalLock(h) as *mut u16;
            if p.is_null() {
                GlobalFree(h);
                CloseClipboard();
                return Err("Not enough memory for the text".into());
            }
            std::ptr::copy_nonoverlapping(wide.as_ptr(), p, wide.len());
            GlobalUnlock(h);
            // After a successful SetClipboardData, Windows owns the memory.
            let ok = !SetClipboardData(CF_UNICODETEXT, h as _).is_null();
            if !ok {
                GlobalFree(h);
            }
            CloseClipboard();
            if ok {
                Ok(())
            } else {
                Err("Windows did not accept the text".into())
            }
        }
    }
}

#[cfg(not(windows))]
mod sys {
    pub fn sequence() -> u32 {
        0
    }
    pub fn get_text() -> Option<String> {
        None
    }
    pub fn set_text(_text: &str) -> Result<(), String> {
        Err("The shared clipboard only works on Windows for now".into())
    }
}
