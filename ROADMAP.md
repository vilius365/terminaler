# Terminaler Roadmap — Goal & Prioritized Features

> **Goal**: Make Terminaler the best terminal for experienced Linux + Claude Code power users —
> the GUI-native, Windows-native equivalent of what tmux power users assemble by hand
> (worktrees + agent status + notifications), on top of a daemon architecture that
> Windows Terminal structurally cannot match.
>
> Established 2026-06-11 from a deep-research pass (21 sources, 25 claims adversarially
> verified 3-vote each: 19 confirmed, 6 refuted) crossed against a full codebase
> feature inventory. Sources include Anthropic's official power-user guide, Boris
> Cherny's worktree announcement, microsoft/terminal #961, ghostty-org #5623/#5932,
> anthropics/claude-code #22528, and the workmux/claude-squad/ccmanager ecosystem.

## Positioning

Terminaler does **not** race the Claude Code CLI (`claude --worktree --tmux` shipped
2026-02-20 and Anthropic may keep commoditizing orchestration plumbing). The defensible
angle is the **GUI layer the CLI cannot provide**: one-action worktree+pane spawn,
an at-a-glance fleet dashboard, click-to-jump alerting, and live session persistence.

## Verified existing strengths (harden + market, don't rebuild)

| Strength | Evidence |
|---|---|
| **Live session persistence** (daemon holds PTYs; GUI restarts don't kill sessions) | Windows Terminal's #961: 231 reactions over ~5 years; Microsoft maintainers called live-state restore "neigh-impossible" in their architecture and shipped only buffer-snapshot replay (v1.21). Terminaler already exceeds the incumbent here. |
| **Claude status cards** (status/model/context %/cost/duration/±lines per pane, multi-pane aware) | Anthropic explicitly advises per-session status visibility; workmux/recon/tmux-agent-sidebar all converge on this need. Terminaler's cards are ahead of every GUI terminal surveyed. |
| **Waiting-input notifications** (toast + Slack webhook, per-pane badges/mute) | Validated: practitioners "keep missing" plain OS notifications. |
| Command palette, quick-select, copy mode + regex search, snap layouts, workspace templates | All present and working (inherited/built); see inventory below. |

## Prioritized build order

### P1 — GUI-native multi-agent worktree orchestration ⭐ highest leverage
The single most-demanded workflow: 3–5 parallel Claude sessions, one per git worktree
(vendor-endorsed; workmux ~1.6k stars exists solely for this in tmux).

- **"New Agent" action** ✅ (`ctrl+shift+a`): create git worktree (or reuse) →
  spawn tab from the `claude-code` template → launch `claude` in it.
- **Agent fleet dashboard** ✅ (`ctrl+shift+j`): cross-window/workspace overlay listing every
  Claude pane with status, worktree/branch, context %, cost — Enter jumps to it.
- **Worktree lifecycle** ✅ (`ctrl+shift+w`): worktree manager overlay listing the repo's
  worktrees with dirty state, offering "merge & remove" and "discard" with confirmation.
- Crates: `terminaler-layout` (template spawn already supports per-pane cwd/command),
  `config/src/keyassignment.rs` (new actions), `terminaler-gui` overlay.

  **P1 code-complete on Linux; all three actions pending Windows functional validation.**

### P2 — Per-session differentiation + click-to-jump alerting
Extends what exists; the bottleneck at 3+ agents is "which one wants me?".

- Click a toast / sidebar badge → focus the exact window+tab+pane (toasts currently inform only).
- Per-agent accent colors (auto-assigned, stable per worktree) on tab edge, pane border,
  and notification — Anthropic literally recommends color-coding tabs for this.
- Aggregate statusline/taskbar signal: "2 agents waiting" overview badge; optional
  taskbar-icon overlay/flash on `waiting_input`.
- Optional summary in notification: last assistant line or permission prompt text
  (from `claude_status` transition payload).

### P3 — Harden + market live persistence (the anti-Windows-Terminal differentiator)
- Crash-safe daemon: GUI and daemon crash/upgrade independently; verify reconnect paths.
- Restore-on-boot option (daemon as startup task; GUI reattaches to live PTYs).
- Scrollback survives reattach; document and demo the "close GUI mid-Claude-run,
  reopen, nothing died" flow prominently in README — this is the headline feature
  Windows users have wanted since 2019.

### P4 — Claude-aware prompt navigation (semantic zones for agent panes)
Claude Code does **not** emit OSC 133 (anthropics/claude-code #22528 + 3 duplicates,
unimplemented through June 2026), so prompt-jumping is inert exactly where users need it.
Terminaler already detects Claude panes (`claude_status`) and has `ScrollToPrompt` +
SemanticZone machinery terminal-side.

- Client-side prompt boundary detection for Claude panes (recognize the REPL prompt /
  turn separators in the grid) feeding the existing semantic-zone jump actions →
  "jump between Claude turns" before any competitor has it.
- Ship OSC 133 shell-integration snippets (bash/zsh/pwsh) + docs so normal shell panes
  get prompt jumping & output selection too (supported by Ghostty/Kitty/WezTerm/WT —
  table stakes among power users).
- Treat the Claude-specific detection as a shim: if Anthropic ships OSC 133 emission,
  the same UX falls back to the standard path.

### P5 — Command palette as the front door
Ghostty validated demand (#5623, endorsed by Hashimoto, shipped in ~3 months).
Terminaler inherits `ActivateCommandPalette` — the work is exposure, not construction.

- Populate with Terminaler-specific entries: snap layouts, workspace templates,
  "New Agent" (P1), agent dashboard (P1), toggle web access, notification mute.
- Show default keybindings in entries (discoverability is the entire point).
- Make sure it's bound and mentioned in first-run config.

## Watchlist (validated gaps, unranked by evidence)

- **Web access input** (`terminaler-web` is read-only today): would enable "answer the
  agent from your phone". No verified demand signal yet — revisit after P1/P2 ship.
- **SSH domains** (stripped in fork): research *refuted* the claim that WezTerm-lineage
  SSH muxing is a differentiator. Don't reinstate on inheritance grounds; only if a
  concrete remote-agent workflow demands it (web access may cover it cheaper).
- **OTel usage stats** (`CLAUDE_CODE_USAGE_INTEGRATION.md`): daily/weekly cost roll-ups
  on the dashboard once P1's fleet view exists.

## Anti-goals (evidence-based)

- No AI built into the terminal itself (command generation/autocomplete à la Warp) —
  contrarian sweep showed experienced users reject AI-in-terminal; the target user
  already runs Claude Code *in* the terminal.
- No Lua/plugin runtime resurrection; JSON config stays.
- No racing Anthropic on CLI orchestration features.

## Key risks

1. **Anthropic compression**: Claude Code Desktop already had worktree support before the
   CLI; a first-party orchestration UI would compress P1's window. Mitigation: ship the
   GUI affordances (dashboard, jump, colors) that a CLI/desktop-app can't match inside
   *your* terminal.
2. **OSC 133 emission lands in Claude Code** → P4's Claude-specific shim becomes
   redundant (acceptable: the UX survives, the detection path swaps).
3. **Practical agent ceiling**: practitioners report supervising 2–3 agents, not 5 —
   favors P2's notification quality over dashboard density if forced to choose.

---

*Feature inventory snapshot (2026-06-11): full per-feature status table in the research
session; notable absences at time of writing — SSH/TLS/serial domains (stripped),
OSC 133 emission/integration scripts, web-access input, worktree tooling.*
