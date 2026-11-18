//! Automation: simple "when this happens, do that" rules that this PC runs on its own.
//!
//! A rule has a trigger (a time of day on chosen weekdays, the PC being idle for a
//! while, or Nexus starting) and an action (lock / sleep / restart / shut down this
//! PC or another paired computer, wake another computer, open something, or end an
//! app). Rules are kept in `automations.json` in the data folder. The window edits
//! them directly; a paired phone or PC can read and change them over an
//! authenticated screen link (`{op:"auto"}`) while "Allow remote control" is on.

use crate::{
    identity::Identity,
    net::{self, Command},
    state::{lock, SharedState},
    store::PairedPeer,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

pub const MAX_RULES: usize = 50;
pub const DAY_NAMES: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

/// When a rule runs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum When {
    /// At hour:minute (local time) on the given weekdays (0 = Monday … 6 = Sunday).
    /// No days means every day.
    Time {
        hour: u8,
        minute: u8,
        #[serde(default)]
        days: Vec<u8>,
    },
    /// When nobody has used the mouse or keyboard for this many minutes.
    Idle { minutes: u32 },
    /// When Nexus starts (shortly after the PC is turned on, if Nexus starts with Windows).
    Startup,
}

/// What a rule does.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Act {
    Lock,
    Sleep,
    Restart,
    Shutdown,
    /// Wake another computer with Wake-on-LAN (needs a target).
    Wake,
    /// Open a program, file, folder or web address on this PC.
    Open {
        what: String,
    },
    /// End every process of an app on this PC, e.g. "chrome.exe".
    EndApp {
        name: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Rule {
    #[serde(default)]
    pub id: u64,
    #[serde(default)]
    pub name: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    pub when: When,
    pub action: Act,
    /// The paired device a power action is for. None means this PC.
    #[serde(default)]
    pub target: Option<String>,
}

fn yes() -> bool {
    true
}

impl Act {
    pub fn power(&self) -> Option<Command> {
        match self {
            Act::Lock => Some(Command::Lock),
            Act::Sleep => Some(Command::Sleep),
            Act::Restart => Some(Command::Restart),
            Act::Shutdown => Some(Command::Shutdown),
            _ => None,
        }
    }

    /// Whether this action can be sent to another device (power actions and Wake).
    pub fn can_target(&self) -> bool {
        self.power().is_some() || *self == Act::Wake
    }
}

impl When {
    pub fn describe(&self) -> String {
        match self {
            When::Time { hour, minute, days } => {
                let d = days_text(days);
                format!("{d} at {hour:02}:{minute:02}")
            }
            When::Idle { minutes } => format!("After {minutes} min with nobody using this PC"),
            When::Startup => "When Nexus starts".into(),
        }
    }
}

pub fn days_text(days: &[u8]) -> String {
    let mut d: Vec<u8> = days.iter().copied().filter(|d| *d < 7).collect();
    d.sort_unstable();
    d.dedup();
    match d.as_slice() {
        [] | [0, 1, 2, 3, 4, 5, 6] => "Every day".into(),
        [0, 1, 2, 3, 4] => "Weekdays".into(),
        [5, 6] => "Weekends".into(),
        _ => d.iter().map(|i| DAY_NAMES[*i as usize]).collect::<Vec<_>>().join(", "),
    }
}

impl Rule {
    /// "Sleep this PC", "Shut down Gaming PC", "Open https://…".
    pub fn describe_action(&self, peers: &[PairedPeer]) -> String {
        let target = match &self.target {
            None => "this PC".to_string(),
            Some(id) => {
                peers.iter().find(|p| &p.id == id).map(|p| p.name.clone()).unwrap_or_else(|| "a removed device".into())
            }
        };
        match &self.action {
            Act::Lock => format!("Lock {target}"),
            Act::Sleep => format!("Put {target} to sleep"),
            Act::Restart => format!("Restart {target}"),
            Act::Shutdown => format!("Shut down {target}"),
            Act::Wake => format!("Wake {target}"),
            Act::Open { what } => format!("Open {what}"),
            Act::EndApp { name } => format!("End {name}"),
        }
    }

    /// The name shown in lists: the user's name, or a description.
    pub fn title(&self, peers: &[PairedPeer]) -> String {
        if self.name.trim().is_empty() {
            self.describe_action(peers)
        } else {
            self.name.trim().to_string()
        }
    }
}

/// Checks one rule and tidies it. Returns a plain-language problem if it can't be used.
pub fn check(rule: &mut Rule, peers: &[PairedPeer]) -> Result<(), String> {
    rule.name = rule.name.chars().take(60).collect::<String>().trim().to_string();
    match &mut rule.when {
        When::Time { hour, minute, days } => {
            if *hour > 23 || *minute > 59 {
                return Err("The time must be between 00:00 and 23:59".into());
            }
            days.retain(|d| *d < 7);
            days.sort_unstable();
            days.dedup();
        }
        When::Idle { minutes } => {
            if *minutes == 0 || *minutes > 24 * 60 {
                return Err("Idle time must be between 1 and 1440 minutes".into());
            }
        }
        When::Startup => {}
    }
    match &mut rule.action {
        Act::Open { what } => {
            *what = what.trim().to_string();
            if what.is_empty() {
                return Err("Write what to open (a program, file, folder or web address)".into());
            }
        }
        Act::EndApp { name } => {
            *name = name.trim().to_string();
            if name.is_empty() {
                return Err("Write the name of the app to end, for example chrome.exe".into());
            }
            if crate::monitor::is_protected(name) {
                return Err(format!("{name} keeps Windows running and can't be ended"));
            }
        }
        _ => {}
    }
    if !rule.action.can_target() {
        rule.target = None;
    }
    if rule.action == Act::Wake && rule.target.is_none() {
        return Err("Choose which computer to wake".into());
    }
    if let Some(t) = &rule.target {
        let Some(p) = peers.iter().find(|p| &p.id == t) else {
            return Err("That device is no longer paired".into());
        };
        if matches!(p.os.as_str(), "Android" | "iOS") {
            return Err("Phones can't be locked, put to sleep or turned off from Nexus".into());
        }
    }
    // A rule that turns this PC off or restarts it every time Nexus starts would never let you in.
    if rule.when == When::Startup
        && rule.target.is_none()
        && matches!(rule.action, Act::Restart | Act::Shutdown | Act::Sleep)
    {
        return Err(
            "This PC can't restart, shut down or sleep when Nexus starts — it would keep turning itself off".into()
        );
    }
    Ok(())
}

/// Checks a whole list (as sent by the window or a paired device) and gives
/// every rule a unique id.
pub fn check_all(mut rules: Vec<Rule>, peers: &[PairedPeer]) -> Result<Vec<Rule>, String> {
    if rules.len() > MAX_RULES {
        return Err(format!("At most {MAX_RULES} automations"));
    }
    let mut next = rules.iter().map(|r| r.id).max().unwrap_or(0) + 1;
    let mut used = std::collections::HashSet::new();
    for (i, r) in rules.iter_mut().enumerate() {
        if r.id == 0 || !used.insert(r.id) {
            r.id = next;
            used.insert(next);
            next += 1;
        }
        check(r, peers).map_err(|e| format!("Automation {}: {e}", i + 1))?;
    }
    Ok(rules)
}

fn file(dir: &Path) -> PathBuf {
    dir.join("automations.json")
}

pub fn load(dir: &Path) -> Vec<Rule> {
    fs::read_to_string(file(dir)).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

pub fn save(dir: &Path, rules: &[Rule]) -> Result<(), String> {
    let text = serde_json::to_string_pretty(rules).map_err(|e| e.to_string())?;
    fs::write(file(dir), text).map_err(|e| format!("Could not save automations: {e}"))
}

/// Checks, stores and activates a new list of rules.
pub fn replace(shared: &SharedState, dir: &Path, rules: Vec<Rule>) -> Result<Vec<Rule>, String> {
    let peers = lock(shared).paired.clone();
    let rules = check_all(rules, &peers)?;
    save(dir, &rules)?;
    lock(shared).automations = rules.clone();
    Ok(rules)
}

/* ---------------------------------------------------------------- the clock */

/// Local wall-clock time: (weekday 0 = Monday, hour, minute, a key unique to this minute).
#[cfg(windows)]
fn local_now() -> (u8, u8, u8, String) {
    use windows_sys::Win32::{Foundation::SYSTEMTIME, System::SystemInformation::GetLocalTime};
    let mut t: SYSTEMTIME = unsafe { std::mem::zeroed() };
    unsafe { GetLocalTime(&mut t) };
    // Windows counts Sunday as 0.
    let weekday = ((t.wDayOfWeek + 6) % 7) as u8;
    let key = format!("{}-{}-{} {}:{}", t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute);
    (weekday, t.wHour as u8, t.wMinute as u8, key)
}

/// Other systems: UTC is close enough for development builds.
#[cfg(not(windows))]
fn local_now() -> (u8, u8, u8, String) {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let days = secs / 86_400;
    // 1 Jan 1970 was a Thursday (3 when Monday is 0).
    let weekday = ((days + 3) % 7) as u8;
    let hour = (secs / 3600 % 24) as u8;
    let minute = (secs / 60 % 60) as u8;
    (weekday, hour, minute, format!("{days} {hour}:{minute}"))
}

/// How long since the mouse or keyboard was last used on this PC.
#[cfg(windows)]
pub fn idle_time() -> Duration {
    use windows_sys::Win32::{
        System::SystemInformation::GetTickCount,
        UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO},
    };
    let mut info = LASTINPUTINFO { cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32, dwTime: 0 };
    if unsafe { GetLastInputInfo(&mut info) } == 0 {
        return Duration::ZERO;
    }
    let now = unsafe { GetTickCount() };
    Duration::from_millis(now.wrapping_sub(info.dwTime) as u64)
}

#[cfg(not(windows))]
pub fn idle_time() -> Duration {
    Duration::ZERO
}

/// Whether a time rule is due at this weekday/hour/minute.
pub fn time_matches(when: &When, weekday: u8, hour: u8, minute: u8) -> bool {
    match when {
        When::Time { hour: h, minute: m, days } => {
            *h == hour && *m == minute && (days.is_empty() || days.contains(&weekday))
        }
        _ => false,
    }
}

/* ------------------------------------------------------------- the runner */

/// Starts the background thread that runs the rules.
pub fn start(me: Arc<Identity>, shared: SharedState, dir: PathBuf) {
    lock(&shared).automations = load(&dir);
    thread::spawn(move || {
        let started = Instant::now();
        let mut startup_done = false;
        let mut fired_minute: HashMap<u64, String> = HashMap::new();
        let mut idle_fired: HashMap<u64, bool> = HashMap::new();
        loop {
            thread::sleep(Duration::from_secs(5));
            let rules: Vec<Rule> = lock(&shared).automations.iter().filter(|r| r.enabled).cloned().collect();
            let (weekday, hour, minute, key) = local_now();
            let idle = idle_time();
            // Give the network a moment to find other devices before running startup rules.
            let run_startup = !startup_done && started.elapsed() > Duration::from_secs(20);
            for r in &rules {
                let due = match &r.when {
                    When::Time { .. } => {
                        time_matches(&r.when, weekday, hour, minute) && fired_minute.get(&r.id) != Some(&key)
                    }
                    When::Idle { minutes } => {
                        let over = idle >= Duration::from_secs(*minutes as u64 * 60);
                        let was = idle_fired.get(&r.id).copied().unwrap_or(false);
                        idle_fired.insert(r.id, over);
                        over && !was
                    }
                    When::Startup => run_startup,
                };
                if due {
                    fired_minute.insert(r.id, key.clone());
                    run_rule(&me, &shared, r, "");
                }
            }
            if run_startup {
                startup_done = true;
            }
        }
    });
}

fn addr_of(shared: &SharedState, id: &str) -> Option<SocketAddr> {
    let s = lock(shared);
    s.seen.get(id).filter(|x| x.fresh()).map(|x| SocketAddr::new(x.addr, x.tcp_port)).or_else(|| {
        s.paired.iter().find(|p| p.id == id).and_then(|p| p.last_addr.as_deref()).and_then(|a| a.parse().ok())
    })
}

/// Runs one rule now on its own thread and writes the outcome to Activity.
/// `why` is added to the log line, e.g. " (Run now)".
pub fn run_rule(me: &Arc<Identity>, shared: &SharedState, rule: &Rule, why: &str) {
    let me = me.clone();
    let shared = shared.clone();
    let rule = rule.clone();
    let why = why.to_string();
    thread::spawn(move || {
        let peers = lock(&shared).paired.clone();
        let title = rule.title(&peers);
        let what = rule.describe_action(&peers);
        lock(&shared).log(format!("Automation \"{title}\"{why}: {what}"));
        let result = execute(&me, &shared, &rule, &peers);
        if let Err(e) = result {
            lock(&shared).log(format!("Automation \"{title}\" failed: {e}"));
        }
    });
}

fn execute(me: &Identity, shared: &SharedState, rule: &Rule, peers: &[PairedPeer]) -> Result<(), String> {
    let target = match &rule.target {
        Some(id) => Some(peers.iter().find(|p| &p.id == id).cloned().ok_or("That device is no longer paired")?),
        None => None,
    };
    match (&rule.action, target) {
        (Act::Wake, Some(p)) => net::wake(&p),
        (Act::Wake, None) => Err("No computer chosen to wake".into()),
        (a, Some(p)) => {
            let cmd = a.power().ok_or("That action only works on this PC")?;
            let addr = addr_of(shared, &p.id).ok_or_else(|| format!("{} is not reachable right now", p.name))?;
            net::send_command(me, &p, addr, cmd)
        }
        (Act::Open { what }, None) => open(what),
        (Act::EndApp { name }, None) => crate::monitor::Monitor::new().end_app(name).map(|_| ()),
        (a, None) => {
            let cmd = a.power().ok_or("Unknown action")?;
            net::run_action(cmd)
        }
    }
}

/// Opens a program, file, folder or web address the way double-clicking it would.
#[cfg(windows)]
fn open(what: &str) -> Result<(), String> {
    use windows_sys::Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL};
    let wide = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let verb = wide("open");
    let file = wide(what);
    let r = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    // ShellExecute returns a value above 32 when it worked.
    if r as isize > 32 {
        Ok(())
    } else {
        Err(format!("Windows could not open \"{what}\""))
    }
}

