# Contributing to Terminaler

Thanks for considering contributing! Whether you're fixing a typo or building a major feature, we appreciate the help.

## Getting Started

### Prerequisites

- [Rust](https://rustup.rs/) stable toolchain
- For Windows cross-compilation from WSL/Linux: MinGW (`mingw64-gcc`, `mingw64-winpthreads-static`)

### Build & Run

```bash
# Build
cargo build

# Run (GUI)
cargo run --bin terminaler-gui

# Run tests
cargo test

# Cross-compile for Windows (from WSL/Linux)
cargo build --target x86_64-pc-windows-gnu
```

### Iterating

Use `cargo check` for fast type-checking during development:

```bash
cargo check
```

For a debug build with backtraces:

```bash
RUST_BACKTRACE=1 cargo run --bin terminaler-gui
```

## Where to Find Things

| Directory | What's There |
|-----------|-------------|
| `terminaler-gui/` | Main GUI binary — window management, GPU rendering, input handling |
| `terminaler-layout/` | Snap layout engine — layout presets, workspace templates |
| `terminaler-web/` | Remote web access server (axum + xterm.js) |
| `config/` | JSON configuration system |
| `mux/` | Multiplexer core — tabs, panes, domains |
| `term/` | Terminal emulator — VT parser, cell grid, scrollback |
| `window/` | Platform window abstraction (Windows backend) |
| `terminaler-font/` | Font discovery, shaping (HarfBuzz), rasterization (FreeType) |
| `bintree/` | Binary tree with zipper cursor — pane layout data structure |
| `pty/` | PTY abstraction (ConPTY on Windows) |

See [CLAUDE.md](CLAUDE.md) for the full crate map and architecture overview.

## Good First Issues

- **More snap layout presets** — Add layouts in `terminaler-layout/src/lib.rs` (just Rust structs, no GUI work needed)
- **Theme presets** — Add color schemes in `config/src/themes.rs`
- **Config validation** — Better error messages for invalid `terminaler.json`

## Bigger Projects

- **Native Windows installer** — WiX or NSIS-based `.msi` / `.exe` installer
- **Tab drag-and-drop** — Reorder tabs by dragging
- **Pane resize with mouse** — Drag pane borders to resize
- **Command palette** — Fuzzy-find actions (`Ctrl+Shift+P` style)
- **Shell integration** — Detect CWD from shell, show in tab title
- **Plugin system** — Lightweight extension points for custom behavior

## Code Style

- Standard `rustfmt` formatting — run `cargo fmt --all` before submitting
- `anyhow::Result` for error propagation
- `log` crate macros for logging (`log::info!`, `log::error!`)
- `parking_lot::Mutex` over `std::sync::Mutex`
- snake_case for functions/variables, PascalCase for types
- camelCase for JSON config keys

## Before Submitting a Pull Request

```bash
cargo fmt --all          # Format code
cargo check              # Type-check
cargo test               # Run tests
```

## Please Include

- **Tests** to cover your changes (see existing tests in each crate for patterns)
- **Documentation** if you're adding or changing behavior — even rough notes help

## Submitting

1. Fork the repo and create a feature branch
2. Make your changes
3. Ensure `cargo fmt`, `cargo check`, and `cargo test` pass
4. Open a pull request with a clear description of the change

If you're new to GitHub Pull Requests, see [Creating a Pull Request](https://help.github.com/articles/creating-a-pull-request/).
