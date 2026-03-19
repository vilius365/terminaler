# Changelog

## 2026-03-19

### Fixed
- Tab sidebar resize drag stops working after ~1 mouse move — drag state was not re-stored after processing move events
- Text selection highlight offset when pane is zoomed via Ctrl+scroll — mouse-to-cell conversion now matches the renderer's float coordinate system exactly, and pane hit-testing uses base-metric coordinates
- Tab sidebar showing same CWD for all Claude pane cards in split layouts — each pane's Claude Card now uses that pane's own CWD instead of the shared tab-level CWD
- Tab sidebar not refreshing when CWD data changes — sidebar element cache is now invalidated when polled info differs

### Changed
- Path/CWD text in tab sidebar now uses smaller title font (12pt Roboto) instead of terminal font for better visual hierarchy
  - Pane CWD in pane tree
  - Git branch under single-pane tabs
  - Claude card detail lines (project/branch, context bar, cost/duration stats)
