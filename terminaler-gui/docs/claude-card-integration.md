# Claude Card — Status Line & Hook Integration

## Overview

The Claude Card in Terminaler's tab sidebar displays per-pane Claude Code session metadata: model, status, context usage, cost, duration, and code change stats.

The integration uses two Claude Code features to populate terminal user variables via OSC 1337:
1. **Status line script** — receives session telemetry JSON, sets data vars (model, cost, context%, etc.)
2. **Hook scripts** — react to lifecycle events, set status var (working, awaiting input, idle)

## Configuration Location

All configuration lives in **user-level** settings (`~/.claude/settings.json`), not project-level. This is intentional — the terminal integration applies to every Claude Code session regardless of project.

Scripts are stored in `~/.claude/scripts/`:

| File | Purpose |
|---|---|
| `~/.claude/scripts/terminaler-statusline.sh` | Status line script — sets data vars (model, cost, context, lines) |
| `~/.claude/scripts/terminaler-hook.sh` | Hook script — sets `claude_status` based on lifecycle events |

## Data Flow

```
Claude Code session (in a pane)
  │
  ├─ Status line script (after each assistant message, debounced 300ms)
  │   ├─ Receives full session JSON via stdin
  │   ├─ Emits OSC 1337 SetUserVar to /dev/tty (data only, not status)
  │   └─ Outputs visual status bar text to stdout
  │
  ├─ Hook scripts (on lifecycle events)
  │   ├─ SessionStart       → claude_status = "working"
  │   ├─ PreToolUse          → claude_status = "working"
  │   ├─ PostToolUse         → claude_status = "working"
  │   ├─ PermissionRequest   → claude_status = "waiting_input"
  │   ├─ Notification        → claude_status = "waiting_input"
  │   └─ Stop                → claude_status = "idle"
  │
  ├─ Terminal emulator stores user vars per-pane
  │
  └─ Terminaler polls pane.copy_user_vars() every 2s
      └─ Renders Claude Card in sidebar
```

### Why Status Is Hook-Only

The status line script does NOT set `claude_status`. Status is managed exclusively by hooks because:
- The status line script fires after each response completes (debounced 300ms) and would overwrite hook-set statuses
- Hooks fire at precise lifecycle moments (tool call starts, permission requested, session ends)
- Separating data (status line) from status (hooks) prevents race conditions

## settings.json Configuration

Add these entries to `~/.claude/settings.json`:

```jsonc
{
  "statusLine": {
    "type": "command",
    "command": "/home/vilius/.claude/scripts/terminaler-statusline.sh"
  },
  "hooks": {
    // Add terminaler-hook.sh to ALL these events:
    "SessionStart": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "/home/vilius/.claude/scripts/terminaler-hook.sh",
            "timeout": 2000
          }
        ]
      }
    ],
    "PreToolUse": [
      {
        // No matcher — fires for all tools
        "hooks": [
          {
            "type": "command",
            "command": "/home/vilius/.claude/scripts/terminaler-hook.sh",
            "timeout": 2000
          }
        ]
      }
    ],
    "PostToolUse": [
      {
        // No matcher — fires for all tools
        "hooks": [
          {
            "type": "command",
            "command": "/home/vilius/.claude/scripts/terminaler-hook.sh",
            "timeout": 2000
          }
        ]
      }
    ],
    "PermissionRequest": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "/home/vilius/.claude/scripts/terminaler-hook.sh",
            "timeout": 2000
          }
        ]
      }
    ],
    "Notification": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "/home/vilius/.claude/scripts/terminaler-hook.sh",
            "timeout": 2000
          }
        ]
      }
    ],
    "Stop": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "/home/vilius/.claude/scripts/terminaler-hook.sh",
            "timeout": 2000
          }
        ]
      }
    ]
  }
}
```

**Note:** These hook entries can coexist with other hooks on the same events. Each event supports an array of hook groups — append the terminaler entries alongside existing ones.

## Status Line JSON Schema

The status line script receives this JSON via stdin:

```jsonc
{
  "model": {
    "id": "claude-opus-4-6",           // API model ID
    "display_name": "Opus"             // Short display name
  },
  "context_window": {
    "total_input_tokens": 15234,
    "total_output_tokens": 4521,
    "context_window_size": 200000,     // Max context window
    "used_percentage": 8,              // 0-100
    "remaining_percentage": 92,
    "current_usage": {                 // null before first API call
      "input_tokens": 8500,
      "output_tokens": 1200,
      "cache_creation_input_tokens": 5000,
      "cache_read_input_tokens": 2000
    }
  },
  "cost": {
    "total_cost_usd": 0.01234,
    "total_duration_ms": 45000,
    "total_api_duration_ms": 2300,
    "total_lines_added": 156,
    "total_lines_removed": 23
  },
  "workspace": {
    "current_dir": "/mnt/e/project",
    "project_dir": "/mnt/e/project"
  },
  "worktree": {                        // Optional — only during worktree sessions
    "name": "my-feature",
    "path": "/path/to/.claude/worktrees/my-feature",
    "branch": "worktree-my-feature",
    "original_cwd": "/path/to/project",
    "original_branch": "main"
  },
  "session_id": "abc123...",
  "cwd": "/current/working/directory",
  "version": "1.0.80",
  "exceeds_200k_tokens": false
}
```

