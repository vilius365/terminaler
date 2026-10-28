//! Brief bar feed: a background thread that periodically runs a configured
//! command, parses its JSON array of live Claude Code sessions, and caches the
//! result in a global snapshot. The GUI reads `snapshot()` at paint time and
//! never blocks on the network. After every poll the thread invalidates all
//! GUI windows so an idle window repaints (there is no idle repaint cadence).

use config::BriefBarConfig;
use parking_lot::{Condvar, Mutex};
use serde::Deserialize;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// One live session as printed by `brief --json`. Every field is optional so a
/// feed that adds, drops or nulls fields still parses.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct BriefRow {
    pub name: Option<String>,
    pub machine: Option<String>,
    pub path: Option<String>,
    pub session_id: Option<String>,
    pub epic: Option<String>,
    pub epic_title: Option<String>,
    pub goal: Option<String>,
    pub now: Option<String>,
    pub active_at: Option<String>,
    pub inferred: Option<bool>,
    pub stale: Option<bool>,
}

#[derive(Debug, Clone, Default)]
pub struct BriefSnapshot {
    pub rows: Vec<BriefRow>,
    pub last_success: Option<Instant>,
    pub last_error: Option<String>,
}

static STATE: OnceLock<Mutex<BriefSnapshot>> = OnceLock::new();
static WAKE: OnceLock<(Mutex<bool>, Condvar)> = OnceLock::new();
static STARTED: AtomicBool = AtomicBool::new(false);

fn state() -> &'static Mutex<BriefSnapshot> {
    STATE.get_or_init(|| Mutex::new(BriefSnapshot::default()))
}

fn wake() -> &'static (Mutex<bool>, Condvar) {
    WAKE.get_or_init(|| (Mutex::new(false), Condvar::new()))
}

/// Cheap clone of the current feed state. Safe on any thread.
pub fn snapshot() -> BriefSnapshot {
    state().lock().clone()
}

/// Start the background poller once. Subsequent calls are no-ops. The live
/// configuration is read every cycle, so reloads apply without a restart.
pub fn ensure_running() {
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    if let Err(err) = std::thread::Builder::new()
        .name("brief-bar".to_string())
        .spawn(poller_loop)
    {
        log::error!("brief_bar: failed to spawn poller thread: {}", err);
        STARTED.store(false, Ordering::SeqCst);
    }
}

/// Resolve a feed row to the `(box_name, session)` it can be attached on: an
/// enabled configured tmux box whose interconnect machine name equals the
/// row's `machine`, holding an attachable session whose agent is an
/// interconnect instance named like the row. `None` when nothing matches.
pub fn match_session(
    row: &BriefRow,
    tmux: Option<&config::TmuxConfig>,
    snaps: &[crate::tmux_discovery::BoxSnapshot],
) -> Option<(String, String)> {
    let name = row.name.as_deref()?;
    let machine = row.machine.as_deref()?;
    let tmux = tmux?;
    for snap in snaps {
        let configured = tmux
            .boxes
            .iter()
            .find(|b| b.enabled && b.name == snap.box_name);
        let Some(configured) = configured else {
            continue;
        };
        if configured.interconnect_machine_name() != machine {
            continue;
        }
        let hit = snap.sessions.iter().find(|e| {
            e.attachable && e.agent_is_instance && e.agent.as_deref() == Some(name)
        });
        if let Some(entry) = hit {
            return Some((snap.box_name.clone(), entry.session.clone()));
        }
    }
    None
}

/// What a cell shows for a live session that has no goal yet (a fresh
/// session that has not received a prompt): the directory it runs in, so the
/// cell still says where the session is.
pub fn fallback_goal(path: Option<&str>) -> String {
    let home = std::env::var("HOME").ok();
    let dir = match path.map(|p| p.trim_end_matches('/')) {
        None | Some("") => return "no prompt yet".to_string(),
        Some(p) if home.as_deref() == Some(p) => "~".to_string(),
        Some(p) => p.rsplit('/').next().unwrap_or(p).to_string(),
    };
    format!("no prompt yet in {}", dir)
}

