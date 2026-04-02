# Implementation Checklist

## Phase 1: Documentation
- [x] Research document (`docs/custom-terminal-research.md`)
- [x] Protocol specification (`docs/json-protocol.md`)
- [x] Implementation checklist (`docs/implementation-checklist.md`)

## Phase 2: Server — JSON Renderer

### `json_render.rs` (new file)
- [x] Define `Span` struct with serde serialization
- [x] Define `CursorInfo` struct
- [x] Implement `color_to_string()` — ColorAttribute → CSS color string
- [x] Implement `srgba_to_hex()` — SrgbaTuple → "#rrggbb"
- [x] Implement `lines_to_spans()` — Line[] → Vec<Vec<Span>> with attribute grouping
- [x] Add `is_false()` / `is_zero()` serde skip helpers
- [x] Unit tests: empty lines, color conversion, serialization, cursor shapes (7 tests)

**Test**: `cargo test -p terminaler-web` — 9 tests pass

### `ws_session.rs` (modify)
- [x] Add `OutputFormat` enum (`Ansi`, `Json`)
- [x] Add `output_format` field to session state in `handle_ws()`
- [x] Parse `"format": "json"` from attach message
- [x] Implement `send_full_refresh_json()` — builds `screen` message with scrollback + viewport spans
- [x] Implement `send_delta_output_json()` — builds `screen_delta` message with changed row spans
- [x] Route to JSON functions when `output_format == Json` (via `send_refresh`/`send_delta` dispatchers)

### `server.rs` (modify)
- [x] Add `TERMINAL_HTML` const with `include_str!`
- [x] Add `/terminal` route
- [x] Add `terminal_handler` function (same token auth as `index_handler`)

### `lib.rs` (modify)
- [x] Add `pub mod json_render;`

**Test**: `cargo check -p terminaler-web` — compiles clean

## Phase 2: Client — `terminal.html`

### Core
- [x] HTML structure (app, tab-bar, term-container, viewport, scrollback, cursor)
- [x] CSS styling (Catppuccin Mocha dark theme, monospace font, flex layout)
- [x] WebSocket connection with auto-reconnect
- [x] Message handling (screen, screen_delta, pane_list, pane_removed)
- [x] DOM renderer (`spansToHTML`, `renderViewport`, `renderViewportRow`)
- [x] Tab bar with pane switching
- [x] 256-color palette generation
- [x] RAF-batched delta updates with merge

### Input
- [x] Hidden textarea for keyboard capture
- [x] Key mapping (arrows, Home/End, PgUp/PgDn, Ctrl combos, Enter, Backspace, Tab, Escape, F1-F12)
- [x] Paste handling
- [x] On-screen keyboard (collapsible, two rows + sticky modifiers)
- [x] Sticky Ctrl/Alt modifiers with auto-clear after keypress

### Layout & Resize
- [x] Character dimension probe element
- [x] Binary search font fitting (target 80 cols)
- [x] Resize event with debounce
- [x] Virtual keyboard handling (visualViewport API)
- [x] Auto-scroll to bottom (unless user scrolled up)
- [x] Scrollback rendering

### Mobile
- [x] `100dvh` layout
- [x] `env(safe-area-inset-*)` padding
- [x] Touch-friendly button sizes (38px min-height)
- [x] Virtual keyboard detection and resize

## Phase 2: Testing
- [x] `cargo check -p terminaler-web` — compiles
- [x] `cargo test -p terminaler-web` — 9 tests pass
- [ ] Side-by-side: `/` (xterm.js) vs `/terminal` (custom)
  - [ ] `ls --color` — colors match
  - [ ] `git log --graph --oneline` — bold, colors, special chars
  - [ ] Typing, arrow keys, Ctrl+C, Tab completion
  - [ ] Multiple panes, tab switching
  - [ ] Browser resize → cols/rows update
  - [ ] Scrollback works with native scroll

## Phase 3: Claude Code Mode (future)
- [ ] Pattern detection on received spans
- [ ] Enhanced rendering for code blocks, headers, tool calls, diffs
- [ ] Toggle button per tab

## Phase 4: Integration (future)
- [ ] Replace `index.html` content
- [ ] Remove xterm.js/css assets
- [ ] Update routes in `server.rs`

## Known Risks
1. **Performance**: Large scrollback (1000 lines) as JSON could be large. Mitigation: spans group consecutive cells, reducing object count significantly.
2. **Full-screen apps**: vim/htop rely on cursor positioning which works differently in DOM. Mitigation: viewport is always `rows` divs, cursor is absolutely positioned.
3. **Wide characters**: CJK characters occupy 2 cells. Mitigation: span text includes the character as-is; CSS handles width via monospace font.
