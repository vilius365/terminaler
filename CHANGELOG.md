# Changelog

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
