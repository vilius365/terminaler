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
    /// handshake then takes over the pane.
    pub fn attach_argv(&self, session: &str) -> Vec<String> {
        match &self.connection {
            TmuxConnection::Ssh { target, extra_args } => {
                let mut argv = vec!["ssh".to_string(), "-t".to_string()];
                argv.extend(extra_args.iter().cloned());
                argv.push(target.clone());
                argv.push("--".to_string());
                // The remote command crosses the remote user's shell, so the
                // session name must be shell-quoted.
                argv.push(format!(
                    "{} -CC attach-session -t {}",
                    self.tmux_command,
                    shell_quote(session)
                ));
                argv
            }
            TmuxConnection::Wsl { distribution } => {
                let mut argv = wsl_prefix(distribution);
                argv.extend([
                    self.tmux_command.clone(),
                    "-CC".to_string(),
                    "attach-session".to_string(),
                    "-t".to_string(),
                    session.to_string(),
                ]);
                argv
            }
            TmuxConnection::Command { argv_prefix } => {
                let mut argv = argv_prefix.clone();
                argv.extend([
                    self.tmux_command.clone(),
                    "-CC".to_string(),
                    "attach-session".to_string(),
                    "-t".to_string(),
                    session.to_string(),
                ]);
                argv
            }
        }
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
            r"tmux -CC attach-session -t 'my session'\''s'"
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
            vec!["wsl.exe", "-d", "Ubuntu", "-e", "tmux", "-CC", "attach-session", "-t", "main"]
        );
        assert_eq!(b.list_sessions_argv(5)[..4], ["wsl.exe", "-d", "Ubuntu", "-e"]);
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
