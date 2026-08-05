use terminaler_dynamic::{FromDynamic, ToDynamic};

/// Configuration for multibox tmux session discovery and attach
/// (TmuxSessionPicker / AttachTmuxSession actions, sidebar section).
#[derive(Debug, Clone, FromDynamic, ToDynamic)]
pub struct TmuxConfig {
    /// Master switch for the tmux integration.
    #[dynamic(default = "default_true")]
    pub enabled: bool,

    /// How often the discovery service polls each box, in seconds.
    #[dynamic(default = "default_poll_interval")]
    pub poll_interval_seconds: u64,

    /// Per-probe timeout in seconds (connection + command deadline).
    #[dynamic(default = "default_probe_timeout")]
    pub probe_timeout_seconds: u64,

    /// The machines to discover tmux sessions on.
    #[dynamic(default)]
    pub boxes: Vec<TmuxBox>,
}

impl Default for TmuxConfig {
    fn default() -> Self {
        Self {
            enabled: default_true(),
            poll_interval_seconds: default_poll_interval(),
            probe_timeout_seconds: default_probe_timeout(),
            boxes: vec![],
        }
    }
}

#[derive(Debug, Clone, FromDynamic, ToDynamic)]
pub struct TmuxBox {
    /// Unique display name; used as the `box:` prefix in session lists.
    pub name: String,

    /// How to run commands on this box.
    pub connection: TmuxConnection,

    /// Path to the tmux binary on the box.
    #[dynamic(default = "default_tmux_command")]
    pub tmux_command: String,

    #[dynamic(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Clone, FromDynamic, ToDynamic)]
pub enum TmuxConnection {
    /// ssh target resolved via the user's ssh config (host alias, user@host,
    /// or a Tailscale name). `extra_args` are inserted before the target.
    Ssh {
        target: String,
        #[dynamic(default)]
        extra_args: Vec<String>,
    },
    /// wsl.exe -d <distribution>; None uses the default distribution.
    Wsl {
        #[dynamic(default)]
        distribution: Option<String>,
    },
    /// Escape hatch: arbitrary argv prefix the tmux command is appended to
    /// (e.g. ["tailscale", "ssh", "host"]).
    Command { argv_prefix: Vec<String> },
}

impl TmuxBox {
    /// Argv for discovering sessions on this box. The format string puts the
    /// session name LAST so names containing spaces survive ssh's re-joining
    /// of the remote command; parse with `splitn(4, ' ')`.
    pub const LIST_SESSIONS_FORMAT: &'static str =
        "#{session_windows} #{session_attached} #{session_created} #{session_name}";

    pub fn list_sessions_argv(&self, connect_timeout_secs: u64) -> Vec<String> {
        match &self.connection {
            TmuxConnection::Ssh { target, extra_args } => {
                let mut argv = vec![
                    "ssh".to_string(),
                    "-o".to_string(),
                    "BatchMode=yes".to_string(),
                    "-o".to_string(),
                    format!("ConnectTimeout={}", connect_timeout_secs),
                ];
                argv.extend(extra_args.iter().cloned());
                argv.push(target.clone());
                argv.push("--".to_string());
                argv.push(format!(
                    "{} list-sessions -F '{}'",
                    self.tmux_command,
                    Self::LIST_SESSIONS_FORMAT
                ));
                argv
            }
            TmuxConnection::Wsl { distribution } => {
                let mut argv = wsl_prefix(distribution);
                argv.extend([
                    self.tmux_command.clone(),
                    "list-sessions".to_string(),
                    "-F".to_string(),
                    Self::LIST_SESSIONS_FORMAT.to_string(),
                ]);
                argv
            }
            TmuxConnection::Command { argv_prefix } => {
                let mut argv = argv_prefix.clone();
                argv.extend([
                    self.tmux_command.clone(),
                    "list-sessions".to_string(),
                    "-F".to_string(),
                    Self::LIST_SESSIONS_FORMAT.to_string(),
                ]);
                argv
            }
        }
    }

    /// Argv for attaching to a session in tmux control mode (`-CC`).
    /// The resulting command is meant to run in a local pane; the control-mode
    /// handshake then turns tmux windows into native tabs.
    pub fn attach_argv(&self, session: &str) -> Vec<String> {
        self.attach_argv_impl(session, true)
    }