/// Last path segment of a working directory; `~` for the home directory.
fn dir_name(path: Option<&str>) -> Option<String> {
    let path = path?.trim_end_matches('/');
    if path.is_empty() {
        return None;
    }
    if std::env::var("HOME").ok().as_deref() == Some(path) {
        return Some("~".to_string());
    }
    path.rsplit('/').next().map(str::to_string)
}

/// A tmux session name nobody chose: empty, or tmux's own numeric default.
fn is_unnamed(session: &str) -> bool {
    let s = session.trim();
    s.is_empty() || s.chars().all(|c| c.is_ascii_digit())
}

/// What the sidebar calls a Claude session: its epic title when the feed
/// knows one, else the tmux session name, else the working directory. `rows`
/// are matched on instance name and interconnect machine, as `match_session`
/// does.
pub fn sidebar_label(
    session: &str,
    instance: Option<&str>,
    machine: &str,
    rows: &[BriefRow],
) -> String {
    let row = instance.and_then(|name| {
        rows.iter().find(|r| {
            r.name.as_deref() == Some(name) && r.machine.as_deref() == Some(machine)
        })
    });
    if let Some(epic) = row
        .and_then(|r| r.epic_title.as_deref().or(r.epic.as_deref()))
        .filter(|e| !e.is_empty())
    {
        return epic.to_string();
    }
    if !is_unnamed(session) {
        return session.to_string();
    }
    row.and_then(|r| dir_name(r.path.as_deref()))
        .unwrap_or_else(|| session.to_string())
}

fn collapse_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Parse the feed, collapse whitespace, and sort by name. A stable order keeps
/// each session's cell in place between polls, so a click lands where the eye
/// expects; sorting by activity would reshuffle the strip every 20 seconds.
pub fn parse_rows(stdout: &str) -> Result<Vec<BriefRow>, String> {
    let mut rows: Vec<BriefRow> =
        serde_json::from_str(stdout.trim()).map_err(|e| format!("bad brief JSON: {}", e))?;
    // Overlord slots are named `ol-<word>`; they are plumbing, not work the
    // strip is meant to surface.
    rows.retain(|r| !r.name.as_deref().is_some_and(|n| n.starts_with("ol-")));
    // Rows without a session id are agents that are not Claude Code sessions
    // (Hermes agents such as `karma` and `infra-agent`). They can never carry
    // a brief, so a cell for them would only ever read "no brief".
    rows.retain(|r| r.session_id.is_some());
    for row in &mut rows {
        for field in [
            &mut row.name,
            &mut row.goal,
            &mut row.now,
            &mut row.epic,
            &mut row.epic_title,
        ] {
            if let Some(text) = field.as_mut() {
                *text = collapse_whitespace(text);
                if text.is_empty() {
                    *field = None;
                }
            }
        }
    }
    rows.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.machine.cmp(&b.machine)));
    Ok(rows)
}

fn run_feed(argv: &[String], timeout: Duration) -> Result<String, String> {
    let program = argv.first().ok_or_else(|| "empty command".to_string())?;
    let mut cmd = std::process::Command::new(program);
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
        .map_err(|e| format!("failed to run {}: {}", program, e))?;
    let output = crate::tmux_discovery::wait_with_timeout(child, timeout)?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = stderr
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| match output.status.code() {
                Some(code) => format!("{} exited with code {}", program, code),
                None => format!("{} terminated by signal", program),
            });
        Err(detail)
    }
}

/// Repaint every GUI window after a state change, from the GUI thread.
fn invalidate_windows() {
    promise::spawn::spawn_into_main_thread(async {
        if let Some(front_end) = crate::frontend::try_front_end() {
            for win in front_end.gui_windows() {
                use ::window::WindowOps;
                win.window.invalidate();
            }
        }
    })
    .detach();
}

