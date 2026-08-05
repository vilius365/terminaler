//! Multibox tmux session discovery.
//!
//! A single background thread periodically runs `tmux list-sessions` on every
//! configured box (over ssh / wsl.exe / custom argv) and caches the results in
//! a global snapshot that the GUI (launcher picker, WebView sidebar) reads
//! without ever blocking on the network.

use config::tmux::TmuxBox;
use parking_lot::{Condvar, Mutex};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoxStatus {
    /// No probe has completed yet.
    Pending,
    Ok,
    /// Last probe failed; the message is the first stderr line.
    Unreachable(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmuxSessionEntry {
    pub session: String,
    pub windows: u32,
    pub attached: bool,
}

#[derive(Debug, Clone)]
pub struct BoxSnapshot {
    pub box_name: String,
    pub status: BoxStatus,
    /// Last successfully discovered sessions; kept while the box is
    /// unreachable so the UI can show them as stale.
    pub sessions: Vec<TmuxSessionEntry>,
    pub last_success: Option<Instant>,
    /// True when `last_success` is older than 3 poll intervals.
    pub stale: bool,
    updating: bool,
}

static STATE: OnceLock<Mutex<Vec<BoxSnapshot>>> = OnceLock::new();
static WAKE: OnceLock<(Mutex<bool>, Condvar)> = OnceLock::new();
static STARTED: AtomicBool = AtomicBool::new(false);

fn state() -> &'static Mutex<Vec<BoxSnapshot>> {
    STATE.get_or_init(|| Mutex::new(Vec::new()))
}

fn wake() -> &'static (Mutex<bool>, Condvar) {
    WAKE.get_or_init(|| (Mutex::new(false), Condvar::new()))
}

/// Cheap clone of the current discovery state. Safe on any thread.
pub fn snapshot() -> Vec<BoxSnapshot> {
    state().lock().clone()
}

/// Wake the poller for an immediate refresh (e.g. picker opened,
/// sidebar refresh button).
pub fn request_refresh() {
    let (lock, condvar) = wake();
    *lock.lock() = true;
    condvar.notify_one();
}

/// Start the background poller once. Subsequent calls are no-ops.
/// Reads the live configuration on every cycle, so config reloads
/// (boxes added/removed, feature disabled) are honored without restart.
pub fn ensure_running() {
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::Builder::new()
        .name("tmux-discovery".to_string())
        .spawn(poller_loop)
        .expect("failed to spawn tmux-discovery thread");
}

fn poller_loop() {
    loop {
        let config = config::configuration();
        let tmux = config.tmux.clone().unwrap_or_default();

        let poll_interval = Duration::from_secs(tmux.poll_interval_seconds.max(5));

        if !tmux.enabled || tmux.boxes.is_empty() || config.tmux.is_none() {
            state().lock().clear();
        } else {
            sync_boxes(&tmux.boxes);
            for tmux_box in tmux.boxes.iter().filter(|b| b.enabled) {
                spawn_probe(tmux_box.clone(), tmux.probe_timeout_seconds, poll_interval);
            }
        }

        // Sleep until the next cycle or an explicit refresh request.
        let (lock, condvar) = wake();
        let mut requested = lock.lock();
        if !*requested {
            condvar.wait_for(&mut requested, poll_interval);
        }
        *requested = false;
    }
}

/// Reconcile the cached snapshot list with the configured boxes,
/// preserving existing entries and dropping removed ones.
fn sync_boxes(boxes: &[TmuxBox]) {
    let mut snapshots = state().lock();
    snapshots.retain(|s| boxes.iter().any(|b| b.enabled && b.name == s.box_name));
    for tmux_box in boxes.iter().filter(|b| b.enabled) {
        if !snapshots.iter().any(|s| s.box_name == tmux_box.name) {
            snapshots.push(BoxSnapshot {
                box_name: tmux_box.name.clone(),
                status: BoxStatus::Pending,
                sessions: vec![],
                last_success: None,
                stale: false,
                updating: false,
            });
        }
    }
}

