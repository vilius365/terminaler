# Changelog

## 2026-08-22

### Fixed
- **Wayland window shrank by one titlebar on every resize motion.** Dragging any resize edge under a client-side frame made the window lose height per mouse-move until it collapsed. The configure handler subtracts the frame borders from the configured size to get the content area, then reported *that reduced figure* to the compositor via `set_window_geometry` — but geometry describes the whole window, so the compositor was told the window was one titlebar shorter than it is, its next configure came back that much smaller, and the titlebar came off again, compounding per event. Now adds the borders back before reporting. Only the **height** was affected because `sctk-adwaita`'s `subtract_borders` returns the width unchanged, which is exactly why the window narrowed correctly but lost height, on both left and right edges. Applies only to the client-side frame; GNOME's Mutter declines server-side decorations, so it is the path GNOME always takes.
- **A newly configured tmux box never appeared in the sidebar.** `paint_tab_sidebar` caches the built element tree and rebuilds only when a fingerprint of the discovery snapshot moves, but the fingerprint iterated `snap.sessions` **only** — so a box whose probe had not answered yet contributed nothing, and *"box present but empty" hashed identically to "box absent"*. A box added to the config stayed invisible until something unrelated happened to invalidate the cache, which is indistinguishable from a failed SSH probe. The fingerprint now includes each box's name and status. Status matters on its own: it drives the header dot and the error line, neither of which is a session. Two things that look decisive here and are **not**: `ps --ppid` sampling misses a sub-second probe, and zero `tmux discovery` lines in a debug log is what *success* looks like, since every one of those `log::debug!` calls sits in an error arm.
- **Agent badge painted over the session row's right border and rounded corner.** The badge is `Float::Right`, and `box_model` positions such a child with `float_max_x = (max_x + float_width).min(max_width)` where `max_width` falls back to the **parent's** full bounds when the element sets none. The row set none, so the badge was clamped to the sidebar's edge rather than the card's. Setting `max_width` on the row fixes it. Neither shortening the name text nor shrinking the row's `min_width` moves a floated child — measured, the latter shifted the card 10px left and left the badge at the identical x. Verified by pixel scan: badged and unbadged rows now end at the same x.

### Changed
- **Linux sidebar's tmux section now matches the Windows WebView.** The two are separate renderers — Windows draws HTML in a WebView2 from `terminaler-gui/assets/sidebar.html`, Linux builds GPU box-model elements in `tab_sidebar.rs` — so nothing is shared and the CSS is a spec the Rust side has to track by hand. Added the missing `TMUX SESSIONS` heading (mirroring `.stats-title`: 10px, weight 700, uppercase, `--accent-orange`), and stopped filling the session rows. `.tmux-session-row` asks for `var(--bg-hover)`, which `:root` never defines, so on Windows it resolves to transparent — sampling inside a row in a reference screenshot gives `#1a1a1a`, the same value as the gap between rows, while the GPU rows filled with `border_subtle` and read as solid blocks. Hover still lifts the fill. Verified against a Windows screenshot: row interior, badge fill, status dot and text colours all match, at the same 41px row pitch.

## 2026-08-07

### Fixed
- **Tmux instance badge was unreadable white-on-yellow**: `.tmux-session-agent-instance` set `color: var(--bg-hover)`, a custom property that is **never defined anywhere** in `sidebar.html`. Because `color` is an inherited property, an invalid `var()` resolves to the inherited value rather than the initial one — so the badge took the row's near-white `--text-primary` (`#e0e0e0`) on the orange `--accent-orange` (`#db8b0b`) background: a contrast ratio of **2.06:1**, well below the 4.5:1 accessibility floor. Now uses `--bg-base`, giving **6.88:1**. Two sibling symptoms of the same missing variable remain and are cosmetic only: `.tmux-session-row`'s `background: var(--bg-hover)` silently resolves to transparent (`background` is *not* inherited, so it falls to the initial value), and `--bg-tertiary` is likewise referenced but never defined.

