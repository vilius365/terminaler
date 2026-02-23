<p align="center">
  <img src="https://img.shields.io/badge/platform-Windows-0078d4?style=flat-square&logo=windows" alt="Windows" />
  <img src="https://img.shields.io/badge/language-Rust-dea584?style=flat-square&logo=rust" alt="Rust" />
  <img src="https://img.shields.io/badge/renderer-wgpu-4fc08d?style=flat-square" alt="wgpu" />
  <img src="https://img.shields.io/badge/license-MIT-blue?style=flat-square" alt="MIT License" />
</p>

# Terminaler

A **Windows-native GPU-accelerated terminal multiplexer** with snap layouts, workspace templates, session persistence, and remote web access. Built in Rust, powered by wgpu.

Forked from [WezTerm](https://github.com/wez/wezterm) — stripped to Windows-only, rebuilt with opinionated defaults for developers who live in the terminal.

## Why Terminaler?

Most terminal multiplexers are Unix-first, Windows-second. Terminaler flips that:

- **Windows-native** — ConPTY, native window management, `%APPDATA%` config. No WSL required to run (but WSL shells work great inside it).
- **Snap layouts** — 8 built-in presets. Pick a layout, get your panes instantly. No manual splitting.
- **Workspace templates** — Predefined environments that open the right shells in the right directories with the right layout. Jump into "Claude Code" or "Full-stack dev" with one shortcut.
- **Session persistence** — Close the window, reopen it, get your tabs and panes back. Sessions serialize to JSON automatically.
- **Remote web access** — Access your terminal sessions from any browser on your LAN via a built-in web server (xterm.js + WebSocket).
- **JSON config** — No scripting language to learn. One `terminaler.json` file with JSONC support (comments allowed).
- **GPU-rendered** — wgpu backend for smooth, tear-free rendering at any font size.

## Quick Start

### Build from Source

```bash
# Clone
git clone https://github.com/vilius365/terminaler.git
cd terminaler

# Build (requires Rust stable toolchain)
cargo build --release

# Run
cargo run --release --bin terminaler-gui
```

### Cross-compile for Windows (from WSL/Linux)

```bash
# Install MinGW cross-compiler
sudo dnf install mingw64-gcc mingw64-winpthreads-static  # Fedora
# or: sudo apt install gcc-mingw-w64-x86-64              # Ubuntu

# Build
cargo build --release --target x86_64-pc-windows-gnu
```

The binary lands in `target/x86_64-pc-windows-gnu/release/terminaler-gui.exe`.

## Architecture

```
terminaler-gui.exe           terminaler-daemon.exe
  (GPU client)        <--->    (background process)
       |              Named         |
  Renders panes       Pipe     Holds PTY sessions
  Handles input                Manages mux state
  Snap layout UI               Persists sessions
                                Web server (optional)
```

**Two-process model**: the GUI renders and handles input, the daemon holds PTY sessions and multiplexer state. Communication over Windows named pipes. This means your terminal sessions survive GUI restarts.

## Snap Layouts

Pick a layout with `Ctrl+Shift+L`:

```
 single         hsplit          vsplit       triple-right
┌──────────┐  ┌─────┬─────┐  ┌───────────┐  ┌─────┬─────┐
│          │  │     │     │  │           │  │     │  2  │
│          │  │     │     │  ├───────────┤  │  1  ├─────┤
│          │  │     │     │  │           │  │     │  3  │
└──────────┘  └─────┴─────┘  └───────────┘  └─────┴─────┘

triple-bottom      quad            dev        claude-code
┌───────────┐  ┌────┬────┐  ┌──────┬─────┐  ┌───────────┐
│           │  │  1 │  2 │  │      │  2  │  │           │
├─────┬─────┤  ├────┼────┤  │  1   ├─────┤  │     1     │
│     │     │  │  3 │  4 │  │      │  3  │  ├───────────┤
└─────┴─────┘  └────┴────┘  └──────┴─────┘  │     2     │
                                             └───────────┘
```

Define custom layouts in your config:

```jsonc
{
    "layouts": {
        "custom": [
            {
                "name": "my-layout",
                "description": "Editor + two terminals",
                "root": {
                    "type": "split",
                    "direction": "horizontal",
                    "ratio": 0.6,
                    "first": { "type": "pane" },
                    "second": {
                        "type": "split",
                        "direction": "vertical",
                        "ratio": 0.5,
                        "first": { "type": "pane", "command": "pwsh" },
                        "second": { "type": "pane", "command": "wsl" }
                    }
                }
            }
        ]
    }
}
```

## Workspace Templates

Workspaces combine a layout with shell commands and working directories:

```jsonc
{
    "workspaces": [
        {
            "name": "Claude Code",
            "layout": "claude-code",
            "panes": [
                { "command": "claude", "cwd": "~/projects" },
                { "command": "pwsh", "cwd": "~/projects" }
            ]
        }
    ]
}
```

Open the workspace picker with `Ctrl+Shift+O`.

## Remote Web Access

Access your terminal sessions from any browser on your network:

```jsonc
{
    "webAccess": {
        "enabled": true,
        "bindAddress": "0.0.0.0:9876"
    }
}
```

Navigate to `http://<your-ip>:9876` and authenticate with the auto-generated token (stored in `%APPDATA%\Terminaler\web-token`). Built on xterm.js with full WebSocket streaming.

## Configuration

Config file: `%APPDATA%\Terminaler\terminaler.json` (auto-generated on first run)

```jsonc
{
    // Shell profiles
    "profiles": [
        { "name": "PowerShell", "command": "pwsh.exe" },
        { "name": "WSL", "command": "wsl.exe" }
    ],

    // Appearance
    "theme": "dark",   // "dark" | "light" | "custom"
    "font": {
        "family": "Cascadia Code",
        "size": 12
    },

    // Keybindings
    "keybindings": [
        { "keys": "ctrl+shift+l", "id": "Terminaler.SnapLayoutPicker" },
        { "keys": "ctrl+shift+o", "id": "Terminaler.WorkspacePicker" },
        { "keys": "ctrl+shift+f", "id": "Terminaler.Search" }
    ]
}
```

Dark theme uses **Catppuccin Mocha**. Light theme also included. Full color customization via the `colors` key.

## Crate Map

| Crate | Purpose |
|-------|---------|
| `terminaler-gui` | Main GUI binary — window management, GPU rendering, input |
| `terminaler-mux-server` | Background daemon — PTY session host |
| `terminaler-layout` | Snap layout engine — declarative tree to split operations |
| `terminaler-web` | Web access server — axum + xterm.js |
| `config` | JSON configuration system |
| `mux` | Multiplexer core — tabs, panes, domains |
| `term` | Terminal emulator — VT parser, cell grid, scrollback |
| `window` | Platform window abstraction (Windows backend) |
| `terminaler-font` | Font discovery, HarfBuzz shaping, FreeType rasterization |
| `bintree` | Binary tree with zipper cursor — pane layout data structure |
| `pty` | PTY abstraction (ConPTY on Windows) |
| `codec` | Client-server protocol codec |

## Contributing

Contributions are welcome! Whether you're fixing a typo or building a major feature, we appreciate the help.

### Good First Issues

- **More snap layout presets** — Add layouts in `terminaler-layout/src/lib.rs` (just Rust structs, no GUI work needed)
- **Theme presets** — Add color schemes in `config/src/themes.rs`
- **Config validation** — Better error messages for invalid `terminaler.json`
- **Documentation** — Usage guides, screenshots, GIFs

### Bigger Projects

- **Native Windows installer** — WiX or NSIS-based `.msi` / `.exe` installer
- **Tab drag-and-drop** — Reorder tabs by dragging
- **Pane resize with mouse** — Drag pane borders to resize
- **Command palette** — Fuzzy-find actions (`Ctrl+Shift+P` style)
- **Shell integration** — Detect CWD from shell, show in tab title
- **Plugin system** — Lightweight extension points for custom behavior

### Development Setup

1. Install [Rust](https://rustup.rs/) (stable channel)
2. Clone the repo
3. `cargo build` — first build ~5 min, incremental builds ~30s
4. `cargo test` — runs the test suite
5. On Windows: `cargo run --bin terminaler-gui` to launch

Cross-compiles from Linux/WSL to Windows using MinGW. See `.cargo/config.toml` for linker config.

### Code Style

- Standard `rustfmt` formatting
- `anyhow::Result` for error propagation
- `log` crate macros for logging
- `parking_lot::Mutex` over `std::sync::Mutex`

## Acknowledgments

Terminaler is built on the foundation of [WezTerm](https://github.com/wez/wezterm) by [@wez](https://github.com/wez). The terminal emulator core, GPU rendering pipeline, font system, and multiplexer architecture all originate from WezTerm. Thank you for the incredible work and the MIT license that makes projects like this possible.

## License

MIT — see [LICENSE.md](LICENSE.md)
