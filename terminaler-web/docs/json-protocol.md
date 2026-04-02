# JSON Terminal Protocol Specification

## Overview

Extension to the existing WebSocket protocol that sends terminal content as structured JSON spans instead of raw ANSI escape sequences. The client opts in via the `attach` message; the default remains ANSI mode for backward compatibility.

## Format Negotiation

### Attach with format (Client → Server)
```json
{"type": "attach", "pane_id": 1, "format": "json"}
```

- `format` field is optional. Omitted or `"ansi"` → ANSI mode (existing behavior).
- `"json"` → JSON span mode (new messages below).

## Message Types

### Server → Client

#### `screen` — Full refresh
Sent on attach, after resize, or when reconnecting.

```json
{
    "type": "screen",
    "pane_id": 1,
    "scrollback": [
        [{"t": "user@host", "fg": "#a6e3a1", "b": true}, {"t": ":~$ ls"}],
        [{"t": "file.txt"}]
    ],
    "viewport": [
        [{"t": "$ ", "fg": "#a6e3a1", "b": true}, {"t": "echo hello"}],
        [{"t": "hello"}],
        []
    ],
    "cursor": {"x": 0, "y": 2, "shape": "block", "visible": true},
    "cols": 80,
    "rows": 24
}
```

| Field | Type | Description |
|-------|------|-------------|
| `scrollback` | `Span[][]` | Array of lines above the viewport (up to 1000) |
| `viewport` | `Span[][]` | Array of viewport lines (exactly `rows` entries) |
| `cursor` | `CursorInfo` | Cursor position and shape |
| `cols` | `number` | Terminal column count |
| `rows` | `number` | Terminal row count |

#### `screen_delta` — Delta update
Sent when terminal output changes. Only includes changed viewport rows.

```json
{
    "type": "screen_delta",
    "pane_id": 1,
    "lines": {
        "3": [{"t": "output line"}],
        "4": [{"t": "$ ", "fg": "#a6e3a1"}, {"t": "next command"}]
    },
    "cursor": {"x": 14, "y": 4, "shape": "block", "visible": true}
}
```

| Field | Type | Description |
|-------|------|-------------|
| `lines` | `{[row: string]: Span[]}` | Map of changed row indices (0-based, viewport-relative) to span arrays |
| `cursor` | `CursorInfo` | Updated cursor position |

#### `pane_list` — Pane enumeration (unchanged)
```json
{"type": "pane_list", "panes": [{"id": 0, "title": "bash", "cols": 80, "rows": 24}]}
```

#### `pane_removed` — Pane closed (unchanged)
```json
{"type": "pane_removed", "pane_id": 0}
```

### Client → Server (unchanged)

#### `list_panes`
```json
{"type": "list_panes"}
```

#### `attach`
```json
{"type": "attach", "pane_id": 1, "format": "json"}
```

#### `input`
```json
{"type": "input", "pane_id": 1, "data": "ls\r"}
```

#### `paste`
```json
{"type": "paste", "pane_id": 1, "data": "pasted text"}
```

#### `resize`
```json
{"type": "resize", "pane_id": 1, "cols": 120, "rows": 40}
```

## Data Structures

### Span
Represents a run of text with uniform styling. All style fields are optional and omitted when at default values (minimizes JSON size).

```typescript
interface Span {
    t: string;          // Text content (required)
    fg?: string;        // Foreground color
    bg?: string;        // Background color
    b?: boolean;        // Bold (true if bold)
    d?: boolean;        // Dim/half-brightness
    i?: boolean;        // Italic
    u?: number;         // Underline: 0=none, 1=single, 2=double, 3=curly, 4=dotted, 5=dashed
    s?: boolean;        // Strikethrough
    r?: boolean;        // Reverse video
    o?: boolean;        // Overline
}
```

### CursorInfo
```typescript
interface CursorInfo {
    x: number;          // Column (0-based)
    y: number;          // Row (0-based, viewport-relative)
    shape: string;      // "block", "bar", or "underline"
    visible: boolean;   // Whether cursor should be rendered
}
```

## Color Encoding

| Format | Example | Meaning |
|--------|---------|---------|
| `#rrggbb` | `"#a6e3a1"` | True color (24-bit RGB) |
| `p{idx}` | `"p1"` | Palette index (0-255) |
| Omitted | — | Default foreground/background |

Palette indices 0-15 map to the standard ANSI colors. Indices 16-231 are the 6x6x6 color cube. Indices 232-255 are the grayscale ramp. The client maintains its own palette lookup table.

## Backward Compatibility

- The existing xterm.js client (`/`) continues to receive ANSI `output` messages — no changes.
- JSON format is only activated when the client sends `"format": "json"` in the `attach` message.
- Both clients can be connected simultaneously to different panes.
- The `OutputFormat` is per-session state, not per-server configuration.