### Added
- **`ci/build-windows-staging.sh` — cross-build for Windows without quitting Terminaler.** `~/pCloudDrive/terminaler-windows-build/` **is** the folder the Windows exes run from, not a staging area, so building into it *was* deploying — and since Windows locks running exes, every build required closing the app first. Builds now land in the sibling `terminaler-windows-staging/`, which syncs harmlessly while Terminaler runs; `--promote` then copies staging → live behind a timestamped backup of the current exes, and is the only step needing the app (and `terminaler-mux-server.exe`) closed. Also `--status` (compare staged vs live mtimes) and `--dry-run`. Ships the full 6-file set — 3 exes plus `WebView2Loader.dll`, `conpty.dll`, `OpenConsole.exe` — and writes a `BUILD-INFO.txt` recording commit, branch, dirty-state and sha256, so a build's provenance is never in doubt again.

### Changed
- **`CLAUDE.md` config documentation corrected — it was substantially wrong.** It stated "Config keys: camelCase in JSON" and documented `profiles`, `layouts`, `workspaces` and `keybindings` sections; **none of those keys exist in the parser** (`grep -rn "profiles" config/src/*.rs` returns nothing). The JSON keys are the literal **snake_case** Rust field names — the `Config` struct derives `FromDynamic` with no rename attributes — e.g. `default_prog`, `default_domain`, `font_size`, `web_access`/`bind_address`, and `keys` (not `keybindings`). `config/src/defaults.rs` is the accurate reference. This also resolves a self-contradiction: the 2026-08-06 entry above (and the `tmux` section note) claimed the `tmux` section used snake_case "unlike most of the config" — in fact the *whole* config always has.

### Notes
- **`default_prog` does not apply to WSL panes, and this fork defaults to WSL.** `terminaler-gui/src/main.rs:585-595` prefers the first WSL domain as the default (falling back to `local` only when no distro exists), and `mux/src/domain.rs:438-441` reads `default_prog` from *that domain* — which `WslDomain::default_domains()` leaves unset — via `.map(…).unwrap_or(…)`, a chain that yields `Some(None)` and therefore silently discards the top-level value. This is **upstream WezTerm behavior** (it arrived verbatim in the `f425086` snapshot), i.e. deliberate namespace separation, not a bug: top-level `default_prog` is a local-domain concept and WSL domains carry their own. To point new panes at a remote host, pair the two settings — `"default_domain": "local"` alongside `"default_prog": ["ssh", "<host>"]` — so ssh runs as a Windows process with the Windows ssh config. Making `default_prog` fall through to WSL panes would *not* be equivalent: `fixup_command` wraps a WSL domain's command as `wsl.exe --distribution <d> --exec <args>`, so it would run ssh from *inside* WSL with WSL's ssh config.

## 2026-08-06

### Added
- **Claude instance name on tmux session rows**: each session in the sidebar card (and the `ctrl+shift+s` picker) is labelled with the agent running in it. Where the session is registered with claude-agent-interconnect the badge shows the **instance name** with a `⇄` prefix (`⇄ holly`) — the same name that session's statusline shows — so identical-looking sessions across boxes are distinguishable at a glance. The registry is read once per poll cycle (not per box) from `GET <interconnect_url>/instances` and matched on `machine` + `tmux_session`, both of which it already carries; no pane-id plumbing needed. Where no instance is registered, a second per-box probe (`tmux list-panes -a -F '#{pane_current_command} #{pane_pid} #{session_name}'`) supplies the generic agent **type** as a muted fallback badge (`claude`, `codex`, `aider`, `gemini`, `opencode`, `cursor-agent`; `node` is deliberately excluded — it would tag every `npm run dev` pane). Both lookups are strictly best-effort: a daemon that is down or a failing pane scan steps the badge down (instance → type → nothing) and can never make a reachable box look unreachable or block the session list. New config: `tmux.interconnect_url` (default `http://127.0.0.1:7799`, `""` disables) and per-box `interconnect_machine` for when the registry knows a box under a different name than its `name` (e.g. box `wsl` registered as `home`). Live-validated on devbox: 4 instances fetched, 3 sessions matched.

