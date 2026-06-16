# Changelog

## 2026-06-15

### Added
- **New Agent and Manage Worktrees work from WSL panes**: when invoked from a WSL-domain pane, both now run git inside the distro (`wsl.exe --distribution <distro> --exec git …`) at Linux paths instead of failing. New Agent spawns the agent tab in the WSL domain with a bare `claude` command; Manage Worktrees lists / merges & removes / discards worktrees through the distro's git. Repo discovery uses `git rev-parse --show-toplevel` (no `std::fs` against the distro filesystem); worktrees land at `<repo-parent>/<repo>-worktrees/<branch>` in Linux space. Distro is resolved from the pane's domain. The git plumbing is now unified behind a `GitEnv` (Local vs WSL) abstraction. **Requires shell integration (OSC 7) in the WSL shell** so terminaler can see the pane's real cwd — without it, New Agent falls back to the pane's spawn directory (terminaler can't inspect a WSL pane's cwd from the Windows side). (Runtime-validated build pending on a Windows+WSL host; the `wsl.exe git` behavior itself is compiled-and-tested-logic only on the Linux dev box.)
- **"N agents waiting" badge** (P2): the tab bar shows a session-wide count of Claude agent panes in `waiting_input` across all windows, so you know at a glance that some agent needs attention even when its pane/window isn't focused. Polled once per second (cheap user-var read, no process enumeration) independent of sidebar visibility.
- **Per-agent identity colors** (P2): each Claude agent gets a stable accent color derived deterministically from its worktree (`claude_worktree`), shown in three places (all from the same seed, so they always match): the pane border, a `●` chip on its sidebar card, and the agent's tab in the horizontal tab bar (active tab gets a colored outline; inactive tabs tint their edge with the dimmed color). So tiled agents are distinguishable at a glance. This is an identity signal, separate from the status color; the orange notification ring still takes precedence on inactive panes with unread notifications. Pane-border rendering refactored onto a shared `draw_border_ring` helper.
- **Manage Worktrees** action (`ctrl+shift+e`): overlay listing the current repo's git worktrees with dirty/clean state, offering two lifecycle actions per worktree — **merge & remove** (`m`: merges the branch into the main worktree, then removes the worktree and deletes the now-merged branch; refuses if the worktree is dirty) and **discard** (`d`: force-removes the worktree and deletes its branch). Both require explicit `y` confirmation and surface dirty state first; the main worktree cannot be removed. Completes the P1 worktree-orchestration lifecycle.

### Fixed
- **Agent worktree actions are guarded inside SSH sessions**: if a pane is sitting in an `ssh <host>` session (e.g. you SSH'd out from a WSL pane), New Agent and Manage Worktrees now refuse with a clear message instead of silently creating the worktree in the **local** filesystem at a stale path (the pane's domain is still local/WSL, so it had no way to know it was remote). Detection is heuristic (an `ssh` process in the pane's foreground/process tree) and biased toward refusing. Full remote (`GitEnv::Ssh`) support is out of scope for now.
- **Manage Worktrees keybinding was shadowed**: rebound from `ctrl+shift+w` to **`ctrl+shift+e`**. `ctrl+shift+w` is auto-synthesized as the CTRL+SHIFT twin of `CloseCurrentTab` (Super+w) — the expected close-tab binding on Windows — and won by insertion order, so the old binding silently closed the tab. (The action was always reachable via the command palette.)
- **Confusing error from WSL/remote panes**: New Agent and Manage Worktrees no longer fail with a misleading `\\host\path is not inside a git repository` when invoked from a WSL or remote pane. Such panes report a `file://` cwd with the remote hostname as the authority, which Windows `to_file_path()` silently turns into a bogus UNC path. They're now detected and rejected with a clear message ("…run from a local pane in a Windows git repository (WSL support is coming)"). Full WSL-domain support is the next planned step. (Local panes are unaffected.)
- **New Agent failed to launch `claude` on Windows**: the default `claude_command` is now `["cmd", "/c", "claude"]` on Windows (was `["claude"]`). On a standard npm/nvm install there is no `claude.exe` — only a `claude.cmd`/extensionless shim — and ConPTY's `CreateProcessW` can't execute a script directly, so the agent pane never started. (Confirmed against a real Windows install.) Non-Windows default stays `["claude"]`.
- Worktree merge now fails safe if git state can't be determined: `merge_and_remove` uses a strict `ensure_clean` check that errors on a failed `git status` instead of assuming the worktree is clean.

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