#[cfg(not(windows))]
fn open(what: &str) -> Result<(), String> {
    let program = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    std::process::Command::new(program).arg(what).spawn().map(|_| ()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(when: When, action: Act) -> Rule {
        Rule { id: 0, name: String::new(), enabled: true, when, action, target: None }
    }

    #[test]
    fn time_rules_match_their_days() {
        let w = When::Time { hour: 23, minute: 0, days: vec![0, 1, 2, 3, 4] };
        assert!(time_matches(&w, 2, 23, 0));
        assert!(!time_matches(&w, 5, 23, 0));
        assert!(!time_matches(&w, 2, 23, 1));
        let every = When::Time { hour: 7, minute: 30, days: vec![] };
        assert!(time_matches(&every, 6, 7, 30));
    }

    #[test]
    fn startup_cannot_turn_this_pc_off() {
        let mut r = rule(When::Startup, Act::Shutdown);
        assert!(check(&mut r, &[]).is_err());
        let mut ok = rule(When::Startup, Act::Lock);
        assert!(check(&mut ok, &[]).is_ok());
    }

    #[test]
    fn wake_needs_a_target_and_ids_are_unique() {
        let mut r = rule(When::Idle { minutes: 5 }, Act::Wake);
        assert!(check(&mut r, &[]).is_err());
        let list = check_all(vec![rule(When::Startup, Act::Lock), rule(When::Startup, Act::Lock)], &[]).unwrap();
        assert_ne!(list[0].id, list[1].id);
    }

    #[test]
    fn rules_round_trip_as_json() {
        let r =
            rule(When::Time { hour: 8, minute: 5, days: vec![5, 6] }, Act::Open { what: "https://example.com".into() });
        let text = serde_json::to_string(&r).unwrap();
        let back: Rule = serde_json::from_str(&text).unwrap();
        assert_eq!(r, back);
        assert_eq!(days_text(&[5, 6]), "Weekends");
    }
}
