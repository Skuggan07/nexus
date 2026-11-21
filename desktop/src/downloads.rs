//! Download on the PC: another device sends a link, this PC downloads the file into
//! its Downloads folder, and the other device watches the progress.

use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    io::{Read, Write},
    path::PathBuf,
    sync::{Mutex, OnceLock},
    time::Duration,
};

/// One download on this PC.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Job {
    pub id: u64,
    pub name: String,
    pub url: String,
    /// Bytes so far, and the full size (0 = unknown).
    pub done: u64,
    pub total: u64,
    /// "downloading", "done", "failed" or "cancelled".
    pub state: String,
    #[serde(default)]
    pub error: String,
    /// Who asked for it.
    #[serde(default)]
    pub by: String,
}

struct Jobs {
    list: Vec<Job>,
    next: u64,
    cancelled: HashSet<u64>,
}

fn jobs() -> &'static Mutex<Jobs> {
    static J: OnceLock<Mutex<Jobs>> = OnceLock::new();
    J.get_or_init(|| Mutex::new(Jobs { list: Vec::new(), next: 1, cancelled: HashSet::new() }))
}

fn with<R>(f: impl FnOnce(&mut Jobs) -> R) -> R {
    let mut g = jobs().lock().unwrap_or_else(|e| e.into_inner());
    f(&mut g)
}

fn update(id: u64, f: impl FnOnce(&mut Job)) {
    with(|j| {
        if let Some(x) = j.list.iter_mut().find(|x| x.id == id) {
            f(x);
        }
    });
}

/// This PC's Downloads folder.
pub fn folder() -> PathBuf {
    directories::UserDirs::new()
        .and_then(|u| u.download_dir().map(|d| d.to_path_buf()))
        .or_else(|| directories::BaseDirs::new().map(|b| b.home_dir().join("Downloads")))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// The newest downloads first (at most 20).
pub fn list() -> Vec<Job> {
    with(|j| j.list.iter().rev().take(20).cloned().collect())
}

pub fn cancel(id: u64) {
    with(|j| {
        j.cancelled.insert(id);
    });
}

/// Undoes %20 and friends in a file name taken from a link.
fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = |c: u8| (c as char).to_digit(16);
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(if b[i] == b'+' { b' ' } else { b[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A safe file name: from the server's "filename=", else the end of the link.
fn file_name(url: &str, disposition: Option<&str>) -> String {
    let from_header = disposition.and_then(|d| {
        d.split(';').map(str::trim).find_map(|p| {
            p.strip_prefix("filename*=UTF-8''")
                .map(unescape)
                .or_else(|| p.strip_prefix("filename=").map(|v| v.trim_matches('"').to_string()))
        })
    });
    let from_url = url.split(|c| c == '?' || c == '#').next().unwrap_or("").rsplit('/').next().map(unescape);
    let raw = from_header.filter(|n| !n.is_empty()).or(from_url.filter(|n| !n.is_empty())).unwrap_or_default();
    let clean: String = raw
        .chars()
        .map(|c| if "\\/:*?\"<>|".contains(c) || c.is_control() { '_' } else { c })
        .collect::<String>()
        .trim()
        .trim_end_matches('.')
        .chars()
        .take(150)
        .collect();
    if clean.is_empty() {
        "download".into()
    } else {
        clean
    }
}

/// "name.zip", or "name (2).zip" when that already exists.
fn free_path(dir: &std::path::Path, name: &str) -> PathBuf {
    let p = dir.join(name);
    if !p.exists() && !dir.join(format!("{name}.part")).exists() {
        return p;
    }
    let (base, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    for n in 2..1000 {
        let p = dir.join(format!("{base} ({n}){ext}"));
        if !p.exists() && !dir.join(format!("{base} ({n}){ext}.part")).exists() {
            return p;
        }
    }
    dir.join(format!("{base} ({}){ext}", crate::store::now_unix()))
}

/// Starts downloading `url` into the Downloads folder. Returns the new job.
pub fn start(url: &str, by: &str, shared: crate::state::SharedState) -> Result<Job, String> {
    let url = url.trim().to_string();
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err("That is not a web link (it should start with http:// or https://)".into());
    }
    let job = with(|j| {
        let job = Job {
            id: j.next,
            name: file_name(&url, None),
            url: url.clone(),
            state: "downloading".into(),
            by: by.to_string(),
            ..Default::default()
        };
        j.next += 1;
        j.list.push(job.clone());
        if j.list.len() > 50 {
            j.list.remove(0);
        }
        job
    });
    let id = job.id;
    std::thread::spawn(move || {
        let result = run(id, &url);
        let name = with(|j| j.list.iter().find(|x| x.id == id).map(|x| x.name.clone())).unwrap_or_default();
        let msg = match &result {
            Ok(()) => format!("Downloaded {name} into Downloads"),
            Err(e) if e == "cancelled" => format!("Download of {name} cancelled"),
            Err(e) => format!("Download of {name} failed: {e}"),
        };
        update(id, |x| match result {
            Ok(()) => x.state = "done".into(),
            Err(e) if e == "cancelled" => x.state = "cancelled".into(),
            Err(e) => {
                x.state = "failed".into();
                x.error = e;
            }
        });
        crate::state::lock(&shared).log(msg);
    });
    Ok(job)
}

fn run(id: u64, url: &str) -> Result<(), String> {
    let agent =
        ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(15)).timeout_read(Duration::from_secs(60)).build();
    let resp = agent.get(url).set("User-Agent", "Mozilla/5.0 (Nexus)").call().map_err(|e| match e {
        ureq::Error::Status(code, _) => format!("the website answered {code}"),
        e => format!("could not reach the website ({e})"),
    })?;
    let total = resp.header("Content-Length").and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
    let name = file_name(resp.get_url(), resp.header("Content-Disposition"));
    let dir = folder();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let dest = free_path(&dir, &name);
    let shown = dest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or(name);
    update(id, |x| {
        x.name = shown;
        x.total = total;
    });
    let part = PathBuf::from(format!("{}.part", dest.display()));
    let mut file = std::fs::File::create(&part).map_err(|e| format!("cannot save in Downloads: {e}"))?;
    let mut reader = resp.into_reader();
    let mut buf = vec![0u8; 256 * 1024];
    let mut done = 0u64;
    loop {
        if with(|j| j.cancelled.contains(&id)) {
            drop(file);
            let _ = std::fs::remove_file(&part);
            return Err("cancelled".into());
        }
        let n = match reader.read(&mut buf) {
            Ok(n) => n,
            Err(e) => {
                drop(file);
                let _ = std::fs::remove_file(&part);
                return Err(format!("the download broke off ({e})"));
            }
        };
        if n == 0 {
            break;
        }
        if let Err(e) = file.write_all(&buf[..n]) {
            drop(file);
            let _ = std::fs::remove_file(&part);
            return Err(format!("cannot save (is the disk full?): {e}"));
        }
        done += n as u64;
        update(id, |x| x.done = done);
    }
    drop(file);
    if total > 0 && done < total {
        let _ = std::fs::remove_file(&part);
        return Err("the download was incomplete".into());
    }
    std::fs::rename(&part, &dest).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::file_name;

    #[test]
    fn names_from_links() {
        assert_eq!(file_name("https://x.com/files/My%20Game.zip?token=1", None), "My Game.zip");
        assert_eq!(file_name("https://x.com/get", Some("attachment; filename=\"setup.exe\"")), "setup.exe");
        assert_eq!(file_name("https://x.com/", None), "download");
        assert_eq!(file_name("https://x.com/a:b.txt", None), "a_b.txt");
    }
}
