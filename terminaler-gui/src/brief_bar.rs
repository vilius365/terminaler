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

fn collapse_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Parse the feed, collapse whitespace, and sort by name. A stable order keeps
/// each session's cell in place between polls, so a click lands where the eye
/// expects; sorting by activity would reshuffle the strip every 20 seconds.
pub fn parse_rows(stdout: &str) -> Result<Vec<BriefRow>, String> {
    let mut rows: Vec<BriefRow> =
        serde_json::from_str(stdout.trim()).map_err(|e| format!("bad brief JSON: {}", e))?;
    for row in &mut rows {
        for field in [&mut row.name, &mut row.goal, &mut row.now] {
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

    #[test]
    fn parses_full_row_and_null_goal() {
        let rows = parse_rows(
            r#"[
            {"name":"notch","goal":"Slim  the\nbuttons","now":"Wrapping up","active_at":"2026-10-05T10:00:00Z","inferred":false,"stale":false,"headline":"x"},
            {"name":"sup","goal":null,"active_at":"2026-10-05T11:00:00Z"}
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
    fn tolerates_missing_fields_and_empty_array() {
        assert_eq!(parse_rows("[]").unwrap().len(), 0);
        let rows = parse_rows(r#"[{}]"#).unwrap();
        assert_eq!(rows[0], BriefRow::default());
    }

    #[test]
    fn sorts_by_name() {
        let rows = parse_rows(r#"[{"name":"b"},{"name":"a"}]"#).unwrap();
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