/// Probe one box on its own short-lived thread so a slow/dead box never
/// delays the others. Skips if a probe for this box is still in flight.
fn spawn_probe(tmux_box: TmuxBox, timeout_secs: u64, poll_interval: Duration) {
    {
        let mut snapshots = state().lock();
        match snapshots.iter_mut().find(|s| s.box_name == tmux_box.name) {
            Some(snap) if !snap.updating => snap.updating = true,
            _ => return,
        }
    }
    std::thread::spawn(move || {
        let result = run_probe(&tmux_box, timeout_secs);
        let mut snapshots = state().lock();
        let Some(snap) = snapshots.iter_mut().find(|s| s.box_name == tmux_box.name) else {
            return;
        };
        snap.updating = false;
        match result {
            Ok(sessions) => {
                snap.sessions = sessions;
                snap.status = BoxStatus::Ok;
                snap.last_success = Some(Instant::now());
                snap.stale = false;
            }
            Err(err) => {
                log::debug!("tmux discovery: box {} unreachable: {}", tmux_box.name, err);
                snap.status = BoxStatus::Unreachable(err);
                snap.stale = snap
                    .last_success
                    .map_or(false, |t| t.elapsed() > 3 * poll_interval);
            }
        }
    });
}

/// Run `tmux list-sessions` on the box. Returns Err only when the box (or
/// transport) is unreachable; a box with no tmux server running is Ok(vec![]).
fn run_probe(tmux_box: &TmuxBox, timeout_secs: u64) -> Result<Vec<TmuxSessionEntry>, String> {
    let argv = tmux_box.list_sessions_argv(timeout_secs.min(5).max(2));
    let mut cmd = std::process::Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(winapi::um::winbase::CREATE_NO_WINDOW);
    }

    let child = cmd
        .spawn()
        .map_err(|e| format!("failed to run {}: {}", argv[0], e))?;
    let output = wait_with_timeout(child, Duration::from_secs(timeout_secs.max(2)))?;

    let stderr = String::from_utf8_lossy(&output.stderr);
    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(parse_list_sessions(&stdout))
    } else if is_no_server(&stderr) {
        // tmux is installed but no server is running: the normal empty state.
        Ok(vec![])
    } else {
        Err(stderr
            .lines()
            .next()
            .unwrap_or("probe failed with no stderr")
            .to_string())
    }
}

/// `Child::wait` with a deadline: ssh's ConnectTimeout covers connection
/// stalls, but a wedged remote command would hang forever without this.
fn wait_with_timeout(
    mut child: std::process::Child,
    timeout: Duration,
) -> Result<std::process::Output, String> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                return child
                    .wait_with_output()
                    .map_err(|e| format!("collecting output: {}", e));
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("probe timed out after {:?}", timeout));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(format!("waiting for probe: {}", e)),
        }
    }
}

fn is_no_server(stderr: &str) -> bool {
    stderr.contains("no server running") || stderr.contains("error connecting to")
}

/// Parse `list-sessions` output in `TmuxBox::LIST_SESSIONS_FORMAT` order:
/// windows, attached, created, then the session name (which may contain
/// spaces, hence name-last and `splitn`).
fn parse_list_sessions(stdout: &str) -> Vec<TmuxSessionEntry> {
    stdout
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(4, ' ');
            let windows: u32 = parts.next()?.parse().ok()?;
            let attached: u32 = parts.next()?.parse().ok()?;
            let _created = parts.next()?;
            let session = parts.next()?.trim();
            if session.is_empty() {
                return None;
            }
            Some(TmuxSessionEntry {
                session: session.to_string(),
                windows,
                attached: attached > 0,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_list_sessions_lines() {
        let out = "1 0 1722860000 dark-factory\n3 1 1722860001 invade team toolkit\n";
        let sessions = parse_list_sessions(out);
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].session, "dark-factory");
        assert_eq!(sessions[0].windows, 1);
        assert!(!sessions[0].attached);
        assert_eq!(sessions[1].session, "invade team toolkit");
        assert_eq!(sessions[1].windows, 3);
        assert!(sessions[1].attached);
    }

    #[test]
    fn skips_malformed_lines() {
        let out = "garbage\n2 1 1722860000 ok\n";
        let sessions = parse_list_sessions(out);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session, "ok");
    }

    #[test]
    fn classifies_no_server_as_empty() {
        assert!(is_no_server("no server running on /tmp/tmux-1000/default"));
        assert!(is_no_server(
            "error connecting to /tmp/tmux-1000/default (No such file or directory)"
        ));
        assert!(!is_no_server("ssh: connect to host devbox port 22: Connection refused"));
    }
}