    /// Argv for a plain attach: the classic full-screen tmux UI contained in
    /// the pane. tmux sees real keystrokes, so `bind -n` bindings work.
    pub fn attach_plain_argv(&self, session: &str) -> Vec<String> {
        self.attach_argv_impl(session, false)
    }

    // Two hard-won details in the attach commands below (root-caused live):
    //
    // `-u`: the attach runs as a non-interactive ssh/wsl command, so the
    // remote LANG/LC_ALL from login rc files may be absent and tmux marks
    // the client non-UTF-8 (all TUI glyphs render as underscores). `-u`
    // forces UTF-8 regardless of the remote environment.
    //
    // window-size reset (plain attach only): a control-mode (-CC) attach
    // flips windows to `window-size manual` sized to the GUI and detach does
    // NOT restore it, so a later plain attach in a split becomes a small
    // panning viewport over the oversized window. Plain attach therefore
    // unsets window-size first; control mode resizes explicitly anyway.
    fn attach_argv_impl(&self, session: &str, control_mode: bool) -> Vec<String> {
        match &self.connection {
            TmuxConnection::Ssh { target, extra_args } => {
                let mut argv = vec!["ssh".to_string(), "-t".to_string()];
                argv.extend(extra_args.iter().cloned());
                argv.push(target.clone());
                argv.push("--".to_string());
                // The remote command crosses the remote user's shell, so the
                // session name must be shell-quoted (and we can use shell
                // constructs: reset window-size on EVERY window, then attach).
                let tmux = &self.tmux_command;
                let quoted = shell_quote(session);
                argv.push(if control_mode {
                    format!("{tmux} -u -CC attach-session -t {quoted}")
                } else {
                    format!(
                        "for __w in $({tmux} list-windows -t {quoted} -F '#{{window_id}}'); do \
                         {tmux} set-option -w -t \"$__w\" -u window-size; done; \
                         exec {tmux} -u attach-session -t {quoted}"
                    )
                });
                argv
            }
            TmuxConnection::Wsl { distribution } => {
                let mut argv = wsl_prefix(distribution);
                argv.extend(self.plain_argv_tail(session, control_mode));
                argv
            }
            TmuxConnection::Command { argv_prefix } => {
                let mut argv = argv_prefix.clone();
                argv.extend(self.plain_argv_tail(session, control_mode));
                argv
            }
        }
    }

    /// A single command line safe to TYPE into the active pane's shell,
    /// whatever it is (cmd.exe, PowerShell, or a POSIX shell): plain attach
    /// of the session, replacing the pane's content with the tmux UI.
    ///
    /// Quoting is deliberately conservative — double quotes are the one form
    /// all three shell families pass through as a single argument — so no
    /// window-size reset chain here (its `$`/`;` constructs expand differently
    /// per shell). Session names containing double quotes are not supported
    /// in this mode.
    pub fn attach_typed_command(&self, session: &str) -> String {
        let needs_quotes = session.chars().any(|c| c.is_whitespace());
        match &self.connection {
            TmuxConnection::Ssh { target, extra_args } => {
                let extra = if extra_args.is_empty() {
                    String::new()
                } else {
                    format!("{} ", extra_args.join(" "))
                };
                // The remote arg is locally double-quoted (stripped by every
                // local shell family), leaving single quotes for the remote
                // POSIX shell.
                format!(
                    "ssh -t {}{} -- \"{} -u attach-session -t '{}'\"",
                    extra, target, self.tmux_command, session
                )
            }
            TmuxConnection::Wsl { distribution } => {
                let dist = distribution
                    .as_ref()
                    .map(|d| format!("-d {} ", d))
                    .unwrap_or_default();
                let name = if needs_quotes {
                    format!("\"{}\"", session)
                } else {
                    session.to_string()
                };
                format!(
                    "wsl.exe {}-e {} -u attach-session -t {}",
                    dist, self.tmux_command, name
                )
            }
            TmuxConnection::Command { argv_prefix } => {
                let name = if needs_quotes {
                    format!("\"{}\"", session)
                } else {
                    session.to_string()
                };
                format!(
                    "{} {} -u attach-session -t {}",
                    argv_prefix.join(" "),
                    self.tmux_command,
                    name
                )
            }
        }
    }

