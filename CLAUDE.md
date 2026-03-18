# Terminaler

Windows-native terminal multiplexer with predefined snap layouts, workspace templates, and session persistence. Forked from [WezTerm](https://github.com/wez/wezterm) (MIT license).

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

**Config location**: `%APPDATA%\Terminaler\terminaler.json` (JSONC with comments)

## Architecture Overview

```
[terminaler-gui.exe]          [terminaler-daemon.exe]
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
- Binary names: `terminaler-gui`, `terminaler-daemon`
- Config keys: camelCase in JSON
- Rust identifiers: standard conventions (snake_case for functions/variables, PascalCase for types)

### Error Handling
- Use `anyhow::Context` for adding context to errors
- Log errors before propagating when at system boundaries
- Never silently swallow errors

## JSON Configuration Format

Config file: `%APPDATA%\Terminaler\terminaler.json` (JSONC - comments allowed)

```jsonc
{
    // Shell profiles
    "profiles": [...],

    // Snap layout presets (8 built-in + custom)
    "layouts": {
        "builtIn": [...],
        "custom": [...]
    },

    // Workspace templates
    "workspaces": [...],

    // Keybindings (action ID + key mapping)
    "keybindings": [
        { "keys": "ctrl+shift+l", "id": "Terminaler.SnapLayoutPicker" },
        { "keys": "ctrl+shift+o", "id": "Terminaler.WorkspacePicker" }
    ],

    // Appearance
    "theme": "dark",
    "font": { "family": "Cascadia Code", "size": 12 },
    "colors": {...},

    // Remote web access
    "webAccess": {
        "enabled": false,
        "bindAddress": "127.0.0.1:9876"
    }
}
```

## WezTerm Upstream Reference

- Repository: https://github.com/wez/wezterm
- Docs: https://wezfurlong.org/wezterm/
- License: MIT
- Forked from: main branch (shallow clone, 2026-02-21)

Cherry-pick terminal emulation bugfixes from upstream as needed. Do not attempt to stay in sync with feature development.
