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
```

**Config location**: `%APPDATA%\Terminaler\terminaler.json` (JSONC with comments)

## Architecture Overview

```
[terminaler.exe]          [terminaler-daemon.exe]
  (GUI client)     <--->    (background process)
     |    Named Pipe         |
  Renders panes          Holds PTY sessions
  Handles input          Manages mux state
  Layout UI              Persists to JSON on disk
```

Two-process model: GUI client renders and handles input, daemon holds PTY sessions and mux state. Communication via Windows named pipes.

## Crate Map

### Core (KEEP)

| Crate | Purpose |
|-------|---------|
| `mux/` | Multiplexer: tabs, panes, domains. Central orchestration |
| `bintree/` | Binary tree with Zipper-based cursor. Used for pane layout within tabs |
| `term/` (terminaler-term) | Terminal emulator core (VT parser, cell grid, scrollback) |
| `termwiz/` | Terminal wizardry - input/output abstractions, surface rendering |
| `vtparse/` | VT parser state machine |
| `pty/` (portable-pty) | PTY abstraction. Uses ConPTY on Windows |
| `codec/` | Mux client-server protocol codec (PDUs over streams) |
| `terminaler-gui/` | **Main GUI binary**. Window management, rendering pipeline, input handling |
| `window/` | Window abstraction layer. Platform backends (Windows = `window/src/os/windows/`) |
| `terminaler-font/` | Font discovery, shaping (HarfBuzz), rasterization (FreeType) |
| `terminaler-input-types/` | Input event types (keys, mouse) |
| `color-types/` | Color type definitions |
| `rangeset/` | Range set data structure |
| `filedescriptor/` | Cross-platform file descriptor abstraction |
| `promise/` | Promise/future utilities |
| `terminaler-surface/` | Surface rendering primitives |
| `terminaler-blob-leases/` | Blob lease memory management |
| `terminaler-dynamic/` | Dynamic value bridge (keep temporarily, remove later) |
| `config/` | Configuration system. **WILL BE REWRITTEN** from Lua to JSON |

### Already Stripped

These crates/paths were removed in Phase 0:
- `wezterm-ssh/`, `luahelper/`, `sync-color-schemes/`, `bidi/`, `wezterm-open-url/`, `deps/cairo/`, `lua-api-crates/`
- `window/src/os/macos/`, `window/src/os/wayland/`, `window/src/os/x11/`

### Future Changes

| Current | Future | Purpose |
|---------|--------|---------|
| `terminaler-mux-server/` | `terminaler-daemon` | Background daemon for PTY persistence (Phase 5) |
| `config/` (Lua loader) | `config/` (JSON loader) | Configuration system (Phase 1) |

### New

| Crate | Purpose |
|-------|---------|
| `terminaler-layout/` | Snap layout engine: declarative tree -> bintree materialization |

## Key Source Files

| File | Purpose |
|------|---------|
| `config/src/lua.rs` | Lua config integration (~700 lines) - **DELETE in Phase 1** |
| `config/src/lib.rs` | Config loading pipeline - **REWRITE for JSON** |
| `config/src/config.rs` | Config struct (150+ fields) - **SLIM DOWN + serde derives** |
| `config/src/keyassignment.rs` | KeyAssignment enum - **ADD new Terminaler actions** |
| `config/src/wsl.rs` | WSL distro detection - **KEEP as-is** |
| `mux/src/tab.rs` | Tab with bintree::Tree pane layout - **ADD snap layout support** |
| `mux/src/domain.rs` | Domain trait (shell spawning) - **KEEP Local + WSL only** |
| `bintree/src/lib.rs` | Binary tree (Tree<L,N> enum, cursors) - **UNDERSTAND** |
| `terminaler-gui/src/termwindow/mod.rs` | Terminal window orchestration - **ADD overlays, hover, drag-drop** |
| `terminaler-gui/src/termwindow/render/mod.rs` | GPU rendering pipeline - **ADD pane highlight, toast** |
| `terminaler-gui/src/tabbar.rs` | Tab bar rendering - **RESTYLE** |

## Implementation Phases

| Phase | Description | Status |
|-------|-------------|--------|
| 0 | Fork setup: clone, rename, strip non-Windows, strip unused crates | **COMPLETE** |
| 1 | JSON config: replace Lua with serde_json config system | Pending |
| 2 | Snap layouts: terminaler-layout crate, 8 built-in presets, picker UI | Pending |
| 3 | UX features: hover highlight, toast, ctrl+scroll font, drag-drop, resize | Pending |
| 4 | Theming & UI polish: dark/light themes, tab bar restyle, command palette | Pending |
| 5 | Session persistence: terminaler-daemon, named pipes, session restore | Pending |
| 6 | Workspaces: workspace templates, launcher UI, Claude Code templates | Pending |
| 7 | Search & polish: terminal search, URL detection, installer | Pending |
| 8 | Testing & release: integration tests, performance, v1.0 | Pending |

## Phase 0 Checklist

- [x] Clone WezTerm repository
- [x] Strip non-Windows platform code (macos, wayland, x11 from window crate)
- [x] Strip SSH, serial, TLS domain code from mux
- [x] Remove unused crates (wezterm-ssh, bidi, cairo, luahelper, sync-color-schemes, lua-api-crates)
- [x] Rename project from WezTerm to Terminaler (22 crates, env vars, strings, terminfo)
- [x] Initialize git repo and make initial commit
- [x] Verify builds on Windows (cross-compiled x86_64-pc-windows-gnu via mingw-w64)

## Conventions

### Rust Style
- Follow existing WezTerm conventions (rustfmt defaults)
- Use `anyhow::Result` for error propagation
- Use `log` crate for logging (`log::info!`, `log::error!`, etc.)
- Use `parking_lot::Mutex` over `std::sync::Mutex`
- `Arc<dyn Pane>` for pane references in the mux

### Naming
- Crate names: `terminaler-*` (kebab-case)
- Binary names: `terminaler-gui`, `terminaler-daemon`
- Config keys: camelCase in JSON (matching WezTerm's existing convention)
- Rust identifiers: standard Rust conventions (snake_case for functions/variables, PascalCase for types)

### Error Handling
- Use `anyhow::Context` for adding context to errors
- Log errors before propagating when at system boundaries
- Never silently swallow errors

### Stripping Strategy
- Remove crates one at a time
- Full `cargo check` after each removal
- When removing a crate, first remove it from workspace Cargo.toml members
- Then remove references in dependent crates' Cargo.toml files
- Then fix compilation errors from missing imports/types

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
    "colors": {...}
}
```

## WezTerm Upstream Reference

- Repository: https://github.com/wez/wezterm
- Docs: https://wezfurlong.org/wezterm/
- License: MIT
- Forked from: main branch (shallow clone, 2026-02-21)

Cherry-pick terminal emulation bugfixes from upstream as needed. Do not attempt to stay in sync with feature development.
