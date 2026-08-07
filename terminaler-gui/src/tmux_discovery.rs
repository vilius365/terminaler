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
use std::sync::{Arc, OnceLock};
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
    /// Label for the agent running in this session: the claude-agent-
    /// interconnect instance name when one is registered for this
    /// box+session (e.g. "witch"), otherwise the generic agent type from the
    /// pane commands (e.g. "claude"). None when no pane runs a known agent,
    /// or when the probes failed — the session list never depends on it.
    pub agent: Option<String>,
    /// True when `agent` is an interconnect instance name rather than a
    /// generic agent type. The sidebar marks these with a ⇄ prefix, matching
    /// the statusline convention.
    pub agent_is_instance: bool,
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
            // Fetched once per cycle and shared by every box probe: the
            // registry is global, and this keeps it to one local request
            // per poll however many boxes are configured.
            let instances = std::sync::Arc::new(fetch_interconnect_instances(
                &tmux.interconnect_url,
                Duration::from_secs(2),
            ));
            for tmux_box in tmux.boxes.iter().filter(|b| b.enabled) {
                spawn_probe(
                    tmux_box.clone(),
                    tmux.probe_timeout_seconds,
                    poll_interval,
                    Arc::clone(&instances),
                );
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
fn spawn_probe(
    tmux_box: TmuxBox,
    timeout_secs: u64,
    poll_interval: Duration,
    instances: std::sync::Arc<Vec<InterconnectInstance>>,
) {
    {
        let mut snapshots = state().lock();
        match snapshots.iter_mut().find(|s| s.box_name == tmux_box.name) {
            Some(snap) if !snap.updating => snap.updating = true,
            _ => return,
        }
    }
    std::thread::spawn(move || {
        let result = run_probe(&tmux_box, timeout_secs, &instances);
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

/// Run `tmux list-sessions` on the box, then annotate the result with the
/// agent running in each session. Returns Err only when the box (or
/// transport) is unreachable; a box with no tmux server running is Ok(vec![]).
fn run_probe(
    tmux_box: &TmuxBox,
    timeout_secs: u64,
    instances: &[InterconnectInstance],
) -> Result<Vec<TmuxSessionEntry>, String> {
    let argv = tmux_box.list_sessions_argv(timeout_secs.min(5).max(2));
    let mut sessions = match run_tmux(&argv, timeout_secs)? {
        Some(stdout) => parse_list_sessions(&stdout),
        // tmux is installed but no server is running: the normal empty state.
        None => return Ok(vec![]),
    };

    if !sessions.is_empty() {
        // Best effort: a failing pane scan leaves the agent column blank
        // rather than downgrading an otherwise reachable box.
        let argv = tmux_box.list_panes_argv(timeout_secs.min(5).max(2));
        match run_tmux(&argv, timeout_secs) {
            Ok(Some(stdout)) => {
                let agents = parse_session_agents(&stdout);
                for session in &mut sessions {
                    session.agent = agents.get(&session.session).cloned();
                }
            }
            Ok(None) => {}
            Err(err) => {
                log::debug!(
                    "tmux discovery: agent scan on box {} failed: {}",
                    tmux_box.name,
                    err
                );
            }
        }

        // Prefer the interconnect instance name ("witch") over the generic
        // agent type ("claude") wherever one is registered for this session.
        // Also best effort: an unreachable daemon just leaves the type.
        if !instances.is_empty() {
            let by_session = instances_for_machine(instances, tmux_box.interconnect_machine_name());
            for session in &mut sessions {
                if let Some(name) = by_session.get(&session.session) {
                    session.agent = Some(name.clone());
                    session.agent_is_instance = true;
                }
            }
        }
    }

    Ok(sessions)
}

/// Decode process output that may be UTF-8 or UTF-16LE.
///
/// `wsl.exe` writes its own diagnostics ("There is no distribution with the
/// supplied name", "Windows Subsystem for Linux has no installed
/// distributions") as UTF-16LE, which `from_utf8_lossy` turns into NUL-
/// separated mush — that is how a real error became the useless "probe
/// failed with no stderr". Output from the Linux side (tmux itself) is plain
/// UTF-8 and must pass through untouched.
fn decode_output(bytes: &[u8]) -> String {
    // A UTF-16LE BOM is decisive.
    if bytes.len() >= 2 && bytes[0] == 0xFF && bytes[1] == 0xFE {
        return decode_utf16le(&bytes[2..]);
    }
    // No BOM: infer from interleaved NULs, which ASCII-range UTF-16LE text
    // is full of and valid UTF-8 never contains.
    let sample = &bytes[..bytes.len().min(64)];
    let nuls = sample.iter().filter(|b| **b == 0).count();
    if !sample.is_empty() && nuls * 3 >= sample.len() {
        return decode_utf16le(bytes);
    }
    String::from_utf8_lossy(bytes).into_owned()
}

fn decode_utf16le(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    String::from_utf16_lossy(&units)
}

/// First non-empty line of some process output, trimmed of the NULs and
/// control characters that survive a mixed-encoding decode.
fn first_meaningful_line(text: &str) -> Option<String> {
    text.lines()
        .map(|line| line.trim_matches(|c: char| c.is_whitespace() || c == '\u{0}'))
        .find(|line| !line.is_empty())
        .map(|line| line.to_string())
}

/// Run one read-only tmux probe. `Ok(None)` means the box answered but no
/// tmux server is running; `Err` means the box/transport is unreachable.
fn run_tmux(argv: &[String], timeout_secs: u64) -> Result<Option<String>, String> {
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

    let stderr = decode_output(&output.stderr);
    let stdout = decode_output(&output.stdout);
    if output.status.success() {
        Ok(Some(stdout))
    } else if is_no_server(&stderr) || is_no_server(&stdout) {
        // wsl.exe -e relays the Linux command's stderr, but some transports
        // fold it into stdout; check both before treating this as an error.
        Ok(None)
    } else {
        // Report whatever the transport actually said. wsl.exe writes its
        // own failures to stdout, so stderr-only reporting loses them; the
        // exit code is the last resort when both streams are silent.
        let detail = first_meaningful_line(&stderr)
            .or_else(|| first_meaningful_line(&stdout))
            .unwrap_or_else(|| match output.status.code() {
                Some(code) => format!("{} exited with code {}", argv[0], code),
                None => format!("{} terminated by signal", argv[0]),
            });
        Err(detail)
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
                agent: None,
                agent_is_instance: false,
            })
        })
        .collect()
}

/// One registered claude-agent-interconnect instance, as far as session
/// labelling cares. Extra fields in the daemon's JSON are ignored.
#[derive(Debug, Clone, serde::Deserialize)]
struct InterconnectInstance {
    instance_id: String,
    #[serde(default)]
    machine: Option<String>,
    #[serde(default)]
    tmux_session: Option<String>,
    #[serde(default)]
    status: Option<String>,
}

/// Fetch registered instances from the interconnect daemon.
///
/// Deliberately short-timeout and failure-tolerant: the daemon is often not
/// running (it is a separate opt-in tool), so every failure path returns an
/// empty map and the caller silently falls back to the agent type.
fn fetch_interconnect_instances(base_url: &str, timeout: Duration) -> Vec<InterconnectInstance> {
    if base_url.trim().is_empty() {
        return vec![];
    }
    use std::convert::TryFrom;
    let url = format!("{}/instances", base_url.trim_end_matches('/'));
    let uri = match http_req::uri::Uri::try_from(url.as_str()) {
        Ok(uri) => uri,
        Err(err) => {
            log::debug!("tmux discovery: bad interconnect_url {}: {}", url, err);
            return vec![];
        }
    };

    let mut body = Vec::new();
    match http_req::request::Request::new(&uri)
        .timeout(timeout)
        .connect_timeout(Some(timeout))
        .read_timeout(Some(timeout))
        .header("User-Agent", "terminaler-tmux-discovery")
        .send(&mut body)
    {
        Ok(res) if res.status_code().is_success() => {
            serde_json::from_slice::<Vec<InterconnectInstance>>(&body).unwrap_or_else(|err| {
                log::debug!("tmux discovery: interconnect JSON unreadable: {}", err);
                vec![]
            })
        }
        Ok(res) => {
            log::debug!("tmux discovery: interconnect returned {}", res.status_code());
            vec![]
        }
        Err(err) => {
            // The common case: daemon not running. Debug, not warn.
            log::debug!("tmux discovery: interconnect unreachable: {}", err);
            vec![]
        }
    }
}

/// Index instances registered on `machine` by their tmux session name.
/// Instances explicitly marked dead are skipped; anything else (including a
/// missing status) counts, so an older daemon without the field still works.
fn instances_for_machine(
    instances: &[InterconnectInstance],
    machine: &str,
) -> std::collections::HashMap<String, String> {
    let mut by_session = std::collections::HashMap::new();
    for inst in instances {
        if inst.machine.as_deref() != Some(machine) {
            continue;
        }
        if matches!(inst.status.as_deref(), Some("dead") | Some("inactive")) {
            continue;
        }
        let Some(session) = inst.tmux_session.as_deref() else {
            continue;
        };
        if session.is_empty() || inst.instance_id.is_empty() {
            continue;
        }
        by_session
            .entry(session.to_string())
            .or_insert_with(|| inst.instance_id.clone());
    }
    by_session
}

/// Foreground pane commands that identify an agent, mapped to the label the
/// UI shows. Matched case-insensitively against `pane_current_command`, with
/// any path and a trailing `.exe` stripped first.
const AGENT_COMMANDS: &[(&str, &str)] = &[
    ("claude", "claude"),
    ("codex", "codex"),
    ("aider", "aider"),
    ("gemini", "gemini"),
    ("opencode", "opencode"),
    ("cursor-agent", "cursor"),
];

/// Parse `list-panes -a` output in `TmuxBox::LIST_PANES_FORMAT` order:
/// command, pid, then the session name (name-last, hence `splitn`).
/// Returns the agent label per session; the first agent pane found in a
/// session wins, and sessions running no agent are absent from the map.
fn parse_session_agents(stdout: &str) -> std::collections::HashMap<String, String> {
    let mut agents = std::collections::HashMap::new();
    for line in stdout.lines() {
        let mut parts = line.splitn(3, ' ');
        let Some(command) = parts.next() else {
            continue;
        };
        let Some(_pid) = parts.next() else { continue };
        let Some(session) = parts.next().map(str::trim) else {
            continue;
        };
        if session.is_empty() {
            continue;
        }
        if let Some(label) = agent_label(command) {
            agents
                .entry(session.to_string())
                .or_insert_with(|| label.to_string());
        }
    }
    agents
}

/// Map a `pane_current_command` value to an agent label, if it is one.
fn agent_label(command: &str) -> Option<&'static str> {
    let base = command
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(command)
        .trim_end_matches(".exe")
        .to_ascii_lowercase();
    AGENT_COMMANDS
        .iter()
        .find(|(cmd, _)| *cmd == base)
        .map(|(_, label)| *label)
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
        assert_eq!(sessions[0].agent, None);
    }

    #[test]
    fn parses_agents_from_pane_list() {
        // Session names may contain spaces; only agent panes are recorded.
        let out = "zsh 101 dark-factory\n\
                   claude 102 dark-factory\n\
                   vim 103 build\n\
                   codex 104 invade team toolkit\n";
        let agents = parse_session_agents(out);
        assert_eq!(agents.get("dark-factory").map(String::as_str), Some("claude"));
        assert_eq!(
            agents.get("invade team toolkit").map(String::as_str),
            Some("codex")
        );
        assert_eq!(agents.get("build"), None);
    }

    fn inst(id: &str, machine: &str, session: &str, status: &str) -> InterconnectInstance {
        InterconnectInstance {
            instance_id: id.to_string(),
            machine: Some(machine.to_string()),
            tmux_session: Some(session.to_string()),
            status: Some(status.to_string()),
        }
    }

    #[test]
    fn instances_are_scoped_to_their_machine() {
        // Same session name on two boxes must not cross-label: `%0` on wsl
        // and on devbox are different instances.
        let all = vec![
            inst("witch", "devbox", "main", "active"),
            inst("lunar", "home", "main", "active"),
        ];
        assert_eq!(
            instances_for_machine(&all, "devbox").get("main").map(String::as_str),
            Some("witch")
        );
        assert_eq!(
            instances_for_machine(&all, "home").get("main").map(String::as_str),
            Some("lunar")
        );
        assert!(instances_for_machine(&all, "mcp-invade").is_empty());
    }

    #[test]
    fn dead_instances_are_ignored_but_unknown_status_counts() {
        let all = vec![
            inst("ghost", "devbox", "old", "dead"),
            inst("stale", "devbox", "gone", "inactive"),
            // An older daemon may not send `status` at all.
            InterconnectInstance {
                instance_id: "tonic".to_string(),
                machine: Some("devbox".to_string()),
                tmux_session: Some("live".to_string()),
                status: None,
            },
        ];
        let by_session = instances_for_machine(&all, "devbox");
        assert_eq!(by_session.get("live").map(String::as_str), Some("tonic"));
        assert!(by_session.get("old").is_none());
        assert!(by_session.get("gone").is_none());
    }

    #[test]
    fn interconnect_payload_parses_and_tolerates_extra_fields() {
        // Real daemon payload carries pane_id/socket_path/etc; we ignore them.
        let json = r#"[{"instance_id":"holly","path":"/x","machine":"devbox",
            "pane_id":"%14","socket_path":"/tmp/s","tmux_session":"terminaler+terminaler",
            "tmux_window":"0","registered_at":"2026-08-06T08:25:52.003Z",
            "last_seen":"2026-08-06T09:14:54.447Z","status":"active"}]"#;
        let parsed: Vec<InterconnectInstance> = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(
            instances_for_machine(&parsed, "devbox")
                .get("terminaler+terminaler")
                .map(String::as_str),
            Some("holly")
        );
    }

    #[test]
    fn empty_interconnect_url_skips_the_request() {
        // Must not attempt a request (and must not panic) when disabled.
        assert!(fetch_interconnect_instances("", Duration::from_millis(50)).is_empty());
        assert!(fetch_interconnect_instances("   ", Duration::from_millis(50)).is_empty());
    }

    #[test]
    fn unreachable_interconnect_yields_no_instances() {
        // Nothing listens here; the lookup must degrade quietly, not error.
        let got = fetch_interconnect_instances("http://127.0.0.1:1", Duration::from_millis(300));
        assert!(got.is_empty());
    }

    /// Encode as wsl.exe does, so the tests exercise real byte layouts.
    fn utf16le(s: &str, bom: bool) -> Vec<u8> {
        let mut out = Vec::new();
        if bom {
            out.extend_from_slice(&[0xFF, 0xFE]);
        }
        for unit in s.encode_utf16() {
            out.extend_from_slice(&unit.to_le_bytes());
        }
        out
    }

    #[test]
    fn decodes_wsl_utf16_diagnostics() {
        let msg = "There is no distribution with the supplied name.\r\n";
        // wsl.exe emits UTF-16LE with and without a BOM depending on version.
        assert!(decode_output(&utf16le(msg, true)).contains("no distribution"));
        assert!(decode_output(&utf16le(msg, false)).contains("no distribution"));
        // Plain UTF-8 from the Linux side must survive untouched.
        assert_eq!(decode_output(b"3 1 1722860000 main\n"), "3 1 1722860000 main\n");
        assert_eq!(decode_output(b""), "");
    }

    #[test]
    fn utf16_error_is_reported_not_swallowed() {
        // The exact regression behind "probe failed with no stderr": the
        // real message was UTF-16 and got mangled into an empty first line.
        let raw = utf16le("There is no distribution with the supplied name.\r\n", true);
        let legacy_first_line = String::from_utf8_lossy(&raw)
            .lines()
            .next()
            .unwrap_or("")
            .trim_matches(|c: char| c == '\u{0}' || c.is_control())
            .to_string();
        // Under the old UTF-8-only path the message was unusable...
        assert!(!legacy_first_line.contains("no distribution"));
        // ...and now it survives.
        let detail = first_meaningful_line(&decode_output(&raw)).unwrap();
        assert!(detail.contains("There is no distribution"), "got {:?}", detail);
    }

    #[test]
    fn no_server_detected_on_either_stream() {
        // tmux's "no server running" is the normal empty state, whichever
        // stream the transport folds it into.
        assert!(is_no_server("no server running on /tmp/tmux-1000/default"));
        assert!(is_no_server(&decode_output(&utf16le(
            "no server running on /tmp/tmux-1000/default\n",
            true
        ))));
    }

    #[test]
    fn first_meaningful_line_skips_blank_and_nul_padding() {
        assert_eq!(first_meaningful_line("\n\n  real message \n"), Some("real message".to_string()));
        assert_eq!(first_meaningful_line("\u{0}\u{0}\n"), None);
        assert_eq!(first_meaningful_line(""), None);
    }

    #[test]
    fn agent_label_strips_path_and_exe() {
        assert_eq!(agent_label("claude"), Some("claude"));
        assert_eq!(agent_label("/usr/local/bin/claude"), Some("claude"));
        assert_eq!(agent_label("Claude.exe"), Some("claude"));
        assert_eq!(agent_label("cursor-agent"), Some("cursor"));
        assert_eq!(agent_label("zsh"), None);
        // `node` is deliberately not an agent: too many false positives.
        assert_eq!(agent_label("node"), None);
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
