# Claude Card Implementation Checklist

## Phase 1: Status Line Script (Data Source)

- [x] **1.1 Create `terminaler-statusline.sh`**
  - Location: `~/.claude/scripts/terminaler-statusline.sh`
  - Reads JSON from stdin, emits OSC 1337 SetUserVar sequences to `/dev/tty`
  - Also outputs visual status bar text to stdout (replaces old inline command)
  - Sets user vars: `claude_model`, `claude_status`, `claude_context_pct`, `claude_cost`, `claude_duration_ms`, `claude_lines_added`, `claude_lines_removed`, `claude_worktree`
  - Model name formatting: `claude-opus-4-6` → `opus-4.6 (1M)`

- [x] **1.2 Create `terminaler-hook.sh`**
  - Location: `~/.claude/scripts/terminaler-hook.sh`
  - Sets `claude_status` based on hook events:
    - `SessionStart` → `working` (+ model from hook data)
    - `Notification` → `waiting_input`
    - `Stop` → `idle`
    - Others → `working`

- [x] **1.3 Configure Claude Code settings**
  - `~/.claude/settings.json` updated:
    - `statusLine.command` → `terminaler-statusline.sh`
    - Added `SessionStart` hook → `terminaler-hook.sh`
    - Added `terminaler-hook.sh` to `Stop` and `Notification` hooks

- [ ] **1.4 Test in Terminaler**
  - Run Claude Code in Terminaler, verify sidebar card shows real data
  - Verify status transitions: working → waiting_input → idle

## Phase 2: Terminaler Sidebar — Per-Pane Claude Card (DONE)

- [x] **2.1 Data model: per-pane Claude info**
  - Changed `SidebarTabInfo.claude_info: Option<ClaudeSessionInfo>` → `pane_claude_info: HashMap<PaneId, ClaudeSessionInfo>`
  - File: `terminaler-gui/src/termwindow/mod.rs` (line ~203)

- [x] **2.2 Detection: check all panes, not just active pane**
  - `update_sidebar_info()` iterates `tab.iter_panes_ignoring_zoom()` and runs detection on every pane
  - File: `terminaler-gui/src/termwindow/render/tab_sidebar.rs` (lines ~74-138)

- [x] **2.3 Extract `build_claude_card_children()` helper**
  - Reusable function that builds the 5-line card element list (status, project+branch, context bar, stats)
  - Used by both tab-level (single-pane) and pane-level (multi-pane) rendering

- [x] **2.4 Tab-level rendering: single-pane Claude tabs**
  - Single-pane Claude tab: card renders at tab level with full layout
  - Multi-pane tab with Claude panes: tab title gets orange accent, no card at tab level

- [x] **2.5 Pane sub-entry rendering: Claude panes in multi-pane tabs**
  - Claude panes show full card (orange model title, status, project+branch, context bar, stats)
  - Normal panes show existing tree-connector layout
  - Orange left border accent on Claude panes

## Phase 3: Visual Polish

- [ ] **3.1 Verify card renders correctly with real data**
  - Run Claude Code with status line script configured
  - Single-pane tab: card at tab level with model name, status, context bar, stats
  - Multi-pane tab: card at pane sub-entry level for Claude pane, normal entry for other panes
  - Screenshot comparison against reference layout

- [ ] **3.2 Handle edge cases**
  - Claude session with no status line script (fallback: process name "claude", no stats)
  - Very long model names (truncation)
  - 0% context (before first API call)
  - Missing cost/duration (new session)
  - Session that ends (process exits but user vars linger until pane closes)

- [ ] **3.3 Context bar color thresholds**
  - Verify: < 70% dimmed, 70-89% yellow, ≥ 90% red
  - Check bar rendering with various percentages (0%, 50%, 78%, 95%, 100%)

## Phase 4: Status Detection Improvements (Future)

- [ ] **4.1 Hook-based status updates**
  - Claude Code hooks can set more granular status:
    - `SessionStart` → `working`
    - `PreToolUse` → `working`
    - `PostToolUse` → `working`
    - Idle timeout (no hook fires for N seconds) → `idle`
  - Requires a separate hook script that also emits OSC 1337 `claude_status` updates

- [ ] **4.2 "Needs input" detection**
  - Detect when Claude is waiting for user input (permission prompt, question)
  - Could use `PermissionRequest` hook event → set `claude_status=waiting_input`

- [ ] **4.3 Session end detection**
  - When Claude Code process exits, clear user vars or set `claude_status=idle`
  - Could detect via process name no longer being "claude"

- [ ] **4.4 Windows toast notification when Claude awaits input**
  - Fire a Windows 11 taskbar notification when a Claude pane goes idle (finished processing)
  - **Attempted approach**: Idle detection in `check_claude_idle_notifications()` (paint loop) tracks
    `pane_last_output` timestamps; when a Claude pane is idle for 3+ seconds after output, fires
    `persistent_toast_notification()`. Idle flag cleared on next `PaneOutput`.
  - **Blocker**: `ToastNotificationManager::CreateToastNotifierWithId("org.wezfurlong.terminaler")`
    fails silently — the app ID is not registered in the Windows Start Menu/registry. Without
    registration, Windows 11 drops toast notifications entirely.
  - **PowerShell fallback also failed** — spawning PowerShell for WinRT toast didn't produce visible
    notifications either; likely same app registration issue.
  - **To fix**: Either (a) register Terminaler as a Windows app with an AUMID (AppUserModelID) in
    the registry/Start Menu shortcut, or (b) use a different notification mechanism (e.g., Win32
    `Shell_NotifyIcon` balloon tips, or a system tray icon with `NOTIFYICONDATA`).
  - **Code in place** (disabled/unused):
    - `TermWindow::check_claude_idle_notifications()` in `mod.rs` — idle detection logic
    - `pane_last_output` / `claude_idle_notified` fields in `TermWindow`
    - `PaneOutput` handler updates timestamps in `mod.rs`
    - PowerShell fallback in `terminaler-toast-notification/src/windows.rs`

## Files Modified

| File | Changes |
|---|---|
| `terminaler-gui/src/termwindow/mod.rs` | `SidebarTabInfo` uses `pane_claude_info: HashMap<PaneId, ClaudeSessionInfo>` |
| `terminaler-gui/src/termwindow/render/tab_sidebar.rs` | Per-pane detection, `build_claude_card_children()` helper, pane-level card rendering |
| `~/.claude/terminaler-statusline.sh` | NEW — Status line script (external to codebase) |
| `~/.claude/settings.json` | Add `statusLine` config |

## Testing Commands

```bash
# Build
cargo build --bin terminaler-gui --target x86_64-pc-windows-gnu

# Test status line script with mock data
echo '{"model":{"id":"claude-opus-4-6","display_name":"Opus"},"context_window":{"used_percentage":78,"context_window_size":1000000},"cost":{"total_cost_usd":1.24,"total_duration_ms":720000,"total_lines_added":142,"total_lines_removed":38},"workspace":{"project_dir":"/mnt/e/project"}}' | ~/.claude/terminaler-statusline.sh

# Verify user vars are set (in a running Terminaler pane)
# The sidebar should show the Claude Card with real data
```
