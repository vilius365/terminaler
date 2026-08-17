# Terminaler

Windows-native terminal multiplexer with predefined snap layouts, workspace templates, and session persistence. Forked from [WezTerm](https://github.com/wez/wezterm) (MIT license).

## Git Workflow (overrides the global feature-branch rule)

Personal project — **commit straight to `main`**. No feature branches, no PRs.
This is a deliberate project-level override of the user-global "all code
changes must be a PR on a new branch" rule (2026-08-17).

Unchanged from the global rules: push and PR remain **user-driven** — do not
`git push` autonomously at end of turn.

## Quick Start

```bash
# Build (requires Rust toolchain)
cargo build

# Run (GUI)
cargo run --bin terminaler-gui

# Run tests
cargo test

# Cross-compile for Windows (from WSL/Linux)
cargo build --target x86_64-pc-windows-gnu
```

### Shipping a Windows build (pCloud)

`~/pCloudDrive/terminaler-windows-build/` **is** the folder the Windows exes run
from — it is not a staging area. Copying into it *is* deploying, and Windows
locks running exes, so a direct build requires quitting Terminaler first.

Use the staging script instead — it builds into a sibling folder, so you can keep
working while it runs:

```bash
ci/build-windows-staging.sh              # build -> ~/pCloudDrive/terminaler-windows-staging/
ci/build-windows-staging.sh --status     # compare staged vs live build times
ci/build-windows-staging.sh --promote    # staging -> live (backs up current exes first)
```

Only `--promote` needs Terminaler (and `terminaler-mux-server.exe`) fully closed.
Support files that must travel with the exes: `WebView2Loader.dll`, `conpty.dll`,
`OpenConsole.exe` — the script handles all three.

**Config location**: `%APPDATA%\Terminaler\terminaler.json` (JSONC with comments)

## Architecture Overview

```
[terminaler-gui.exe]       [terminaler-mux-server.exe]
  (GPU client)          <--->    (background process)
       |         Named Pipe         |
  Renders panes              Holds PTY sessions
  Handles input              Manages mux state
  Snap layout UI             Persists sessions to JSON
                             Web server (optional)
```

Two-process model: GUI client renders and handles input, daemon holds PTY sessions and mux state. Communication via Windows named pipes. Sessions survive GUI restarts.

## Crate Map

| Crate | Purpose |
|-------|---------|
| `terminaler-gui/` | **Main GUI binary**. Window management, GPU rendering, input handling |
| `terminaler-mux-server/` | Background daemon — PTY session host |
| `terminaler-layout/` | Snap layout engine — declarative layout tree, 8 built-in presets, workspace templates |
| `terminaler-web/` | Remote web access server — axum + xterm.js + WebSocket |
| `config/` | JSON configuration system (JSONC with comments) |
| `mux/` | Multiplexer core — tabs, panes, domains, session state |
| `bintree/` | Binary tree with zipper cursor — pane layout data structure |
| `term/` (terminaler-term) | Terminal emulator core (VT parser, cell grid, scrollback) |
| `termwiz/` | Terminal wizardry — input/output abstractions, surface rendering |
| `vtparse/` | VT parser state machine |
| `pty/` (portable-pty) | PTY abstraction (ConPTY on Windows) |
| `codec/` | Mux client-server protocol codec (PDUs over streams) |
| `window/` | Platform window abstraction (Windows backend: `window/src/os/windows/`) |
| `terminaler-font/` | Font discovery, shaping (HarfBuzz), rasterization (FreeType) |
| `terminaler-input-types/` | Input event types (keys, mouse) |
| `terminaler-surface/` | Surface rendering primitives |
| `terminaler-blob-leases/` | Blob lease memory management |
| `terminaler-dynamic/` | Dynamic value bridge (FromDynamic/ToDynamic) |
| `color-types/` | Color type definitions |
| `rangeset/` | Range set data structure |
| `filedescriptor/` | Cross-platform file descriptor abstraction |
| `promise/` | Promise/future utilities |

## Key Source Files

| File | Purpose |
|------|---------|
| `terminaler-gui/src/termwindow/mod.rs` | Terminal window orchestration — overlays, snap layout application |
| `terminaler-gui/src/termwindow/render/pane.rs` | Pane rendering — split highlights, long-press overlay, layout icons |
| `terminaler-gui/src/termwindow/render/mod.rs` | GPU rendering pipeline |
| `terminaler-gui/src/termwindow/mouseevent.rs` | Mouse event handling — long-press detection, button clicks |
| `terminaler-gui/src/termwindow/render/tab_sidebar.rs` | Vertical tab sidebar — Claude Card, notifications, pane tree |
| `terminaler-gui/src/termwindow/render/fancy_tab_bar.rs` | Horizontal fancy tab bar with window buttons |
| `terminaler-gui/src/tabbar.rs` | Tab bar rendering |
| `terminaler-escape-parser/src/osc.rs` | OSC escape sequence parser (9/99/777 notifications) |
| `terminaler-layout/src/lib.rs` | Layout presets, workspace templates, split operations |
| `config/src/lib.rs` | Config loading pipeline (JSON) |
| `config/src/config.rs` | Config struct with serde derives |
| `config/src/keyassignment.rs` | KeyAssignment enum — all keyboard actions |
| `config/src/themes.rs` | Dark/light color scheme definitions |
| `config/src/defaults.rs` | First-run default config generation |
| `config/src/web.rs` | WebAccessConfig struct |
| `mux/src/tab.rs` | Tab with bintree::Tree pane layout |
| `mux/src/session_state.rs` | Session state serialization (save/restore) |
| `mux/src/domain.rs` | Domain trait (shell spawning) — Local + WSL |
| `mux/src/tmux.rs` | TmuxDomain — tmux -CC control mode; windows become tabs (restored from upstream) |
| `config/src/tmux.rs` | TmuxConfig/TmuxBox — multibox tmux discovery config + attach argv builders |
| `terminaler-gui/src/tmux_discovery.rs` | Background tmux session poller (ssh/wsl probes, cached snapshots, agent/instance labelling) |
| `bintree/src/lib.rs` | Binary tree (Tree<L,N> enum, cursors) |
| `terminaler-web/src/lib.rs` | Web server public API |
| `terminaler-web/src/ws_session.rs` | WebSocket session management |

## Conventions

### Rust Style
- Follow existing conventions (rustfmt defaults)
- Use `anyhow::Result` for error propagation
- Use `log` crate for logging (`log::info!`, `log::error!`, etc.)
- Use `parking_lot::Mutex` over `std::sync::Mutex`
- `Arc<dyn Pane>` for pane references in the mux

### Naming
- Crate names: `terminaler-*` (kebab-case)
- Binary names: `terminaler-gui` (GUI), `terminaler-mux-server` (daemon), `terminaler` (CLI)
- Config keys: **snake_case** in JSON — the keys are the literal Rust field names
  (no rename attributes), e.g. `default_prog`, `default_domain`, `font_size`,
  `web_access`/`bind_address`. `config/src/defaults.rs` is the accurate reference.
- Rust identifiers: standard conventions (snake_case for functions/variables, PascalCase for types)

### Error Handling
- Use `anyhow::Context` for adding context to errors
- Log errors before propagating when at system boundaries
- Never silently swallow errors

## JSON Configuration Format

Config file: `%APPDATA%\Terminaler\terminaler.json` (JSONC - comments allowed)

```jsonc
// Keys below are VERIFIED against config/src/config.rs. Note there is no
// "profiles", "layouts", "workspaces", "keybindings" or "theme" key — earlier
// revisions of this file documented those, but the parser has never had them.
{
    // Default program for new panes. Element 0 is the executable, the rest are
    // arguments. Omit for the platform default shell (PowerShell on Windows).
    "default_prog": ["ssh", "devbox"],

    // Default domain (shell). "local", or "WSL:<distro>" where <distro> matches
    // `wsl.exe -l -v` EXACTLY (literal match, not a prefix).
    "default_domain": "local",

    // Appearance
    "color_scheme": "Gruvbox Dark (Gogh)",
    "font_size": 12.0,
    "colors": {...},

    // Keybindings — note the key is "keys", and each entry is key + action
    "keys": [
        { "key": "ctrl+shift+l", "action": { "SnapLayoutPicker": null } }
    ],

    // Remote web access
    "web_access": {
        "enabled": false,
        "bind_address": "127.0.0.1:9876"
    },

    // Multibox tmux discovery (sidebar card + Ctrl+Shift+S picker)
    "tmux": {
        // Sessions are labelled with the claude-agent-interconnect instance
        // name (e.g. "witch") where one is registered, falling back to the
        // agent type from the pane command (e.g. "claude"). "" disables the
        // instance lookup and leaves only the fallback.
        // NOTE: snake_case keys, like the rest of the config; unknown keys are
        // rejected outright, so camelCase fails.
        // Also: JSONC allows comments but NOT trailing commas — on any parse
        // error the last-good config is kept near-silently, so a bad edit
        // looks exactly like a broken feature. Suspect syntax first.
        // interconnect_url must be reachable FROM the machine running the GUI;
        // the 127.0.0.1 default is wrong whenever the daemon is on another box.
        "interconnect_url": "http://127.0.0.1:7799",
        "boxes": [
            { "name": "devbox", "connection": { "Ssh": { "target": "devbox" } } },
            // interconnect_machine: this box's CLAUDE_MACHINE_NAME, when the
            // registry knows it by a different name than `name`.
            // "distribution" must match `wsl.exe -l -v` EXACTLY (literal, not a
            // prefix match); omit the key to track the default distro. It feeds
            // the probe AND both attach paths, so a wrong value breaks all three.
            { "name": "wsl", "interconnect_machine": "home",
              "connection": { "Wsl": { "distribution": "Ubuntu" } } }
        ]
    }
}

The `Ssh` variant hardcodes `-o BatchMode=yes`, so probes never prompt. A box
behind an interactive auth gate (e.g. Tailscale SSH in `check` mode) therefore
fails with only a timeout, and `sshd` logs a bare `Connection reset [preauth]`
rather than an auth error — a plain interactive `ssh host` still succeeds and
hides it. Reproduce with the prober's own argv, or use the `Command` variant
(`{"argv_prefix": ["tailscale", "ssh", "user@host"]}`) to bypass BatchMode.
```

## WezTerm Upstream Reference

- Repository: https://github.com/wez/wezterm
- Docs: https://wezfurlong.org/wezterm/
- License: MIT
- Forked from: main branch (shallow clone, 2026-02-21)

Cherry-pick terminal emulation bugfixes from upstream as needed. Do not attempt to stay in sync with feature development.
