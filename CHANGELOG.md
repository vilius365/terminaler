# Changelog

## 2026-06-15

### Added
- **Manage Worktrees** action (`ctrl+shift+w`): overlay listing the current repo's git worktrees with dirty/clean state, offering two lifecycle actions per worktree — **merge & remove** (`m`: merges the branch into the main worktree, then removes the worktree and deletes the now-merged branch; refuses if the worktree is dirty) and **discard** (`d`: force-removes the worktree and deletes its branch). Both require explicit `y` confirmation and surface dirty state first; the main worktree cannot be removed. Completes the P1 worktree-orchestration lifecycle.

## 2026-06-11

### Added
- **New Claude Agent** action (`ctrl+shift+a`): overlay prompts for a branch name, creates a git worktree (`<repo-parent>/<repo>-worktrees/<branch>` by default), and spawns a tab from the `claude-code` workspace template with `claude` running in the main pane — GUI-native equivalent of `claude --worktree --tmux`
- **Claude Agent Dashboard** (`ctrl+shift+j`): fuzzy-searchable overlay listing every Claude pane across all windows and workspaces (status, model, worktree, branch, context %, cost), sorted waiting-input first; Enter jumps to the pane, switching workspaces if needed
- `claude_agent` config section: `worktree_root`, `claude_command`, `template`
- Workspace templates are now actually materialized when spawning agent tabs (previously defined but unused)
- Roadmap document (`ROADMAP.md`) — research-backed goal and prioritized build order (P1–P5)

### Fixed
- Git branch detection now works inside git worktrees (`.git` file with `gitdir:` pointer was previously treated as a directory and the branch showed blank)

## 2026-03-24

### Fixed
- Crash (panic) when Claude notification text contains multi-byte UTF-8 characters (emojis) — string truncation now operates on character boundaries instead of byte offsets

## 2026-03-19

### Added
- Slack webhook notifications when Claude Code is awaiting input — configure `slack_notification_webhook` in `terminaler.json`
- Claude `waiting_input` user-var trigger for notifications — fires on status transition, with idle timeout fallback for older Claude versions
- Notification messages include model name, cost, and project CWD context
- Windows registry AUMID registration (`HKCU\SOFTWARE\Classes\AppUserModelId\org.wezfurlong.terminaler`) so Windows 11 toast notifications work without a Start Menu shortcut

### Fixed
- Tab sidebar resize drag stops working after ~1 mouse move — drag state was not re-stored after processing move events
- Text selection highlight offset when pane is zoomed via Ctrl+scroll — mouse-to-cell conversion now matches the renderer's float coordinate system exactly, and pane hit-testing uses base-metric coordinates
- Tab sidebar showing same CWD for all Claude pane cards in split layouts — each pane's Claude Card now uses that pane's own CWD instead of the shared tab-level CWD
- Tab sidebar not refreshing when CWD data changes — sidebar element cache is now invalidated when polled info differs

### Changed
- Multi-pane Claude panes now render as full cards in sidebar (matching single-pane tabs) instead of compact tree-connector one-liners
- Default Claude status is `idle` (gray) when no user var is set, instead of `working` (green)
- Path/CWD text in tab sidebar now uses smaller title font (12pt Roboto) instead of terminal font for better visual hierarchy
  - Pane CWD in pane tree
  - Git branch under single-pane tabs
  - Claude card detail lines (project/branch, context bar, cost/duration stats)