### Fixed
- **`wsl.exe` probe failures reported as the useless "probe failed with no stderr"**: `wsl.exe` writes its diagnostics ("There is no distribution with the supplied name") as **UTF-16LE**, which the UTF-8-only decode turned into NUL-separated mush — the first line came out empty and the real message was discarded. Probe output is now decoded as UTF-16LE when a BOM or a high NUL density says so (genuine UTF-8 from the Linux side passes through untouched), falls back to **stdout** when stderr is empty (`wsl.exe` uses both), and reports the exit code when both streams are silent. The "no server running" check now looks at both streams too, so a box with no tmux server running is the normal empty state rather than an error.

### Changed
- **Window count is no longer the cryptic `1w`**: session rows show `▣ 3` with a tooltip spelling it out (`3 windows, attached`, correctly singular at `1 window`). The session name now absorbs the row's slack and truncates with an ellipsis, so the agent badge and count stay visible however narrow the sidebar is.
- Documented the `tmux` config section in `CLAUDE.md`, including two traps that cost real debugging time: the section uses **snake_case** keys (unlike most of the config) and **rejects unknown keys outright**, so a camelCase spelling is a hard error rather than an ignored line; and JSONC permits comments but **not trailing commas**, so a config whose last entry is followed only by comment lines is invalid and silently falls back to the last-good config — indistinguishable from a broken feature.

## 2026-08-05

### Fixed
- **Garbled/letterboxed plain attach** (found in live Windows testing, root-caused by the local_lan Claude session): a control-mode attach leaves the tmux windows on `window-size manual` sized to the GUI, turning any later plain attach in a split into a small panning viewport — plain attach now unsets `window-size` on the session's windows before attaching (all windows via shell loop over ssh; current window via tmux `;` chaining on argv-verbatim transports). Separately, the non-interactive ssh attach could come up with a non-UTF-8 tmux client (missing remote LANG → all TUI glyphs rendered as underscores) — every attach now passes `tmux -u`.

### Changed
- **Default attach is now a plain `tmux attach` in a 50% split** (picker Enter and sidebar row click), keeping your layout intact and letting tmux `bind -n` keys (e.g. F12 detach) work — control mode injects keys via `send-keys`, which bypasses tmux's key table entirely, so no root-table binding can ever fire there. Session rows are styled as buttons so they read as clickable.
- **⧉ icon on a sidebar session row attaches in the ACTIVE pane**: types a shell-family-safe (`cmd`/PowerShell/POSIX) plain-attach command into the focused pane's shell, replacing its content with the session UI — the pane must be sitting at a prompt. Control mode (windows-as-native-tabs) no longer has a sidebar affordance; it remains reachable via manual `tmux -CC attach` or an `AttachTmuxSession { mode: ControlTab }` keybinding. Recommended companion on every remote box (live on devbox+wsl): `set-hook -g client-attached "if -F '#{?client_control_mode,,1}' 'set -w -u window-size'"` — self-heals control-mode sizing residue for attach paths that can't carry the reset (the ⧉ typed command must stay quoting-portable).

### Added
- **Native tmux control-mode integration restored**: running `tmux -CC attach` in a pane (locally, over `ssh -t`, or via `wsl.exe`) now turns the tmux session's windows into real Terminaler tabs/panes, instead of leaving the pane hung. The `TmuxDomain`/`tmux_commands`/`tmux_pty` machinery stripped in the Phase 0 fork commit (`8b63091`) is resurrected from upstream WezTerm (~1800 lines, near-verbatim) and rewired into `localpane.rs`; the control-mode lexer half had survived the strip and needed no changes. Hardened over upstream: `TmuxChild::try_wait` no longer panics (`todo!()` → real answer from the active lock), and the per-attach mux-notification subscriber now unsubscribes when its domain detaches instead of leaking. Inherited upstream limitations apply (scrollback captured once at attach, no mouse reporting inside tmux panes, origin pane frozen behind a "press q to detach" banner). **Windows ConPTY passthrough of the DCS 1000p handshake is validated working** on a live Windows host — with the sideloaded modern ConPTY (`conpty.dll` + `OpenConsole.exe` next to the exe, produced by the cross-build and preferred by `pty/src/win/psuedocon.rs` over the in-box kernel ConPTY).
- **Multibox tmux session discovery** (`tmux` config section): a background poller runs `tmux list-sessions` on every configured box — over `ssh` (BatchMode, ConnectTimeout, riding your ssh config/keys), `wsl.exe -e`, or a custom argv prefix (e.g. `tailscale ssh`) — on a configurable interval (default 30s), with parallel per-box probes, hard timeouts, and per-box failure isolation (one box down never affects the others; last-known sessions are kept and marked stale). Config: `boxes: [{ name, connection: { Ssh { target } | Wsl { distribution } | Command { argv_prefix } }, tmux_command }]`.
- **Tmux Session Picker** (`ctrl+shift+s`, also in the command palette and Shell menu): fuzzy picker listing `box:session (N windows, attached)` across all boxes, with unreachable boxes shown inline; Enter spawns the control-mode attach in a new tab (explicitly in the `local` domain — the default domain prefers WSL, which would wrap the ssh/wsl argv in another `wsl.exe --exec`).
- **Tmux sessions section in the WebView2 sidebar**: sessions grouped by box with status dot (green ok / red unreachable / grey pending), stale dimming, per-box error line, window count + attached marker per session, a ⟳ refresh button, and click-to-attach. Hidden entirely when the `tmux` config section is absent or disabled.

