//! Shared clipboard: text copied on this PC is sent to the paired devices that are
//! online, and text they send is put on this PC's clipboard. Each transfer uses its
//! own authenticated screen link (`{op:"clip", text}`), so only paired devices can
//! read or change the clipboard. It can be turned off in Settings.
//!
//! Files and folders copied in File Explorer are sent to the other paired devices too
//! (`{op:"clipfiles"}`). On a PC they land in a folder inside Nexus's data folder and are
//! put on that PC's clipboard as files, so Ctrl+V in Explorer pastes them like normal.
//! A phone saves them (pictures to the Gallery, other files to Downloads) and puts
//! them on its clipboard. Photos and files sent from a phone arrive the same way.

use crate::{
    identity::Identity,
    net,
    state::{lock, SharedState, Status},
};
use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// Longest text that is shared (about a million characters).
pub const MAX_TEXT: usize = 1_000_000;
/// Most that is sent when files are copied: 4 GB and 10,000 files and folders.
pub const MAX_FILES_BYTES: u64 = 4 * 1024 * 1024 * 1024;
pub const MAX_FILES_COUNT: usize = 10_000;

/// Watches this PC's clipboard and sends new text or files to the paired devices that are online.
pub fn start(me: Arc<Identity>, shared: SharedState, dir: PathBuf) {
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
            if let Some(paths) = sys::get_files() {
                let key = files_key(&paths);
                let stage = stage_root(&dir);
                {
                    let mut s = lock(&shared);
                    // Files that just arrived from another PC are not sent back.
                    if s.clip_last == key || paths.iter().all(|p| p.starts_with(&stage)) {
                        s.clip_last = key;
                        continue;
                    }
                    s.clip_last = key;
                }
                let (me, shared) = (me.clone(), shared.clone());
                thread::spawn(move || send_files_to_all(&me, &shared, &paths));
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

/* ------------------------------------------------------------------ files */

fn files_key(paths: &[PathBuf]) -> String {
    let list: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
    format!("files:{}", list.join("|"))
}

/// Where files from other PCs are kept until they are pasted.
fn stage_root(dir: &Path) -> PathBuf {
    dir.join("clipboard")
}

/// A fresh folder for files that are arriving. Files from earlier copies are removed.
pub fn new_stage(dir: &Path) -> Result<PathBuf, String> {
    let root = stage_root(dir);
    if let Ok(entries) = std::fs::read_dir(&root) {
        for e in entries.flatten() {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    let p = root.join(stamp.to_string());
    std::fs::create_dir_all(&p).map_err(|e| format!("Cannot make a folder for the files: {e}"))?;
    Ok(p)
}

/// Accepts a relative path from another device only if it stays inside the folder.
pub fn safe_rel(rel: &str) -> Option<String> {
    let parts: Vec<&str> = rel.split(['/', '\\']).filter(|p| !p.is_empty() && *p != ".").collect();
    if parts.is_empty() || parts.iter().any(|p| *p == ".." || p.contains(':')) {
        return None;
    }
    Some(parts.join("/"))
}

/// Lists everything to send: the copied files, and everything inside copied folders.
fn collect_items(paths: &[PathBuf]) -> Result<Vec<net::ClipItem>, String> {
    let mut items = Vec::new();
    let mut total = 0u64;
    let mut tops: Vec<String> = Vec::new();
    for p in paths {
        let Some(name) = p.file_name().map(|n| n.to_string_lossy().to_string()) else { continue };
        if tops.contains(&name) {
            continue;
        }
        tops.push(name.clone());
        walk(p, &name, &mut items, &mut total)?;
    }
    if items.is_empty() {
        return Err("there was nothing to send".into());
    }
    Ok(items)
}

fn walk(path: &Path, rel: &str, items: &mut Vec<net::ClipItem>, total: &mut u64) -> Result<(), String> {
    let too_big = || "too much for the clipboard (over 4 GB or 10,000 files) — use Browse files instead".to_string();
    let meta = std::fs::metadata(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    if meta.is_dir() {
        items.push(net::ClipItem { rel: rel.to_string(), abs: path.to_path_buf(), size: 0, dir: true });
        if items.len() > MAX_FILES_COUNT {
            return Err(too_big());
        }
        let entries = std::fs::read_dir(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            walk(&e.path(), &format!("{rel}/{name}"), items, total)?;
        }
    } else {
        *total += meta.len();
        items.push(net::ClipItem { rel: rel.to_string(), abs: path.to_path_buf(), size: meta.len(), dir: false });
        if *total > MAX_FILES_BYTES || items.len() > MAX_FILES_COUNT {
            return Err(too_big());
        }
    }
    Ok(())
}

/// Sends copied files to the other devices that are online: PCs paste them with Ctrl+V,
/// phones save them to the Gallery or Downloads.
fn send_files_to_all(me: &Identity, shared: &SharedState, paths: &[PathBuf]) {
    let targets = online_targets(shared);
    if targets.is_empty() {
        return;
    }
    let items = match collect_items(paths) {
        Ok(i) => i,
        Err(e) => {
            lock(shared).log(format!("Copied files were not shared: {e}"));
            return;
        }
    };
    let count = items.iter().filter(|i| !i.dir).count();
    lock(shared).log(format!("Sending {count} copied file{}…", if count == 1 { "" } else { "s" }));
    let (mut pcs, mut phones) = (Vec::new(), Vec::new());
    for (peer, addr) in targets {
        let phone = matches!(peer.os.as_str(), "Android" | "iOS");
        match net::clipfiles_send(me, &peer, addr, &items) {
            Ok(()) if phone => phones.push(peer.name.clone()),
            Ok(()) => pcs.push(peer.name.clone()),
            Err(e) => lock(shared).log(format!("Could not send the copied files to {}: {e}", peer.name)),
        }
    }
    if !pcs.is_empty() {
        lock(shared).log(format!("Copied files sent to {} — paste them there with Ctrl+V", pcs.join(", ")));
    }
    if !phones.is_empty() {
        lock(shared).log(format!(
            "Copied files sent to {} — pictures are in the Gallery, other files in Downloads › Nexus",
            phones.join(", ")
        ));
    }
}

/// Puts files that arrived from another PC on this PC's clipboard.
pub fn receive_files(shared: &SharedState, paths: &[PathBuf]) -> Result<(), String> {
    lock(shared).clip_last = files_key(paths);
    sys::set_files(paths)
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
    use std::{os::windows::ffi::OsStrExt, path::PathBuf};
    use windows_sys::Win32::{
        Foundation::GlobalFree,
        System::{
            DataExchange::{
                CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardSequenceNumber,
                IsClipboardFormatAvailable, OpenClipboard, SetClipboardData,
            },
            Memory::{GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE},
        },
        UI::Shell::DragQueryFileW,
    };

    const CF_UNICODETEXT: u32 = 13;
    const CF_HDROP: u32 = 15;

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

    /// The files copied in File Explorer, if that's what is on the clipboard.
    pub fn get_files() -> Option<Vec<PathBuf>> {
        unsafe {
            if IsClipboardFormatAvailable(CF_HDROP) == 0 || !open() {
                return None;
            }
            let mut out = Vec::new();
            let h = GetClipboardData(CF_HDROP);
            if !h.is_null() {
                let count = DragQueryFileW(h as _, u32::MAX, std::ptr::null_mut(), 0);
                for i in 0..count {
                    let len = DragQueryFileW(h as _, i, std::ptr::null_mut(), 0);
                    let mut buf = vec![0u16; len as usize + 1];
                    let got = DragQueryFileW(h as _, i, buf.as_mut_ptr(), buf.len() as u32) as usize;
                    if got > 0 {
                        out.push(PathBuf::from(String::from_utf16_lossy(&buf[..got.min(buf.len())])));
                    }
                }
            }
            CloseClipboard();
            if out.is_empty() {
                None
            } else {
                Some(out)
            }
        }
    }

    /// Replaces the clipboard with one block of data in the given format.
    fn put(format: u32, data: &[u8]) -> Result<(), String> {
        unsafe {
            if !open() {
                return Err("The clipboard is busy, try again".into());
            }
            EmptyClipboard();
            let h = GlobalAlloc(GMEM_MOVEABLE, data.len());
            if h.is_null() {
                CloseClipboard();
                return Err("Not enough memory for the clipboard".into());
            }
            let p = GlobalLock(h) as *mut u8;
            if p.is_null() {
                GlobalFree(h);
                CloseClipboard();
                return Err("Not enough memory for the clipboard".into());
            }
            std::ptr::copy_nonoverlapping(data.as_ptr(), p, data.len());
            GlobalUnlock(h);
            // After a successful SetClipboardData, Windows owns the memory.
            let ok = !SetClipboardData(format, h as _).is_null();
            if !ok {
                GlobalFree(h);
            }
            CloseClipboard();
            if ok {
                Ok(())
            } else {
                Err("Windows did not accept the clipboard data".into())
            }
        }
    }

    pub fn set_text(text: &str) -> Result<(), String> {
        let bytes: Vec<u8> = text.encode_utf16().chain(std::iter::once(0)).flat_map(|u| u.to_le_bytes()).collect();
        put(CF_UNICODETEXT, &bytes)
    }

    /// Puts files on the clipboard the way File Explorer's Copy does (a DROPFILES list).
    pub fn set_files(paths: &[PathBuf]) -> Result<(), String> {
        // DROPFILES header: offset of the list (20), drop point x and y, fNC, fWide = 1.
        let mut bytes: Vec<u8> = [20u32, 0, 0, 0, 1].iter().flat_map(|v| v.to_le_bytes()).collect();
        for p in paths {
            for u in p.as_os_str().encode_wide().chain(std::iter::once(0)) {
                bytes.extend_from_slice(&u.to_le_bytes());
            }
        }
        bytes.extend_from_slice(&[0, 0]);
        put(CF_HDROP, &bytes)
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
    pub fn get_files() -> Option<Vec<std::path::PathBuf>> {
        None
    }
    pub fn set_files(_paths: &[std::path::PathBuf]) -> Result<(), String> {
        Err("Copying files only works on Windows for now".into())
    }
}