    /// Argv-verbatim transports (wsl.exe -e, custom prefixes) have no remote
    /// shell, so the window-size reset uses tmux's own `;` command chaining
    /// (a lone `;` argv element separates tmux commands). This only resets
    /// the session's current window — best effort without a shell loop.
    fn plain_argv_tail(&self, session: &str, control_mode: bool) -> Vec<String> {
        let mut argv = vec![self.tmux_command.clone(), "-u".to_string()];
        if control_mode {
            argv.push("-CC".to_string());
        } else {
            argv.extend([
                "set-option".to_string(),
                "-uw".to_string(),
                "-t".to_string(),
                session.to_string(),
                "window-size".to_string(),
                ";".to_string(),
            ]);
        }
        argv.extend([
            "attach-session".to_string(),
            "-t".to_string(),
            session.to_string(),
        ]);
        argv
    }
}

/// How AttachTmuxSession opens the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, FromDynamic, ToDynamic)]
pub enum TmuxAttachMode {
    /// Plain `tmux attach` in a new split next to the active pane: the
    /// classic tmux UI contained in that pane; layout untouched, tmux
    /// key bindings (bind -n) work.
    SplitPlain,
    /// `tmux -CC attach` in a new tab: control mode turns the session's
    /// windows into native Terminaler tabs (takes focus).
    ControlTab,
    /// Plain `tmux attach` typed into the currently active pane's shell,
    /// replacing its content with the session UI. Requires the pane to be
    /// sitting at a shell prompt.
    CurrentPane,
}

impl Default for TmuxAttachMode {
    fn default() -> Self {
        Self::SplitPlain
    }
}

/// `wsl.exe [-d <dist>] -e` — `-e` passes the rest of the argv verbatim to the
/// command, bypassing the distro's login shell (no quoting hazards).
fn wsl_prefix(distribution: &Option<String>) -> Vec<String> {
    let mut argv = vec!["wsl.exe".to_string()];
    if let Some(dist) = distribution {
        argv.push("-d".to_string());
        argv.push(dist.clone());
    }
    argv.push("-e".to_string());
    argv
}

/// Single-quote a string for a POSIX shell (the remote side of ssh).
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

fn default_true() -> bool {
    true
}

fn default_poll_interval() -> u64 {
    30
}

fn default_probe_timeout() -> u64 {
    8
}

fn default_tmux_command() -> String {
    "tmux".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse the documented JSON shape through the same pipeline the real
    /// config loader uses (serde_json -> dynamic -> FromDynamic), so the
    /// sample in defaults.rs is guaranteed to actually work.
    #[test]
    fn parses_documented_json_shape() {
        let json = r#"{
            "poll_interval_seconds": 30,
            "boxes": [
                { "name": "wsl",        "connection": { "Wsl": { "distribution": "Ubuntu" } } },
                { "name": "devbox",     "connection": { "Ssh": { "target": "devbox" } } },
                { "name": "ts",         "connection": { "Command": { "argv_prefix": ["tailscale", "ssh", "host"] } } }
            ]
        }"#;
        let value: serde_json::Value = serde_json::from_str(json).unwrap();
        let dynamic = crate::json_to_dynamic(&value);
        let cfg = TmuxConfig::from_dynamic(
            &dynamic,
            terminaler_dynamic::FromDynamicOptions {
                unknown_fields: terminaler_dynamic::UnknownFieldAction::Deny,
                deprecated_fields: terminaler_dynamic::UnknownFieldAction::Deny,
            },
        )
        .unwrap();
        assert!(cfg.enabled);
        assert_eq!(cfg.poll_interval_seconds, 30);
        assert_eq!(cfg.boxes.len(), 3);
        assert!(matches!(
            &cfg.boxes[0].connection,
            TmuxConnection::Wsl { distribution: Some(d) } if d == "Ubuntu"
        ));
        assert!(matches!(
            &cfg.boxes[1].connection,
            TmuxConnection::Ssh { target, .. } if target == "devbox"
        ));
        assert!(matches!(
            &cfg.boxes[2].connection,
            TmuxConnection::Command { argv_prefix } if argv_prefix.len() == 3
        ));
    }