## 2026-07-09

### Added
- **Eye-friendly default color scheme**: the out-of-the-box palette is now warm Gruvbox Dark (cream `#ebdbb2` on soft dark grey `#282828`) instead of the terminal core's grey-on-**pure-black** default. Pure-black backgrounds maximize halation and eye strain; the previous `dark_theme()`/`ThemeName` machinery in `themes.rs` was dead code and never applied. `resolved_palette` now defaults to the bundled Gruvbox scheme; user `color_scheme`/`colors` still override it.
- **Runtime theme selector** (`ctrl+shift+k`, also "Theme Picker" in the command palette and View menu): a fuzzy picker over all ~1000 bundled color schemes plus any custom ones, built on the existing launcher infrastructure. Selecting a theme applies it live and persists it to `terminaler.json` (comment-preserving surgical edit), so it survives restarts.
- **Theme selector in the WebView2 sidebar**: a "🎨 Theme" button expands a scrollable, click-to-apply list of all schemes (active one highlighted and scrolled into view), plus a "Search all… (`ctrl+shift+k`)" row that opens the full fuzzy picker. Click-driven by necessity — the sidebar blurs focus to keep the terminal focused, so a native `<select>`/search input can't work there.

### Fixed
- **Theme apply was slow** (took "ages"): applying a color scheme ran the full config-reload pipeline twice (an explicit reload plus the file-watcher reacting to the persisted write), and each `config_was_reloaded()` re-resolved every font, rebuilt the font-dir DB, cleared all glyph/shape caches, and recomputed window size — none of which a color change needs. A theme change now applies the palette live via a lightweight path (`apply_palette_change` — palette + colored render caches + per-pane config + repaint only), skipping font re-resolution and resize.
- **Silent Windows startup failure**: a `to_dynamic()` equality guard briefly added to `config_was_reloaded()` (to make the watcher reload a no-op) could run near startup via the initial `AppearanceChanged` event and broke launch. Removed — the lightweight apply path already delivers the speed win without it.
- **Config writer hardened**: `persist_color_scheme` now validates that the surgically-edited config still parses before writing, and refuses to write otherwise — a theme apply can never corrupt `terminaler.json` and break the next startup.

### Changed
- Corrected the architecture doc: the daemon binary is `terminaler-mux-server`, not the non-existent `terminaler-daemon` (`CLAUDE.md`).

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
## 2026-04-10

### Added
- Cookie-based authentication for web access — token is set via HttpOnly cookie on first visit, no longer exposed in URL or WebSocket params
- Session restore now preserves hidden panes, per-window workspace, and tab titles
- Software OpenGL (mesa) fallback when WGL initialization fails
- Error dialog with troubleshooting hint when GUI window creation fails
- Build.rs copies companion DLLs (conpty, ANGLE, mesa) during cross-compile from WSL

### Fixed
- GUI window failing to appear when web access port is already in use — web server startup was blocking the main thread

### Changed
- Web access token removed from URL query string (pages and WebSocket)
- WebSocket session validates pane ownership via `require_attached_pane` helper

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
