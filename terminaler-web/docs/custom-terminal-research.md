# Custom Web Terminal — Research Document

## Overview

This document captures the research and design rationale for replacing the xterm.js-based web terminal client with a custom-built, server-side rendered HTML/CSS terminal.

## Approaches Surveyed

### 1. Client-side ANSI Parsing (Current: xterm.js)
- **How it works**: Server sends raw ANSI escape sequences; xterm.js parses them client-side using a full VT100/VT220 state machine, maintains its own cell grid, and renders via Canvas or WebGL.
- **Pros**: Full terminal emulation fidelity, handles alt-screen apps (vim, htop), good performance with WebGL renderer.
- **Cons**: Large dependency (~200KB minified), duplicates terminal state (server already has the cell grid), complex to customize, overkill for a remote viewer.

### 2. Other Client-side Libraries
- **hterm** (Google): Similar approach to xterm.js, used by Chrome's Secure Shell. Also client-side parsing.
- **terminal.js**, **term.js**: Smaller but unmaintained. Same fundamental approach.
- **Limitation**: All client-side parsers duplicate the terminal state that already exists on the server.

### 3. Server-side Rendering to HTML
- **How it works**: Server converts its existing cell grid to HTML (or structured data), sends pre-rendered content. Client simply inserts into DOM.
- **Examples**: Claude mobile app uses a "Remote Control" pattern — the terminal UI is a synchronized view of server state, not an independent emulator.
- **Pros**: No client-side parsing needed, tiny client code, server is single source of truth, easy to add custom rendering (Claude Code mode), works on any browser.
- **Cons**: Higher bandwidth for full refreshes (mitigated by delta updates), no client-side alt-screen buffer.

### 4. Server-side JSON Spans (Our Choice)
- **How it works**: Server converts `Line`/`Cell` data to JSON arrays of styled spans. Client renders spans as `<div>` lines with `<span>` elements. Delta updates send only changed rows.
- **Why JSON over HTML**: Structured data is easier to process client-side (e.g., for Claude Code mode pattern detection), smaller wire format, easier to extend.

## Decision Rationale

We chose **server-side JSON rendering** because:

1. **No duplicated state**: The server (mux crate) already maintains the complete terminal cell grid. Sending structured data to the client avoids re-parsing ANSI sequences that were already parsed.

2. **Lightweight client**: The entire client is a single HTML file (~15KB) vs xterm.js (~200KB). No build step, no npm dependencies.

3. **Extensibility**: JSON spans are easy to post-process for Claude Code mode (pattern detection on structured data vs. parsing rendered terminal output).

4. **Mobile-friendly**: Simple DOM rendering works well on mobile browsers. CSS handles font sizing, touch scrolling is native.

5. **Backward compatible**: The existing ANSI mode (`/` route with xterm.js) continues to work. The new client is served at `/terminal` and opts into JSON format via the attach message.

## Current Architecture Analysis

### `ansi_render.rs`
- `lines_to_ansi()`: Iterates `Line::visible_cells()`, compares `CellAttributes` between consecutive cells, emits SGR escape codes for changes.
- `full_refresh_with_scrollback()`: Renders scrollback as flowing text + viewport with cursor positioning.
- Key pattern: Cell iteration → attribute comparison → output emission. We mirror this exactly but output `Span` structs instead of SGR codes.

### `ws_session.rs`
- `handle_ws()`: Main WebSocket loop with `tokio::select!` over client messages and mux broadcast notifications.
- `send_full_refresh()`: Dispatches to smol main thread, gathers scrollback + viewport lines, calls `ansi_render::full_refresh_with_scrollback()`, sends as `{"type":"output","data":"..."}`.
- `send_delta_output()`: Gets changed lines since last seqno, renders with `ansi_render::lines_to_ansi()`, sends as `{"type":"output","data":"..."}`.
- **Extension point**: Add `OutputFormat` enum to session state. When format is JSON, call `json_render` functions instead. New message types `screen` and `screen_delta` carry structured data.

### `server.rs`
- Simple axum router: `/` (index.html), `/xterm.min.js`, `/xterm.css`, `/ws`.
- Add `/terminal` route serving `terminal.html` with same token auth.

## Key References

- WezTerm terminal emulator: https://github.com/wez/wezterm
- xterm.js: https://xtermjs.org/
- Catppuccin Mocha color scheme: https://github.com/catppuccin/catppuccin
- CSS `dvh` units for mobile viewport: https://web.dev/blog/viewport-units
- `visualViewport` API: https://developer.mozilla.org/en-US/docs/Web/API/VisualViewport