    /// `{ "Wsl": {} }` (default distribution) must also parse.
    #[test]
    fn parses_wsl_default_distribution() {
        let json = r#"{ "boxes": [ { "name": "wsl", "connection": { "Wsl": {} } } ] }"#;
        let value: serde_json::Value = serde_json::from_str(json).unwrap();
        let dynamic = crate::json_to_dynamic(&value);
        let cfg = TmuxConfig::from_dynamic(&dynamic, Default::default()).unwrap();
        assert!(matches!(
            &cfg.boxes[0].connection,
            TmuxConnection::Wsl { distribution: None }
        ));
    }

    fn ssh_box() -> TmuxBox {
        TmuxBox {
            name: "devbox".to_string(),
            connection: TmuxConnection::Ssh {
                target: "devbox".to_string(),
                extra_args: vec![],
            },
            tmux_command: default_tmux_command(),
            enabled: true,
        }
    }

    #[test]
    fn ssh_list_sessions_argv() {
        let argv = ssh_box().list_sessions_argv(5);
        assert_eq!(
            argv,
            vec![
                "ssh",
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=5",
                "devbox",
                "--",
                "tmux list-sessions -F '#{session_windows} #{session_attached} #{session_created} #{session_name}'",
            ]
        );
    }

    #[test]
    fn ssh_attach_quotes_session_name() {
        let argv = ssh_box().attach_argv("my session's");
        assert_eq!(
            argv.last().unwrap(),
            r"tmux -u -CC attach-session -t 'my session'\''s'"
        );
    }

    #[test]
    fn plain_attach_resets_window_size_and_forces_utf8() {
        let remote = ssh_box().attach_plain_argv("main").last().unwrap().clone();
        // Shell loop unsets window-size on every window, then attaches with -u.
        assert!(remote.starts_with("for __w in $(tmux list-windows -t 'main'"));
        assert!(remote.contains("set-option -w -t \"$__w\" -u window-size"));
        assert!(remote.ends_with("exec tmux -u attach-session -t 'main'"));

        let b = TmuxBox {
            name: "wsl".to_string(),
            connection: TmuxConnection::Wsl { distribution: None },
            tmux_command: default_tmux_command(),
            enabled: true,
        };
        // No remote shell: tmux `;` command chaining, current window only.
        assert_eq!(
            b.attach_plain_argv("main"),
            vec![
                "wsl.exe", "-e", "tmux", "-u", "set-option", "-uw", "-t", "main",
                "window-size", ";", "attach-session", "-t", "main"
            ]
        );
        assert_eq!(
            b.attach_argv("main"),
            vec!["wsl.exe", "-e", "tmux", "-u", "-CC", "attach-session", "-t", "main"]
        );
    }

    #[test]
    fn wsl_argv_verbatim() {
        let b = TmuxBox {
            name: "wsl".to_string(),
            connection: TmuxConnection::Wsl {
                distribution: Some("Ubuntu".to_string()),
            },
            tmux_command: default_tmux_command(),
            enabled: true,
        };
        assert_eq!(
            b.attach_argv("main"),
            vec![
                "wsl.exe", "-d", "Ubuntu", "-e", "tmux", "-u", "-CC", "attach-session", "-t",
                "main"
            ]
        );
        assert_eq!(b.list_sessions_argv(5)[..4], ["wsl.exe", "-d", "Ubuntu", "-e"]);
    }

    #[test]
    fn typed_command_is_shell_family_safe() {
        assert_eq!(
            ssh_box().attach_typed_command("dark-factory"),
            "ssh -t devbox -- \"tmux -u attach-session -t 'dark-factory'\""
        );
        let b = TmuxBox {
            name: "wsl".to_string(),
            connection: TmuxConnection::Wsl {
                distribution: Some("Ubuntu".to_string()),
            },
            tmux_command: default_tmux_command(),
            enabled: true,
        };
        assert_eq!(
            b.attach_typed_command("my session"),
            "wsl.exe -d Ubuntu -e tmux -u attach-session -t \"my session\""
        );
    }

    #[test]
    fn command_prefix_appends_tmux() {
        let b = TmuxBox {
            name: "ts".to_string(),
            connection: TmuxConnection::Command {
                argv_prefix: vec!["tailscale".to_string(), "ssh".to_string(), "host".to_string()],
            },
            tmux_command: "/usr/bin/tmux".to_string(),
            enabled: true,
        };
        let argv = b.attach_argv("main");
        assert_eq!(argv[..3], ["tailscale", "ssh", "host"]);
        assert_eq!(argv[3], "/usr/bin/tmux");
    }
}