**Optional fields** (may be absent):
- `worktree` — only during `--worktree` sessions
- `context_window.current_usage` — null before first API call

## User Variables

### Data Variables (set by status line script)

| User Variable | Source | Example | Notes |
|---|---|---|---|
| `claude_model` | `model.id` + `context_window_size` | `opus 4.6 (1M)` | Formatted model name |
| `claude_context_pct` | `context_window.used_percentage` | `78` | Integer 0-100 |
| `claude_cost` | `cost.total_cost_usd` | `1.24` | Float, USD |
| `claude_duration_ms` | `cost.total_duration_ms` | `720000` | Milliseconds |
| `claude_lines_added` | `cost.total_lines_added` | `142` | Integer |
| `claude_lines_removed` | `cost.total_lines_removed` | `38` | Integer |
| `claude_worktree` | `worktree.name` | `my-feature` | Only set when in worktree |

### Status Variable (set by hooks only)

| Value | When Set | Display | Color |
|---|---|---|---|
| `working` | `SessionStart`, `PreToolUse`, `PostToolUse` | ▶ working | Green |
| `waiting_input` | `PermissionRequest`, `Notification` | ● awaiting input | Yellow |
| `idle` | `Stop` | ✔ idle | Gray |
| `error` | (reserved for future use) | ✗ error | Red |
| _(not set)_ | Before first hook fires | ▶ active | Green |

### Model Name Formatting

The `model.id` is transformed by the status line script:
- `claude-opus-4-6` → `opus 4.6`
- `claude-opus-4-6[1m]` → `opus 4.6` (brackets stripped)
- `claude-haiku-4-5-20251001` → `haiku 4.5` (date suffix stripped)

Context window size suffix appended:
- `≥ 900,000` tokens → `(1M)`
- `≥ 180,000` tokens → `(200K)`
- smaller → no suffix

## OSC 1337 SetUserVar Protocol

Both scripts emit user variables by writing OSC 1337 sequences directly to `/dev/tty` (bypassing stdout capture by Claude Code):

```
ESC ] 1337 ; SetUserVar=NAME=VALUE BEL
```

- `NAME`: plain text variable name (e.g. `claude_model`)
- `VALUE`: **base64-encoded** value
- `BEL`: `\007`

```bash
printf '\033]1337;SetUserVar=%s=%s\007' "claude_model" "$(echo -n 'opus 4.6 (1M)' | base64)" > /dev/tty
```

Writing to `/dev/tty` is required because:
- Hook stdout is captured by Claude Code (used for blocking responses)
- Status line stdout is captured by Claude Code (rendered as the status bar)
- `/dev/tty` writes directly to the terminal's PTY, where the escape sequence is interpreted

Terminaler (inheriting WezTerm's terminal emulation) supports OSC 1337 natively. Values are stored per-pane and accessible via `pane.copy_user_vars()`.

## Claude Card Visual Layout

```
  opus 4.6 (1M)              [X]    ← orange title (model name)
  ▶ working                         ← status (green)
  terminaler  main                 ← project + branch (dimmed)
  ████████████░░░ 78%                ← context bar (color-coded)
  $1.24 · 12m · +142 -38            ← stats (dimmed)
```

### Color Coding

| Element | Color (LinearRGBA) |
|---|---|
| Model name | Orange `(1.0, 0.7, 0.2)` |
| Status: working | Green `(0.3, 0.8, 0.4)` |
| Status: awaiting input | Yellow `(1.0, 0.8, 0.2)` |
| Status: idle | Gray `(0.5, 0.5, 0.5)` |
| Status: error | Red `(1.0, 0.3, 0.3)` |
| Context bar < 70% | Dimmed |
| Context bar 70-89% | Yellow |
| Context bar ≥ 90% | Red |
| Project/branch, stats | Dimmed (60% text color) |
| Left border accent | Orange `(1.0, 0.6, 0.1)` |

## Terminaler Detection Logic

Terminaler detects Claude Code panes using three signals (any one is sufficient):

1. **Process detection**: Foreground process name is `claude` or `claude-code`
2. **Title detection**: Pane title contains "claude code", "claude-code", or starts with "claude"
3. **User vars**: Any user variable key starts with `claude_`

Detection runs on **all panes** in each tab every 2 seconds, not just the active pane. This enables per-pane Claude Cards in split layouts.

If process/title detection succeeds but no user vars are set (status line script not configured), the card shows with defaults: model "claude", status "active", CWD from pane.

## Troubleshooting

**Card shows "claude" instead of model name**: Status line script isn't running or `/dev/tty` isn't writable. Check `~/.claude/settings.json` has the `statusLine` entry and the script is executable (`chmod +x`).

**Status stuck on one value**: Check that all hook events are registered in `settings.json`. Run `jq .hooks ~/.claude/settings.json` to verify.

**No card appears**: Verify Claude Code is the foreground process. Check with `pane.get_foreground_process_name()` — the process name must match `claude` or `claude-code`.

**User vars not updating**: Terminaler polls every 2 seconds. The sidebar must be visible (tab sidebar enabled in config). Force refresh by resizing the sidebar.