fn poller_loop() {
    loop {
        let config = config::configuration();
        let cfg: BriefBarConfig = config.brief_bar.clone().unwrap_or_default();
        let interval = Duration::from_secs(
            cfg.poll_interval_seconds
                .max(config::BRIEF_BAR_MIN_POLL_SECONDS),
        );

        if cfg.enabled && !cfg.command.is_empty() {
            let result = run_feed(&cfg.command, Duration::from_secs(cfg.timeout_seconds.max(1)))
                .and_then(|out| parse_rows(&out));
            {
                let mut st = state().lock();
                match result {
                    Ok(rows) => {
                        st.rows = rows;
                        st.last_success = Some(Instant::now());
                        st.last_error = None;
                    }
                    Err(err) => {
                        log::warn!("brief_bar: poll failed: {}", err);
                        st.last_error = Some(err);
                    }
                }
            }
            invalidate_windows();
        } else {
            *state().lock() = BriefSnapshot::default();
        }

        let (lock, condvar) = wake();
        let mut woken = lock.lock();
        if !*woken {
            let _ = condvar.wait_for(&mut woken, interval);
        }
        *woken = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn label_row(epic: Option<&str>, path: Option<&str>) -> BriefRow {
        BriefRow {
            name: Some("witch".into()),
            machine: Some("devbox".into()),
            epic_title: epic.map(str::to_string),
            path: path.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn sidebar_label_prefers_epic_then_session_then_dir() {
        let rows = [label_row(Some("Terminaler terminal app"), Some("/a/proj"))];
        assert_eq!(
            sidebar_label("main", Some("witch"), "devbox", &rows),
            "Terminaler terminal app"
        );
        let rows = [label_row(None, Some("/a/proj"))];
        assert_eq!(sidebar_label("main", Some("witch"), "devbox", &rows), "main");
        assert_eq!(sidebar_label("3", Some("witch"), "devbox", &rows), "proj");
        // Other machine or no instance: the row does not apply.
        assert_eq!(sidebar_label("3", Some("witch"), "wsl", &rows), "3");
        assert_eq!(sidebar_label("3", None, "devbox", &rows), "3");
    }

    #[test]
    fn parses_full_row_and_null_goal() {
        let rows = parse_rows(
            r#"[
            {"name":"notch","session_id":"s1","goal":"Slim  the\nbuttons","now":"Wrapping up","active_at":"2026-10-05T10:00:00Z","inferred":false,"stale":false,"headline":"x"},
            {"name":"sup","session_id":"s2","goal":null,"active_at":"2026-10-05T11:00:00Z"}
        ]"#,
        )
        .unwrap();
        assert_eq!(rows.len(), 2);
        // Sorted by name, not by activity.
        assert_eq!(rows[0].name.as_deref(), Some("notch"));
        assert_eq!(rows[0].goal.as_deref(), Some("Slim the buttons"));
        assert_eq!(rows[1].goal, None);
    }

    #[test]
    fn drops_overlord_rows() {
        let rows = parse_rows(
            r#"[{"name":"ol-otter","session_id":"s0","goal":"x"},{"name":"spell","session_id":"s1","goal":"y"}]"#,
        )
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name.as_deref(), Some("spell"));
    }

    #[test]
    fn drops_rows_without_a_session() {
        let rows = parse_rows(
            r#"[{"name":"karma","session_id":null,"goal":null},{"name":"infra-agent"},{"name":"spell","session_id":"s1"}]"#,
        )
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name.as_deref(), Some("spell"));
    }

    #[test]
    fn fallback_goal_names_the_directory() {
        assert_eq!(fallback_goal(Some("/srv/work/terminaler/")), "no prompt yet in terminaler");
        assert_eq!(fallback_goal(None), "no prompt yet");
        assert_eq!(fallback_goal(Some("")), "no prompt yet");
    }

    #[test]
    fn parses_epic_fields() {
        let rows = parse_rows(
            r#"[{"name":"notch","session_id":"a","epic":"terminaler-terminal-app","epic_title":"Terminaler terminal app"},{"name":"x","session_id":"b","epic":null,"epic_title":null}]"#,
        )
        .unwrap();
        assert_eq!(rows[0].epic.as_deref(), Some("terminaler-terminal-app"));
        assert_eq!(rows[0].epic_title.as_deref(), Some("Terminaler terminal app"));
        assert_eq!(rows[1].epic, None);
    }

    #[test]
    fn tolerates_missing_fields_and_empty_array() {
        assert_eq!(parse_rows("[]").unwrap().len(), 0);
        let rows = parse_rows(r#"[{"session_id":"s"}]"#).unwrap();
        assert_eq!(
            rows[0],
            BriefRow {
                session_id: Some("s".to_string()),
                ..BriefRow::default()
            }
        );
    }

    #[test]
    fn sorts_by_name() {
        let rows = parse_rows(r#"[{"name":"b","session_id":"1"},{"name":"a","session_id":"2"}]"#).unwrap();
        assert_eq!(rows[0].name.as_deref(), Some("a"));
    }

    #[test]
    fn garbage_is_an_error() {
        assert!(parse_rows("not json").is_err());
        assert!(parse_rows(r#"{"name":"x"}"#).is_err());
    }

    fn tmux_box(name: &str, machine: Option<&str>) -> config::tmux::TmuxBox {
        config::tmux::TmuxBox {
            name: name.to_string(),
            connection: config::tmux::TmuxConnection::Ssh {
                target: name.to_string(),
                extra_args: vec![],
            },
            tmux_command: "tmux".to_string(),
            interconnect_machine: machine.map(str::to_string),
            enabled: true,
        }
    }

    fn entry(session: &str, agent: &str, instance: bool, attachable: bool) -> crate::tmux_discovery::TmuxSessionEntry {
        crate::tmux_discovery::TmuxSessionEntry {
            session: session.to_string(),
            windows: 1,
            attached: false,
            agent: Some(agent.to_string()),
            agent_is_instance: instance,
            attachable,
        }
    }

    fn snap(box_name: &str, sessions: Vec<crate::tmux_discovery::TmuxSessionEntry>) -> crate::tmux_discovery::BoxSnapshot {
        crate::tmux_discovery::BoxSnapshot {
            box_name: box_name.to_string(),
            status: crate::tmux_discovery::BoxStatus::Ok,
            sessions,
            last_success: None,
            stale: false,
            updating: false,
        }
    }

    fn row(name: &str, machine: &str) -> BriefRow {
        BriefRow {
            name: Some(name.to_string()),
            machine: Some(machine.to_string()),
            ..Default::default()
        }
    }

    fn cfg(boxes: Vec<config::tmux::TmuxBox>) -> config::TmuxConfig {
        config::TmuxConfig {
            boxes,
            ..Default::default()
        }
    }

    #[test]
    fn match_by_instance_name_and_machine() {
        let c = cfg(vec![tmux_box("wsl", Some("home")), tmux_box("devbox", None)]);
        let snaps = vec![
            snap("wsl", vec![entry("s1", "notch", true, true)]),
            snap("devbox", vec![entry("s2", "notch", true, true)]),
        ];
        // "home" is wsl's interconnect machine; "devbox" defaults to its name.
        assert_eq!(match_session(&row("notch", "home"), Some(&c), &snaps), Some(("wsl".into(), "s1".into())));
        assert_eq!(match_session(&row("notch", "devbox"), Some(&c), &snaps), Some(("devbox".into(), "s2".into())));
    }

    #[test]
    fn no_match_wrong_machine() {
        let c = cfg(vec![tmux_box("devbox", None)]);
        let snaps = vec![snap("devbox", vec![entry("s", "notch", true, true)])];
        assert_eq!(match_session(&row("notch", "laptop"), Some(&c), &snaps), None);
    }

    #[test]
    fn no_match_generic_agent_type() {
        let c = cfg(vec![tmux_box("devbox", None)]);
        let snaps = vec![snap("devbox", vec![entry("s", "claude", false, true)])];
        assert_eq!(match_session(&row("claude", "devbox"), Some(&c), &snaps), None);
    }

    #[test]
    fn no_match_not_attachable() {
        let c = cfg(vec![tmux_box("devbox", None)]);
        let snaps = vec![snap("devbox", vec![entry("s", "notch", true, false)])];
        assert_eq!(match_session(&row("notch", "devbox"), Some(&c), &snaps), None);
    }

    #[test]
    fn no_match_without_tmux_config_or_machine() {
        let snaps = vec![snap("devbox", vec![entry("s", "notch", true, true)])];
        assert_eq!(match_session(&row("notch", "devbox"), None, &snaps), None);
        let c = cfg(vec![tmux_box("devbox", None)]);
        let mut r = row("notch", "devbox");
        r.machine = None;
        assert_eq!(match_session(&r, Some(&c), &snaps), None);
    }
}
