# Repository Guidelines

## Project Structure & Module Organization
This repository is a Rust workspace. Main user-facing binaries live in `terminaler-gui/`, `terminaler-mux-server/`, and `terminaler-cli/`. Core runtime crates include `mux/`, `term/`, `pty/`, `window/`, `config/`, `terminaler-layout/`, and `terminaler-web/`. Shared support crates such as `terminaler-font/`, `termwiz/`, `codec/`, and `bintree/` sit at the workspace root. Static assets are under `assets/`, test fixtures under `test-data/`, and automation scripts under `ci/`.

## Build, Test, and Development Commands
Use Cargo from the workspace root:

```bash
cargo check                         # Fast type-check across the workspace
cargo build                         # Debug build
cargo run --bin terminaler-gui      # Launch the GUI client
cargo test                          # Run unit and integration tests
cargo fmt --all                     # Apply standard Rust formatting
cargo build --target x86_64-pc-windows-gnu
```

Use `RUST_BACKTRACE=1 cargo run --bin terminaler-gui` when debugging crashes. Prefer Cargo commands over the upstream-derived `Makefile` targets.

## Coding Style & Naming Conventions
Follow `rustfmt` with the repo’s `.rustfmt.toml`: 4-space indentation, standard Rust formatting, and module-granular imports. Use `snake_case` for functions and modules, `PascalCase` for types and enums, and `camelCase` for JSON config keys in `config/`. Prefer `anyhow::Result` with `anyhow::Context` for fallible paths, `log` macros for diagnostics, and `parking_lot::Mutex` over `std::sync::Mutex` where shared state already follows that pattern.

## Testing Guidelines
Keep tests close to the code they verify. Existing patterns include crate-local integration tests in `*/tests/` and module tests such as `term/src/test/`. Name tests for the behavior being verified, for example `restores_session_state` or `parses_osc_777_notification`. Run `cargo test` before opening a PR; if you touch layout, mux, or parser code, add or update targeted tests rather than relying on manual GUI checks.

## Commit & Pull Request Guidelines
Recent history uses short, imperative, sentence-style subjects such as `Fix crash on multi-byte UTF-8 characters...` and `Per-pane scrollbars, hide/unhide panes, and freeze diagnostics`. Follow that style, keep commits focused, and branch from the active release branch instead of committing directly to `main` or `release`. PRs should include a clear summary, linked issue when available, testing notes, and screenshots/GIFs for visible GUI changes.

## Configuration & Security Notes
Runtime config lives in `%APPDATA%\\Terminaler\\terminaler.json` and supports JSONC comments. Do not commit secrets, tokens, or machine-specific config. When changing web access or session persistence behavior, document any new ports, files, or auth requirements in `README.md`.
