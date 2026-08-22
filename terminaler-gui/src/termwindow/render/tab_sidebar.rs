use crate::termwindow::box_model::*;
use crate::termwindow::render::corners::{
    BOTTOM_LEFT_ROUNDED_CORNER, BOTTOM_RIGHT_ROUNDED_CORNER, TOP_LEFT_ROUNDED_CORNER,
    TOP_RIGHT_ROUNDED_CORNER,
};
use crate::termwindow::{SidebarTabInfo, TabSidebarItem, UIItem, UIItemType};
use crate::utilsprites::RenderMetrics;
use config::{Dimension, TabSidebarPosition};
use mux::pane::CachePolicy;
use mux::tab::TabId;
use mux::Mux;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};
use terminaler_font::LoadedFont;
use terminaler_term::color::ColorPalette;
use window::color::LinearRgba;

/// ── Shared visual spec (V2 "Tiles") ─────────────────────────────────────────
/// Colors mirror the CSS `:root` token block in assets/sidebar.html — that
/// block is the single source of truth and the two are kept in sync BY HAND.
///
/// Spec table (both renderers):
///   rail width          90 px  (config tab_sidebar_width)
///   tile                68 px wide, 1 px status-tint border, rounded, 6 px gap
///   pane sub-tile       62 px wide, single line
///   status dot          INSIDE the tile, right end of the icon line
///   notification badge  red count, left end of the icon line
///   context bar         thin strip inside the tile bottom
///   flyout              264 px wide, opens after 200 ms hover
///   label               centered under the icon, truncated with …
pub(crate) const TILE_W: f32 = 80.;
pub(crate) const TILE_HALF_W: f32 = 72.;
pub(crate) const TILE_H: f32 = 48.;
pub(crate) const TILE_GAP: f32 = 6.;
pub(crate) const FLYOUT_W: f32 = 264.;

pub(crate) struct SidebarTheme {
    pub bg_base: LinearRgba,
    pub bg_surface: LinearRgba,
    pub bg_elevated: LinearRgba,
    pub border_subtle: LinearRgba,
    pub border_default: LinearRgba,
    pub text_primary: LinearRgba,
    pub text_secondary: LinearRgba,
    pub text_tertiary: LinearRgba,
    pub accent_blue: LinearRgba,
    pub accent_green: LinearRgba,
    pub accent_yellow: LinearRgba,
    pub accent_red: LinearRgba,
    pub accent_orange: LinearRgba,
}

impl SidebarTheme {
    pub fn load() -> Self {
        // sRGB -> linear, the same transfer function SrgbaTuple::to_linear
        // applies; inlined to keep this dependency-free.
        fn srgb_to_linear(c: f32) -> f32 {
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        }
        fn rgb(r: u8, g: u8, b: u8) -> LinearRgba {
            LinearRgba::with_components(
                srgb_to_linear(r as f32 / 255.),
                srgb_to_linear(g as f32 / 255.),
                srgb_to_linear(b as f32 / 255.),
                1.,
            )
        }
        Self {
            bg_base: rgb(0x12, 0x12, 0x12),
            bg_surface: rgb(0x1a, 0x1a, 0x1a),
            bg_elevated: rgb(0x22, 0x22, 0x22),
            border_subtle: rgb(0x2e, 0x2e, 0x2e),
            border_default: rgb(0x3a, 0x3a, 0x3a),
            text_primary: rgb(0xe0, 0xe0, 0xe0),
            text_secondary: rgb(0x99, 0x99, 0x99),
            text_tertiary: rgb(0x66, 0x66, 0x66),
            accent_blue: rgb(0x4d, 0x9e, 0xff),
            accent_green: rgb(0x3f, 0xb9, 0x50),
            accent_yellow: rgb(0xd2, 0x99, 0x22),
            accent_red: rgb(0xf8, 0x51, 0x49),
            accent_orange: rgb(0xdb, 0x8b, 0x0b),
        }
    }
}

/// Fade a color by scaling its alpha — used for stale boxes and inactive tints.
fn dim(c: LinearRgba, f: f32) -> LinearRgba {
    LinearRgba::with_components(c.0, c.1, c.2, c.3 * f)
}

/// Status color for a claude session (dot, flyout edge).
fn status_color(
    claude: &crate::termwindow::ClaudeSessionInfo,
    theme: &SidebarTheme,
) -> LinearRgba {
    use crate::termwindow::ClaudeStatus;
    match claude.status {
        Some(ClaudeStatus::Working) => theme.accent_green,
        Some(ClaudeStatus::WaitingInput) => theme.accent_yellow,
        Some(ClaudeStatus::Error) => theme.accent_red,
        Some(ClaudeStatus::Idle) | None => theme.text_tertiary,
    }
}

/// Tile border tint: the status color, softened like the CSS `.tile.st-*`
/// border rgba values.
fn status_border(
    claude: &crate::termwindow::ClaudeSessionInfo,
    theme: &SidebarTheme,
) -> LinearRgba {
    dim(status_color(claude, theme), 0.6)
}

impl crate::TermWindow {
    pub fn invalidate_tab_sidebar(&mut self) {
        self.tab_sidebar.take();
    }

    /// Count Claude agent panes in `waiting_input` across all windows,
    /// throttled to once per second. Cheap: reads cached user vars only (no
    /// process enumeration). Invalidates the tab bar when the count changes so
    /// the "N waiting" badge repaints.
    pub fn update_agents_waiting(&mut self) {
        if self.last_agents_poll.elapsed() < Duration::from_secs(1) {
            return;
        }
        self.last_agents_poll = Instant::now();

        let mux = Mux::get();
        let waiting = mux
            .iter_panes()
            .iter()
            .filter(|pane| {
                pane.copy_user_vars()
                    .get("claude_status")
                    .map_or(false, |s| s.as_str() == "waiting_input")
            })
            .count();

        if waiting != self.agents_waiting {
            self.agents_waiting = waiting;
            self.invalidate_fancy_tab_bar();
        }
    }

    /// Poll CWD and git branch info for each tab, throttled to every 2 seconds.
    pub fn update_sidebar_info(&mut self) {
        if self.last_sidebar_info_poll.elapsed() < Duration::from_secs(1) {
            return;
        }
        self.last_sidebar_info_poll = Instant::now();
        let poll_start = Instant::now();

        let mux = Mux::get();
        let mux_window = match mux.get_window(self.mux_window_id) {
            Some(w) => w,
            None => return,
        };

        let mut new_info: HashMap<TabId, SidebarTabInfo> = HashMap::new();

        for tab in mux_window.iter() {
            let tab_id = tab.tab_id();

            let active_pane = match tab.get_active_pane() {
                Some(p) => p,
                None => continue,
            };

            let cwd_url = active_pane.get_current_working_dir(CachePolicy::AllowStale);
            let cwd_path = cwd_url.as_ref().and_then(|u| {
                if u.scheme() == "file" {
                    Some(u.path().to_string())
                } else {
                    None
                }
            });

            let cwd_short = match &cwd_path {
                Some(path) => shorten_path(path),
                None => String::new(),
            };

            let git_start = Instant::now();
            let git_branch = cwd_path.as_deref().and_then(find_git_branch);
            let git_elapsed = git_start.elapsed();
            if git_elapsed > Duration::from_millis(100) {
                log::warn!(
                    "update_sidebar_info: find_git_branch took {:?} for tab {}",
                    git_elapsed,
                    tab_id
                );
            }

            // Detect Claude Code sessions on ALL panes in the tab
            let mut pane_claude_info = std::collections::HashMap::new();
            for pane_pos in tab.iter_panes_ignoring_zoom() {
                let pane = &pane_pos.pane;
                if let Some(info) = claude_info_for_pane(pane) {
                    pane_claude_info.insert(pane.pane_id(), info);
                }
            }

            new_info.insert(
                tab_id,
                SidebarTabInfo {
                    cwd_short,
                    git_branch,
                    pane_claude_info,
                },
            );
        }

        // If the info changed, invalidate the cached sidebar element so it
        // gets rebuilt on the next paint with the fresh data.
        if self.sidebar_info_changed(&new_info) {
            self.tab_sidebar.take();
        }

        self.tab_sidebar_info = new_info;

        let poll_elapsed = poll_start.elapsed();
        if poll_elapsed > Duration::from_millis(200) {
            log::warn!(
                "update_sidebar_info poll took {:?} on the GUI thread (process enumeration + git lookups)",
                poll_elapsed
            );
        }
    }

    fn sidebar_info_changed(&self, new_info: &HashMap<TabId, SidebarTabInfo>) -> bool {
        if self.tab_sidebar_info.len() != new_info.len() {
            return true;
        }
        for (tab_id, new) in new_info {
            match self.tab_sidebar_info.get(tab_id) {
                Some(old) => {
                    if old.cwd_short != new.cwd_short
                        || old.git_branch != new.git_branch
                        || old.pane_claude_info != new.pane_claude_info
                    {
                        return true;
                    }
                }
                None => return true,
            }
        }
        false
    }

    /// Build the AGENTS section listing discovered Claude agents / tmux
    /// sessions. The caller measures the result with compute_element to learn
    /// how much vertical space to reserve for it.
    ///
    /// Returns None when the feature is off or nothing was discovered, so the
    /// sidebar keeps its previous layout exactly.
    /// Sessions this window is itself attached to, as (box_name, session).
    ///
    /// A discovered session that is already open as a local pane is the same
    /// thing listed twice, so the sidebar folds the remote row away. The link
    /// is derived rather than recorded: an attach pane runs
    /// `ssh -t <target> -- ... tmux ... attach ... -t <session>` (see
    /// TmuxBox::attach_argv_impl), so the pane's foreground process argv still
    /// names both the transport and the session. Deriving it means a detach
    /// un-folds the row the moment the ssh process exits, with no poll lag and
    /// nothing to keep in sync across a GUI restart.
    ///
    /// Returns an empty set when argv is unreadable, which leaves rows merely
    /// demoted rather than hidden — the safe direction to fail.
    fn locally_attached_sessions(&self) -> std::collections::HashSet<(String, String)> {
        use std::collections::HashSet;

        let mut found = HashSet::new();
        let boxes: Vec<(String, Vec<String>)> = match self.config.tmux.as_ref() {
            Some(tmux) => tmux
                .boxes
                .iter()
                .map(|b| (b.name.clone(), b.connection.probe_tokens()))
                .collect(),
            None => return found,
        };

        let mux = Mux::get();
        let window = match mux.get_window(self.mux_window_id) {
            Some(w) => w,
            None => return found,
        };

        for tab in window.iter() {
            for pos in tab.iter_panes_ignoring_zoom() {
                let info = match pos
                    .pane
                    .get_foreground_process_info(CachePolicy::AllowStale)
                {
                    Some(info) => info,
                    None => continue,
                };
                let argv = info.argv.join(" ");
                if !argv.contains("tmux") || !argv.contains("attach") {
                    continue;
                }
                for (box_name, tokens) in &boxes {
                    // A local box has no transport tokens, so it matches on the
                    // session name alone.
                    if !tokens.is_empty() && !tokens.iter().any(|t| argv.contains(t.as_str())) {
                        continue;
                    }
                    for snap in crate::tmux_discovery::snapshot() {
                        if &snap.box_name != box_name {
                            continue;
                        }
                        for session in &snap.sessions {
                            // Match the quoted `-t <session>` the attach argv
                            // builds, so a session name that is a prefix of
                            // another does not claim its neighbour's row.
                            if argv.contains(&format!("-t {}", session.session))
                                || argv.contains(&format!("-t '{}'", session.session))
                                || argv.contains(&format!("-t \"{}\"", session.session))
                            {
                                found.insert((box_name.clone(), session.session.clone()));
                            }
                        }
                    }
                }
            }
        }

        found
    }

    /// Tmux boxes as tile groups: an eyebrow header per box, then one tile per
    /// session (V2 "Tiles" spec). The rows-budget cap and the folding of
    /// locally-attached sessions carry over from the old list layout.
    fn build_agents_section(
        &self,
        font: &Rc<LoadedFont>,
        title_font: &Rc<LoadedFont>,
        metrics: &RenderMetrics,
        theme: &SidebarTheme,
        sidebar_width: f32,
        tile_budget: usize,
    ) -> Option<Element> {
        if !self.config.tmux.as_ref().map_or(false, |t| t.enabled) {
            return None;
        }

        let snaps = crate::tmux_discovery::snapshot();
        let row_count: usize = snaps.iter().map(|s| s.sessions.len()).sum();
        if row_count == 0 {
            return None;
        }

        let mut children = vec![];
        let mut tiles_emitted = 0usize;
        let mut tiles_hidden = 0usize;

        // Sessions this window already hosts in a pane fold away rather than
        // listing the same thing twice; see locally_attached_sessions.
        let folded = self.locally_attached_sessions();

        for snap in &snaps {
            if snap
                .sessions
                .iter()
                .all(|s| folded.contains(&(snap.box_name.clone(), s.session.clone())))
            {
                continue;
            }
            if snap.sessions.is_empty() {
                continue;
            }
            if tiles_emitted + 1 >= tile_budget {
                tiles_hidden += snap.sessions.len();
                continue;
            }

            let (dot_color, stale) = match snap.status {
                crate::tmux_discovery::BoxStatus::Ok => (theme.accent_green, false),
                crate::tmux_discovery::BoxStatus::Unreachable(_) => (theme.accent_red, true),
                _ => (theme.text_tertiary, true),
            };
            children.push(sidebar_eyebrow(
                title_font,
                &snap.box_name,
                Some(dot_color),
                theme,
                sidebar_width,
                stale,
            ));

            // An unreachable box's error is worth a dim one-liner; the old
            // layout had this and the tiles must not silently drop it.
            if let crate::tmux_discovery::BoxStatus::Unreachable(ref err) = snap.status {
                children.push(
                    Element::new(
                        title_font,
                        ElementContent::Text(truncate_str(err, 14)),
                    )
                    .display(DisplayType::Block)
                    .line_height(Some(1.1))
                    .padding(BoxDimension {
                        left: Dimension::Pixels(12.),
                        right: Dimension::Pixels(4.),
                        top: Dimension::Pixels(0.),
                        bottom: Dimension::Pixels(2.),
                    })
                    .colors(ElementColors {
                        border: BorderColor::default(),
                        bg: InheritableColor::Inherited,
                        text: theme.text_tertiary.into(),
                    })
                    .min_width(Some(Dimension::Pixels(sidebar_width))),
                );
            }

            for session in &snap.sessions {
                if folded.contains(&(snap.box_name.clone(), session.session.clone())) {
                    continue;
                }
                if tiles_emitted >= tile_budget {
                    tiles_hidden += 1;
                    continue;
                }

                // A named interconnect instance is the more specific identity,
                // so it names the tile in orange; a generic agent type or the
                // bare session name stays secondary.
                let (icon, icon_color, label, label_color) = if session.agent_is_instance {
                    (
                        "\u{21c4}", // ⇄ matches the statusline's instance marker
                        theme.accent_orange,
                        session.agent.clone().unwrap_or_else(|| session.session.clone()),
                        theme.accent_orange,
                    )
                } else if session.agent.is_some() {
                    (
                        "\u{21c4}",
                        theme.text_secondary,
                        session.agent.clone().unwrap(),
                        theme.text_secondary,
                    )
                } else {
                    (
                        "\u{25a3}", // ▣
                        theme.text_tertiary,
                        session.session.clone(),
                        theme.text_secondary,
                    )
                };

                // Window count only when it says something (n>1), plus a dot
                // when another client is attached — a "1" on every tile was
                // pure noise (user feedback, 2026-08-22).
                let count = match (session.windows > 1, session.attached) {
                    (true, true) => Some(format!("{}\u{25cf}", session.windows.min(9))),
                    (true, false) => Some(format!("{} ", session.windows.min(9))),
                    (false, true) => Some("\u{25cf}".to_string()),
                    (false, false) => None,
                };

                let dimf = if stale || session.attached { 0.55 } else { 1.0 };
                let mut tile = TileArgs::new(font, title_font, metrics, theme, sidebar_width);
                tile.icon = icon.to_string();
                tile.icon_color = dim(icon_color, dimf);
                tile.label = label;
                tile.label_color = dim(label_color, dimf);
                tile.right_hint = count.map(|c| (c, dim(theme.text_tertiary, dimf)));
                tile.border_color = dim(theme.border_subtle, dimf);
                if session.attachable {
                    tile.item = Some(TabSidebarItem::TmuxSession {
                        box_name: snap.box_name.clone(),
                        session: session.session.clone(),
                    });
                    tile.hover_border = Some(theme.accent_orange);
                }
                children.push(build_tile(tile));
                tiles_emitted += 1;
            }
        }

        if children.is_empty() {
            return None;
        }

        // Say what is not shown, so a truncated list never reads as the whole
        // picture.
        if tiles_hidden > 0 {
            children.push(
                Element::new(
                    title_font,
                    ElementContent::Text(format!("+{} \u{2026} C-S-s", tiles_hidden)),
                )
                .display(DisplayType::Block)
                .line_height(Some(1.2))
                .padding(BoxDimension {
                    left: Dimension::Pixels(10.),
                    right: Dimension::Pixels(4.),
                    top: Dimension::Pixels(1.),
                    bottom: Dimension::Pixels(3.),
                })
                .colors(ElementColors {
                    border: BorderColor::default(),
                    bg: InheritableColor::Inherited,
                    text: theme.text_tertiary.into(),
                })
                .min_width(Some(Dimension::Pixels(sidebar_width))),
            );
        }

        let section = Element::new(font, ElementContent::Children(children))
            .display(DisplayType::Block)
            .colors(ElementColors {
                border: BorderColor::default(),
                bg: InheritableColor::Inherited,
                text: theme.text_secondary.into(),
            })
            .min_width(Some(Dimension::Pixels(sidebar_width)));

        Some(section)
    }

    pub fn build_tab_sidebar(
        &self,
        _palette: &ColorPalette,
    ) -> anyhow::Result<ComputedElement> {
        let font = self.fonts.default_font()?;
        // Dedicated 9pt rail font (Entity::Sidebar): the 12pt title font was
        // visibly too large at 90px — labels ellipsized at 5-6 chars.
        let title_font = self.fonts.sidebar_font()?;
        let metrics = RenderMetrics::with_font_metrics(&font.metrics());
        let sidebar_width = self.tab_sidebar_width as f32;
        let border = self.get_os_border();
        let tab_bar_height = if self.show_tab_bar {
            self.tab_bar_pixel_height().unwrap_or(0.)
        } else {
            0.
        };
        let sidebar_top = border.top.get() as f32 + tab_bar_height;
        let window_height = self.dimensions.pixel_height as f32 - sidebar_top;

        // The rail commits to the fixed spec palette (SidebarTheme) on both
        // renderers rather than restyling per terminal color scheme — the
        // Windows sidebar is fixed-color too, and the two must read as the
        // same product. This extends what the tmux section already did.
        let theme = SidebarTheme::load();

        let mux = Mux::get();
        let mux_window = mux
            .get_window(self.mux_window_id)
            .ok_or_else(|| anyhow::anyhow!("no mux window"))?;

        let active_tab_id = mux
            .get_active_tab_for_window(self.mux_window_id)
            .map(|t| t.tab_id());

        let mut tab_elements = vec![];

        // Group heading for this window's own panes, matching the box eyebrows
        // the discovered machines get below. No status dot: live mux state has
        // no poller whose reachability could be reported.
        tab_elements.push(sidebar_eyebrow(
            &title_font,
            "local",
            None,
            &theme,
            sidebar_width,
            false,
        ));

        for (tab_idx, tab) in mux_window.iter().enumerate() {
            let tab_id = tab.tab_id();
            let is_active = active_tab_id == Some(tab_id);
            let title = tab.get_title();
            let info = self.tab_sidebar_info.get(&tab_id);
            let panes = tab.iter_panes_ignoring_zoom();
            let has_multiple_panes = panes.len() > 1;

            let (has_notification, notif_count, notif_elapsed) = match self
                .pane_state_for_tab(tab_id)
            {
                Some(ps) => (
                    ps.notification_start.is_some(),
                    ps.notification_count,
                    ps.notification_start
                        .map(|s| Instant::now().duration_since(s).as_secs_f32()),
                ),
                None => (false, 0, None),
            };

            let has_any_claude = info.map_or(false, |i| !i.pane_claude_info.is_empty());
            let single_pane_claude = if !has_multiple_panes && has_any_claude {
                info.and_then(|i| i.pane_claude_info.values().next())
            } else {
                None
            };
            // Multi-pane tabs borrow the first claude pane's status for the
            // tab tile's tint; per-pane detail lives on the sub-tiles.
            let claude_for_accent =
                single_pane_claude.or_else(|| info.and_then(|i| i.pane_claude_info.values().next()));

            // The tile names the PROJECT (last path component), not the model:
            // identity is what the rail shows, detail lives in the flyout.
            let label_src = info
                .map(|i| i.cwd_short.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or(&title);
            let label = last_path_component(label_src);

            let (icon, icon_color) = if has_any_claude {
                ("\u{2733}", theme.accent_orange) // ✳
            } else if is_active {
                ("\u{276f}", theme.text_primary) // ❯
            } else {
                ("\u{276f}", theme.text_secondary)
            };

            let border_color = if let Some(c) = claude_for_accent {
                status_border(c, &theme)
            } else if is_active {
                theme.accent_blue
            } else {
                theme.border_subtle
            };

            // Notification pulse: blend the tile bg toward red, same period as
            // the old card pulse; paint_tab_sidebar schedules the frames.
            let base_bg = if is_active {
                theme.bg_elevated
            } else {
                theme.bg_base
            };
            let bg = match notif_elapsed {
                Some(elapsed) if has_notification => {
                    let period = 1.5_f32;
                    let t = ((elapsed * std::f32::consts::TAU / period).sin() + 1.0) / 2.0;
                    let blend = t * 0.35;
                    LinearRgba::with_components(
                        base_bg.0 + (theme.accent_red.0 - base_bg.0) * blend,
                        base_bg.1 + (theme.accent_red.1 - base_bg.1) * blend,
                        base_bg.2 + (theme.accent_red.2 - base_bg.2) * blend,
                        base_bg.3,
                    )
                }
                _ => base_bg,
            };

            let mut tile = TileArgs::new(&font, &title_font, &metrics, &theme, sidebar_width);
            tile.icon = icon.to_string();
            tile.icon_color = icon_color;
            tile.label = label;
            tile.label_color = if is_active {
                theme.text_primary
            } else {
                theme.text_secondary
            };
            if has_notification && notif_count > 0 {
                tile.left_hint = Some((format!("{}", notif_count.min(9)), theme.accent_red));
            }
            if let Some(c) = claude_for_accent {
                tile.right_hint = Some(("\u{25cf}".to_string(), status_color(c, &theme)));
            }
            tile.border_color = border_color;
            tile.bg = bg;
            tile.hover_bg = Some(theme.bg_elevated);
            tile.ctx_pct = single_pane_claude.and_then(|c| c.context_pct);
            tile.item = Some(TabSidebarItem::Tab {
                tab_idx,
                active: is_active,
            });
            tab_elements.push(build_tile(tile));

            // Pane sub-tiles for split tabs.
            if has_multiple_panes {
                for pane_pos in &panes {
                    let pane = &pane_pos.pane;
                    let pane_id = pane.pane_id();
                    let pane_title = pane.get_title();
                    let pane_cwd = pane
                        .get_current_working_dir(CachePolicy::AllowStale)
                        .and_then(|u| {
                            if u.scheme() == "file" {
                                Some(shorten_path(u.path()))
                            } else {
                                None
                            }
                        });
                    let is_active_pane = pane_pos.is_active && is_active;
                    let pane_claude = info.and_then(|i| i.pane_claude_info.get(&pane_id));

                    let pane_label_src = pane_cwd
                        .as_deref()
                        .filter(|s| !s.is_empty())
                        .map(last_path_component)
                        .unwrap_or_else(|| pane_title.clone());

                    let mut sub = TileArgs::new(&font, &title_font, &metrics, &theme, sidebar_width);
                    sub.half = true;
                    if pane_claude.is_some() {
                        sub.icon = "\u{2733}".to_string();
                        sub.icon_color = theme.accent_orange;
                    } else {
                        sub.icon = "\u{2514}".to_string(); // └
                        sub.icon_color = theme.text_tertiary;
                    }
                    sub.label = pane_label_src;
                    sub.label_color = if is_active_pane {
                        theme.text_primary
                    } else {
                        theme.text_secondary
                    };
                    if let Some(c) = pane_claude {
                        sub.right_hint =
                            Some(("\u{25cf}".to_string(), status_color(c, &theme)));
                        sub.border_color = status_border(c, &theme);
                    } else if is_active_pane {
                        sub.border_color = theme.accent_blue;
                    } else {
                        sub.border_color = theme.border_subtle;
                    }
                    sub.bg = if is_active_pane {
                        theme.bg_elevated
                    } else {
                        theme.bg_base
                    };
                    sub.hover_bg = Some(theme.bg_elevated);
                    sub.item = Some(TabSidebarItem::Pane {
                        tab_idx,
                        pane_idx: pane_pos.index,
                    });
                    tab_elements.push(build_tile(sub));
                }
            }
        }

        // Layout context used to compute the final tree.
        let dpi = self.dimensions.dpi as f32;
        let context_probe = LayoutContext {
            width: config::DimensionContext {
                dpi,
                pixel_max: sidebar_width,
                pixel_cell: metrics.cell_size.width as f32,
            },
            height: config::DimensionContext {
                dpi,
                pixel_max: window_height,
                pixel_cell: metrics.cell_size.height as f32,
            },
            bounds: euclid::rect(0., 0., sidebar_width, window_height),
            metrics: &metrics,
            gl_state: self.render_state.as_ref().unwrap(),
            zindex: 10,
        };

        // Tmux tile groups, capped at roughly half the sidebar so the local
        // tiles (the primary content) cannot be squeezed out. Hidden tiles are
        // summarised; Ctrl+Shift+S still lists every session.
        let tile_budget = {
            let avail = (window_height * 0.5).max(0.);
            ((avail / (TILE_H + TILE_GAP)) as usize).max(2)
        };
        let agents_section = self.build_agents_section(
            &font,
            &title_font,
            &metrics,
            &theme,
            sidebar_width,
            tile_budget,
        );

        // Everything flows in document order; nothing is stretched to pin a
        // child to the window's bottom edge (a height prediction the layout is
        // free to exceed pushed the old new-tab button off-screen).
        let tabs_container = Element::new(&font, ElementContent::Children(tab_elements))
            .display(DisplayType::Block)
            .colors(ElementColors {
                border: BorderColor::default(),
                bg: InheritableColor::Inherited,
                text: InheritableColor::Inherited,
            });

        let mut sidebar_children = vec![tabs_container];
        if let Some(section) = agents_section {
            sidebar_children.push(section);
        }
        sidebar_children.push(build_widget_dock(
            &font,
            &title_font,
            &theme,
            sidebar_width,
            self.config.tmux.as_ref().map_or(false, |t| t.enabled),
        ));

        // Root container
        let root = Element::new(&font, ElementContent::Children(sidebar_children))
            .display(DisplayType::Block)
            .padding(BoxDimension {
                left: Dimension::Pixels(0.),
                right: Dimension::Pixels(0.),
                top: Dimension::Pixels(6.),
                bottom: Dimension::Pixels(0.),
            })
            .colors(ElementColors {
                border: BorderColor::default(),
                bg: theme.bg_surface.into(),
                text: theme.text_secondary.into(),
            })
            .min_width(Some(Dimension::Pixels(sidebar_width)));

        let mut computed = self.compute_element(&context_probe, &root)?;

        // Position sidebar below the title bar
        let x_offset = match self.config.tab_sidebar_position {
            TabSidebarPosition::Left => border.left.get() as f32,
            TabSidebarPosition::Right => {
                self.dimensions.pixel_width as f32
                    - sidebar_width
                    - border.right.get() as f32
            }
        };
        computed.translate(euclid::vec2(x_offset, sidebar_top));

        Ok(computed)
    }

    pub fn paint_tab_sidebar(
        &mut self,
        layers: &mut crate::quad::TripleLayerQuadAllocator,
    ) -> anyhow::Result<()> {
        use anyhow::Context;

        // Update sidebar metadata periodically
        self.update_sidebar_info();

        // Start the tmux discovery poller once the sidebar is live, so the
        // session list populates without the user opening the picker first.
        // The WebView sidebar does the same from push_webview_sidebar_state();
        // that function is #[cfg(windows)], so without this call the poller
        // never started on the GPU-rendered (non-Windows) path and the tmux
        // section stayed empty no matter how the boxes were configured.
        if self.config.tmux.as_ref().map_or(false, |t| t.enabled) {
            crate::tmux_discovery::ensure_running();
        }

        // Paint full-height background for the sidebar column
        let sidebar_width = self.tab_sidebar_width as f32;
        let window_height = self.dimensions.pixel_height as f32;
        let border = self.get_os_border();
        // Spec palette, not the window frame: the rail is fixed-color on both
        // renderers (see SidebarTheme).
        let bg_color = SidebarTheme::load().bg_surface;
        let bg_x = match self.config.tab_sidebar_position {
            TabSidebarPosition::Left => border.left.get() as f32,
            TabSidebarPosition::Right => {
                self.dimensions.pixel_width as f32
                    - sidebar_width
                    - border.right.get() as f32
            }
        };
        let bg_y = border.top.get() as f32;
        self.filled_rectangle(
            layers,
            1,
            euclid::rect(bg_x, bg_y, sidebar_width, window_height - bg_y),
            bg_color,
        )
        .context("sidebar background")?;

        // Resize handle on the inner edge of the sidebar
        let handle_width = 4.0f32;
        let handle_x = match self.config.tab_sidebar_position {
            TabSidebarPosition::Left => bg_x + sidebar_width - handle_width,
            TabSidebarPosition::Right => bg_x,
        };
        // Register the resize handle as a UI item for hit-testing
        self.ui_items.push(UIItem {
            x: handle_x as usize,
            y: bg_y as usize,
            width: (handle_width * 2.0) as usize, // wider hit area
            height: (window_height - bg_y) as usize,
            item_type: UIItemType::TabSidebar(TabSidebarItem::ResizeHandle),
        });

        // Check if any tab has a notification — if so, force rebuild
        // each frame for the pulsing animation.
        // Collect tab IDs first, then check pane state separately to avoid
        // nested Mux borrows.
        let tab_ids: Vec<TabId> = {
            let mux = Mux::get();
            mux.get_window(self.mux_window_id)
                .map(|w| w.iter().map(|tab| tab.tab_id()).collect())
                .unwrap_or_default()
        };
        let has_flashing_tab = tab_ids.iter().any(|&tid| {
            self.pane_state_for_tab(tid)
                .map_or(false, |ps| ps.notification_start.is_some())
        });

        if has_flashing_tab {
            // Force rebuild so animation updates
            self.tab_sidebar.take();
            // Schedule next frame for smooth animation (~30fps)
            self.update_next_frame_time(Some(Instant::now() + Duration::from_millis(32)));
        }

        // Fallback fonts for the rail's glyph icons load asynchronously; an
        // element cached before they arrive keeps its blank shapes forever
        // (nothing re-shapes a cached element, so the ClearShapeCache heal
        // never fires for it) — tiles rendered as tall empty boxes until a
        // resize invalidated the cache. Rebuild every paint for the first
        // moments after window creation so late glyphs land on their own.
        if self.created.elapsed() < Duration::from_secs(3) {
            self.tab_sidebar.take();
            self.update_next_frame_time(Some(Instant::now() + Duration::from_millis(250)));
        }

        // The sidebar is cached until invalidated, so a poll that discovers new
        // agents would otherwise never reach the screen. Compare a cheap
        // fingerprint of the discovery snapshot and rebuild when it moves.
        if self.config.tmux.as_ref().map_or(false, |t| t.enabled) {
            let mut fingerprint = String::new();
            for snap in crate::tmux_discovery::snapshot() {
                // The box itself, before its sessions. A box whose probe has
                // not answered yet has no sessions to iterate, so keying only
                // on sessions made "box present but empty" indistinguishable
                // from "box absent" — a newly configured box stayed invisible
                // until something else happened to invalidate the sidebar, and
                // a box going unreachable kept painting its old rows. The
                // status matters for the same reason: it drives the header dot
                // and the error line, neither of which is a session.
                fingerprint.push_str(&snap.box_name);
                fingerprint.push('#');
                fingerprint.push_str(match snap.status {
                    crate::tmux_discovery::BoxStatus::Ok => "ok",
                    crate::tmux_discovery::BoxStatus::Pending => "pending",
                    crate::tmux_discovery::BoxStatus::RegistryOnly => "registry",
                    crate::tmux_discovery::BoxStatus::Unreachable(_) => "unreachable",
                });
                fingerprint.push('\n');
                for session in &snap.sessions {
                    fingerprint.push_str(&snap.box_name);
                    fingerprint.push(':');
                    fingerprint.push_str(&session.session);
                    if let Some(agent) = &session.agent {
                        fingerprint.push('/');
                        fingerprint.push_str(agent);
                    }
                    // Attach state changes how a row renders — dimmed, or
                    // folded away entirely — so it has to move the fingerprint
                    // or the cached sidebar keeps painting the old state.
                    if session.attached {
                        fingerprint.push_str("@attached");
                    }
                    fingerprint.push('\n');
                }
            }
            // Folding depends on which sessions this window currently hosts,
            // which changes when a pane attaches or detaches without the
            // discovery snapshot moving at all.
            let mut folded: Vec<String> = self
                .locally_attached_sessions()
                .into_iter()
                .map(|(b, s)| format!("{b}:{s}"))
                .collect();
            folded.sort();
            fingerprint.push_str("\x1efolded:");
            fingerprint.push_str(&folded.join(","));

            if self.tmux_sidebar_fingerprint != fingerprint {
                self.tmux_sidebar_fingerprint = fingerprint;
                self.tab_sidebar.take();
            }
        }

        if self.tab_sidebar.is_none() {
            let palette = self.palette().clone();
            let sidebar = self.build_tab_sidebar(&palette)?;
            self.tab_sidebar.replace(sidebar);
        }

        let computed = self.tab_sidebar.as_ref().unwrap();
        let ui_items = computed.ui_items();

        let gl_state = self.render_state.as_ref().unwrap();
        self.render_element(computed, gl_state, None)?;

        self.ui_items.extend(ui_items);

        // Detail flyout: painted last and its UI items pushed last, so reverse
        // hit-testing gives it the mouse over the terminal area it overlays.
        self.paint_sidebar_flyout(bg_x, bg_y)?;
        Ok(())
    }

    /// Paint the hover flyout next to the rail once the hover delay has
    /// elapsed. Rebuilt every paint while open — its data is live (claude
    /// status, tmux snapshot) and it only exists while hovered.
    fn paint_sidebar_flyout(&mut self, rail_x: f32, rail_y: f32) -> anyhow::Result<()> {
        let fly = match self.sidebar_flyout.clone() {
            Some(f) => f,
            None => {
                self.sidebar_flyout_rect = None;
                return Ok(());
            }
        };
        if fly.hover_since.elapsed() < crate::termwindow::SIDEBAR_FLYOUT_DELAY {
            // Not open yet: schedule the frame that will open it, so the delay
            // elapses without needing another mouse move.
            self.update_next_frame_time(Some(
                fly.hover_since + crate::termwindow::SIDEBAR_FLYOUT_DELAY,
            ));
            return Ok(());
        }

        let font = self.fonts.default_font()?;
        let title_font = self.fonts.sidebar_font()?;
        let metrics = RenderMetrics::with_font_metrics(&font.metrics());
        let theme = SidebarTheme::load();

        let element = match self.build_sidebar_flyout(&fly.item, &font, &title_font, &theme) {
            Some(e) => e,
            None => return Ok(()),
        };

        let window_w = self.dimensions.pixel_width as f32;
        let window_h = self.dimensions.pixel_height as f32;
        let dpi = self.dimensions.dpi as f32;
        let context = LayoutContext {
            width: config::DimensionContext {
                dpi,
                pixel_max: FLYOUT_W,
                pixel_cell: metrics.cell_size.width as f32,
            },
            height: config::DimensionContext {
                dpi,
                pixel_max: window_h,
                pixel_cell: metrics.cell_size.height as f32,
            },
            bounds: euclid::rect(0., 0., FLYOUT_W, window_h),
            metrics: &metrics,
            gl_state: self.render_state.as_ref().unwrap(),
            zindex: 50,
        };
        let mut computed = self.compute_element(&context, &element)?;

        let sidebar_width = self.tab_sidebar_width as f32;
        // Fuse the flyout to the TILE edge, not the rail edge: tiles are inset
        // by margin, and a dead strip between tile and flyout would make the
        // pointer resolve to no UI item on the way over — which closes the
        // flyout before it can be reached (user-reported). The flyout overlaps
        // the rail margin at zindex 50, and its accent border lands exactly on
        // the tile border.
        let tile_margin = ((sidebar_width - TILE_W) / 2.).max(2.);
        // Position from the flyout's ACTUAL painted width: the layout does not
        // honor min_width on this root element (it painted ~90px instead of
        // 264, leaving exactly that much gap when positioned by FLYOUT_W).
        // Anchoring the computed width to the tile edge abuts them always.
        let w = computed.bounds.width();
        let h = computed.bounds.height();
        let x = match self.config.tab_sidebar_position {
            TabSidebarPosition::Right => (rail_x + tile_margin - w).max(0.),
            TabSidebarPosition::Left => {
                (rail_x + sidebar_width - tile_margin).min(window_w - w)
            }
        };
        let y = fly.anchor_y.min(window_h - h - 8.).max(rail_y);
        computed.translate(euclid::vec2(x, y));
        self.sidebar_flyout_rect = Some((x, y, w, h));

        let gl_state = self.render_state.as_ref().unwrap();
        self.render_element(&computed, gl_state, None)?;
        self.ui_items.extend(computed.ui_items());
        Ok(())
    }

    /// Build the detail flyout for a rail item. Returns None when the anchor
    /// no longer resolves (tab closed, session gone) — the flyout simply does
    /// not paint that frame and the close logic clears it on the next move.
    fn build_sidebar_flyout(
        &self,
        item: &TabSidebarItem,
        font: &Rc<LoadedFont>,
        title_font: &Rc<LoadedFont>,
        theme: &SidebarTheme,
    ) -> Option<Element> {
        let mut children: Vec<Element> = vec![];
        let mut accent = theme.accent_orange;

        match item {
            TabSidebarItem::Tab { tab_idx, .. } | TabSidebarItem::Pane { tab_idx, .. } => {
                let mux = Mux::get();
                let mux_window = mux.get_window(self.mux_window_id)?;
                let tab = mux_window.iter().nth(*tab_idx)?;
                let tab_id = tab.tab_id();
                let info = self.tab_sidebar_info.get(&tab_id);
                let panes = tab.iter_panes_ignoring_zoom();

                // Resolve the concrete pane this flyout describes.
                let (pane, pane_cwd) = match item {
                    TabSidebarItem::Pane { pane_idx, .. } => {
                        let pos = panes.iter().find(|p| p.index == *pane_idx)?;
                        let cwd = pos
                            .pane
                            .get_current_working_dir(CachePolicy::AllowStale)
                            .and_then(|u| {
                                if u.scheme() == "file" {
                                    Some(shorten_path(u.path()))
                                } else {
                                    None
                                }
                            });
                        (pos.pane.clone(), cwd)
                    }
                    _ => {
                        let pos = panes
                            .iter()
                            .find(|p| p.is_active)
                            .or_else(|| panes.first())?;
                        (pos.pane.clone(), None)
                    }
                };
                let pane_id = pane.pane_id();
                let claude = info.and_then(|i| i.pane_claude_info.get(&pane_id));

                // Title: the model for a claude pane, the full path otherwise.
                let title_text = match claude.and_then(|c| c.model.as_deref()) {
                    Some(model) => model.to_string(),
                    None => info
                        .map(|i| i.cwd_short.clone())
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| tab.get_title()),
                };
                let title_color = if claude.is_some() {
                    theme.accent_orange
                } else {
                    theme.text_primary
                };
                children.push(
                    Element::new(font, ElementContent::Text(truncate_str(&title_text, 28)))
                        .display(DisplayType::Block)
                        .line_height(Some(1.2))
                        .colors(ElementColors {
                            border: BorderColor::default(),
                            bg: InheritableColor::Inherited,
                            text: title_color.into(),
                        }),
                );

                if let Some(c) = claude {
                    accent = status_color(c, theme);
                    build_claude_card_children(
                        &mut children,
                        c,
                        info,
                        pane_cwd.as_deref(),
                        font,
                        title_font,
                        theme.text_secondary,
                        theme.accent_red,
                    );
                } else {
                    // Plain shell: full cwd + git branch.
                    if let Some(cwd) = pane_cwd
                        .as_deref()
                        .or(info.map(|i| i.cwd_short.as_str()))
                        .filter(|s| !s.is_empty())
                    {
                        children.push(flyout_line(
                            title_font,
                            truncate_str(cwd, 36),
                            theme.text_secondary,
                        ));
                    }
                    if let Some(branch) = info.and_then(|i| i.git_branch.as_deref()) {
                        children.push(flyout_line(
                            title_font,
                            format!("\u{e0a0} {}", truncate_str(branch, 30)),
                            theme.text_secondary,
                        ));
                    }
                }

                // Notifications
                let (notif_count, muted) = {
                    let states = self.pane_state.borrow();
                    match states.get(&pane_id) {
                        Some(ps) => (ps.notification_count, ps.notifications_muted),
                        None => (0, false),
                    }
                };
                if notif_count > 0 {
                    children.push(flyout_line(
                        title_font,
                        format!("\u{25cf} {} notifications", notif_count),
                        theme.accent_red,
                    ));
                }

                // Zoom hint when the pane's effective scale is not 100%.
                let pane_scale = {
                    let states = self.pane_state.borrow();
                    states.get(&pane_id).map(|ps| ps.font_scale).unwrap_or(1.0)
                };
                let pct = (pane_scale * self.fonts.get_font_scale() * 100.0).round() as u16;
                if pct != 100 {
                    children.push(flyout_line(
                        title_font,
                        format!("zoom {}%  \u{00b7}  Ctrl+0 resets", pct),
                        theme.text_tertiary,
                    ));
                }

                // Action chips: close and mute. TabSidebar item types so
                // hovering them keeps the flyout open (see keep-flyout logic).
                let close_item = match item {
                    TabSidebarItem::Pane { .. } => UIItemType::TabSidebar(
                        TabSidebarItem::ClosePane {
                            pane_id: pane_id as usize,
                        },
                    ),
                    _ => UIItemType::CloseTab(*tab_idx),
                };
                let mute_label = if muted { "unmute" } else { "mute" };
                let chips = vec![
                    flyout_chip(title_font, "close", close_item, theme),
                    flyout_chip(
                        title_font,
                        mute_label,
                        UIItemType::TabSidebar(TabSidebarItem::MuteNotifications {
                            pane_id: pane_id as usize,
                        }),
                        theme,
                    ),
                ];
                children.push(
                    Element::new(title_font, ElementContent::Children(chips))
                        .display(DisplayType::Block)
                        .line_height(Some(1.6))
                        .padding(BoxDimension {
                            left: Dimension::Pixels(0.),
                            right: Dimension::Pixels(0.),
                            top: Dimension::Pixels(4.),
                            bottom: Dimension::Pixels(0.),
                        }),
                );
            }
            TabSidebarItem::TmuxSession { box_name, session } => {
                let snaps = crate::tmux_discovery::snapshot();
                let snap = snaps.iter().find(|s| &s.box_name == box_name)?;
                let sess = snap.sessions.iter().find(|s| &s.session == session)?;

                let name_color = if sess.agent_is_instance {
                    theme.accent_orange
                } else {
                    theme.text_primary
                };
                children.push(
                    Element::new(font, ElementContent::Text(truncate_str(session, 28)))
                        .display(DisplayType::Block)
                        .line_height(Some(1.2))
                        .colors(ElementColors {
                            border: BorderColor::default(),
                            bg: InheritableColor::Inherited,
                            text: name_color.into(),
                        }),
                );

                let status_text = match snap.status {
                    crate::tmux_discovery::BoxStatus::Ok => "reachable",
                    crate::tmux_discovery::BoxStatus::Unreachable(_) => "unreachable",
                    _ => "pending",
                };
                children.push(flyout_line(
                    title_font,
                    format!("{} \u{00b7} {}", box_name, status_text),
                    theme.text_secondary,
                ));
                children.push(flyout_line(
                    title_font,
                    format!(
                        "{} window{}{}",
                        sess.windows,
                        if sess.windows == 1 { "" } else { "s" },
                        if sess.attached { " \u{00b7} attached" } else { "" },
                    ),
                    theme.text_secondary,
                ));
                if let Some(agent) = &sess.agent {
                    let line = if sess.agent_is_instance {
                        format!("\u{21c4} {} (interconnect)", agent)
                    } else {
                        format!("running {}", agent)
                    };
                    children.push(flyout_line(title_font, line, theme.accent_orange));
                }

                if sess.attachable {
                    let chips = vec![flyout_chip(
                        title_font,
                        "attach in split",
                        UIItemType::TabSidebar(TabSidebarItem::TmuxSession {
                            box_name: box_name.clone(),
                            session: session.clone(),
                        }),
                        theme,
                    )];
                    children.push(
                        Element::new(title_font, ElementContent::Children(chips))
                            .display(DisplayType::Block)
                            .line_height(Some(1.6))
                            .padding(BoxDimension {
                                left: Dimension::Pixels(0.),
                                right: Dimension::Pixels(0.),
                                top: Dimension::Pixels(4.),
                                bottom: Dimension::Pixels(0.),
                            }),
                    );
                }
            }
            _ => return None,
        }

        let content_w = FLYOUT_W - 4. - 22.;
        Some(
            Element::new(font, ElementContent::Children(children))
                .display(DisplayType::Block)
                .item_type(UIItemType::TabSidebar(TabSidebarItem::Flyout))
                .padding(BoxDimension {
                    left: Dimension::Pixels(10.),
                    right: Dimension::Pixels(12.),
                    top: Dimension::Pixels(8.),
                    bottom: Dimension::Pixels(8.),
                })
                .border(BoxDimension {
                    left: Dimension::Pixels(1.),
                    right: Dimension::Pixels(3.),
                    top: Dimension::Pixels(1.),
                    bottom: Dimension::Pixels(1.),
                })
                .border_corners(Some(rounded_corners(0.25)))
                .colors(ElementColors {
                    border: BorderColor {
                        left: theme.border_default,
                        top: theme.border_default,
                        bottom: theme.border_default,
                        // The fused state edge: the flyout carries its anchor's
                        // status color on the rail-facing side.
                        right: accent,
                    },
                    bg: theme.bg_elevated.into(),
                    text: theme.text_primary.into(),
                })
                .min_width(Some(Dimension::Pixels(content_w)))
                .max_width(Some(Dimension::Pixels(content_w))),
        )
    }

    /// Check if a tab has notification state set on any of its panes.
    pub(crate) fn pane_state_for_tab(&self, tab_id: TabId) -> Option<std::cell::Ref<'_, crate::termwindow::PaneState>> {
        let mux = Mux::get();
        let tab = mux.get_tab(tab_id)?;
        let active_pane = tab.get_active_pane()?;
        let pane_id = active_pane.pane_id();
        let states = self.pane_state.borrow();
        if states.contains_key(&pane_id) {
            // Re-borrow to return Ref
            drop(states);
            Some(std::cell::Ref::map(self.pane_state.borrow(), |m| {
                &m[&pane_id]
            }))
        } else {
            None
        }
    }
}

/// Build the Claude Card body elements (status, project+branch, context bar, stats).
/// Appended to an existing `children` vec — the caller is responsible for the title line.
fn build_claude_card_children(
    children: &mut Vec<Element>,
    claude: &crate::termwindow::ClaudeSessionInfo,
    info: Option<&SidebarTabInfo>,
    pane_cwd: Option<&str>,
    font: &Rc<LoadedFont>,
    detail_font: &Rc<LoadedFont>,
    dimmed_color: LinearRgba,
    notif_color: LinearRgba,
) {
    use crate::termwindow::ClaudeStatus;

    // Status indicator
    let (status_icon, status_text, status_color) = match claude.status {
        Some(ClaudeStatus::Working) => (
            "\u{25b6}",  // ▶
            "working",
            LinearRgba::with_components(0.247, 0.725, 0.314, 1.0),
        ),
        Some(ClaudeStatus::WaitingInput) => (
            "\u{25cf}",  // ●
            "awaiting input",
            LinearRgba::with_components(0.824, 0.600, 0.133, 1.0),
        ),
        Some(ClaudeStatus::Idle) => (
            "\u{2714}",  // ✔
            "idle",
            LinearRgba::with_components(0.4, 0.4, 0.4, 1.0),
        ),
        Some(ClaudeStatus::Error) => (
            "\u{2717}",  // ✗
            "error",
            notif_color,
        ),
        None => (
            "\u{2714}",  // ✔
            "idle",
            LinearRgba::with_components(0.4, 0.4, 0.4, 1.0),
        ),
    };
    children.push(
        Element::new(
            font,
            ElementContent::Text(format!("{} {}", status_icon, status_text)),
        )
        .display(DisplayType::Block)
        .line_height(Some(0.9))
        .colors(ElementColors {
            border: BorderColor::default(),
            bg: InheritableColor::Inherited,
            text: status_color.into(),
        }),
    );

    // Worktree/project + branch — prefer per-pane CWD over tab-level
    let project = claude
        .worktree
        .as_deref()
        .or(pane_cwd)
        .or(info.map(|i| i.cwd_short.as_str()))
        .unwrap_or("");
    if !project.is_empty() {
        let project_line = if let Some(ref branch) =
            info.and_then(|i| i.git_branch.as_ref())
        {
            format!(
                "{} \u{e0a0} {}",
                truncate_str(project, 24),
                truncate_str(branch, 14)
            )
        } else {
            truncate_str(project, 38)
        };
        // Prefix a per-agent identity chip when the session has a worktree.
        // Same seed as the pane border (claude.worktree), so the sidebar chip
        // and the pane's border share the agent's color.
        let mut line_children = vec![];
        if let Some(seed) = claude.worktree.as_deref().filter(|s| !s.is_empty()) {
            let chip = crate::agent_color::agent_accent_color(seed, true);
            line_children.push(
                Element::new(detail_font, ElementContent::Text("\u{25cf} ".to_string()))
                    .display(DisplayType::Inline)
                    .colors(ElementColors {
                        border: BorderColor::default(),
                        bg: InheritableColor::Inherited,
                        text: chip.into(),
                    }),
            );
        }
        line_children.push(
            Element::new(detail_font, ElementContent::Text(project_line))
                .display(DisplayType::Inline)
                .colors(ElementColors {
                    border: BorderColor::default(),
                    bg: InheritableColor::Inherited,
                    text: dimmed_color.into(),
                }),
        );
        children.push(
            Element::new(detail_font, ElementContent::Children(line_children))
                .display(DisplayType::Block)
                .line_height(Some(0.9)),
        );
    }

    // Context window bar — use ASCII block chars that render reliably
    if let Some(pct) = claude.context_pct {
        let bar_width = 15;
        let filled = (pct as usize * bar_width) / 100;
        let empty = bar_width.saturating_sub(filled);
        let bar: String = "\u{2588}".repeat(filled) + &"\u{2591}".repeat(empty);
        let bar_text = format!("{} {}%", bar, pct);
        let bar_color = if pct >= 90 {
            notif_color
        } else if pct >= 70 {
            LinearRgba::with_components(0.824, 0.600, 0.133, 1.0)
        } else {
            dimmed_color
        };
        children.push(
            Element::new(detail_font, ElementContent::Text(bar_text))
                .display(DisplayType::Block)
                .line_height(Some(0.9))
                .colors(ElementColors {
                    border: BorderColor::default(),
                    bg: InheritableColor::Inherited,
                    text: bar_color.into(),
                }),
        );
    }

    // Cost + duration (dimmed), then lines added/removed (colored)
    let mut stats = vec![];
    if let Some(cost) = claude.cost_usd {
        stats.push(format!("${:.2}", cost));
    }
    if let Some(ms) = claude.duration_ms {
        let mins = ms / 60_000;
        if mins > 0 {
            stats.push(format!("{}m", mins));
        }
    }
    let has_line_stats = {
        let added = claude.lines_added.unwrap_or(0);
        let removed = claude.lines_removed.unwrap_or(0);
        added > 0 || removed > 0
    };
    if !stats.is_empty() || has_line_stats {
        // Build an inline container so cost/duration and colored +/- sit on one line
        let mut stat_children = vec![];
        if !stats.is_empty() {
            let separator = if has_line_stats { " \u{00b7} " } else { "" };
            stat_children.push(
                Element::new(
                    detail_font,
                    ElementContent::Text(format!("{}{}", stats.join(" \u{00b7} "), separator)),
                )
                .display(DisplayType::Inline)
                .colors(ElementColors {
                    border: BorderColor::default(),
                    bg: InheritableColor::Inherited,
                    text: dimmed_color.into(),
                }),
            );
        }
        if has_line_stats {
            let added = claude.lines_added.unwrap_or(0);
            let removed = claude.lines_removed.unwrap_or(0);
            let green = LinearRgba::with_components(0.247, 0.725, 0.314, 1.0);
            let red = LinearRgba::with_components(0.973, 0.318, 0.286, 1.0);
            if added > 0 {
                stat_children.push(
                    Element::new(
                        detail_font,
                        ElementContent::Text(format!("+{}", added)),
                    )
                    .display(DisplayType::Inline)
                    .colors(ElementColors {
                        border: BorderColor::default(),
                        bg: InheritableColor::Inherited,
                        text: green.into(),
                    }),
                );
            }
            if added > 0 && removed > 0 {
                stat_children.push(
                    Element::new(
                        detail_font,
                        ElementContent::Text(" ".to_string()),
                    )
                    .display(DisplayType::Inline)
                    .colors(ElementColors {
                        border: BorderColor::default(),
                        bg: InheritableColor::Inherited,
                        text: dimmed_color.into(),
                    }),
                );
            }
            if removed > 0 {
                stat_children.push(
                    Element::new(
                        detail_font,
                        ElementContent::Text(format!("-{}", removed)),
                    )
                    .display(DisplayType::Inline)
                    .colors(ElementColors {
                        border: BorderColor::default(),
                        bg: InheritableColor::Inherited,
                        text: red.into(),
                    }),
                );
            }
        }
        children.push(
            Element::new(detail_font, ElementContent::Children(stat_children))
                .display(DisplayType::Block)
                .line_height(Some(0.9)),
        );
    }
}

/// Get the accent color for a Claude session based on its status.
fn claude_status_accent(claude: &crate::termwindow::ClaudeSessionInfo, active: bool) -> LinearRgba {
    use crate::termwindow::ClaudeStatus;
    let base = match claude.status {
        Some(ClaudeStatus::Working) => LinearRgba::with_components(0.247, 0.725, 0.314, 1.0),      // green
        Some(ClaudeStatus::WaitingInput) => LinearRgba::with_components(0.824, 0.600, 0.133, 1.0),  // yellow
        Some(ClaudeStatus::Idle) => LinearRgba::with_components(0.4, 0.4, 0.4, 1.0),          // gray
        Some(ClaudeStatus::Error) => LinearRgba::with_components(0.973, 0.318, 0.286, 1.0),         // red
        None => LinearRgba::with_components(0.4, 0.4, 0.4, 1.0),                                     // gray (idle default)
    };
    if active {
        base
    } else {
        LinearRgba::with_components(base.0 * 0.7, base.1 * 0.7, base.2 * 0.7, 0.6)
    }
}

impl crate::TermWindow {
    /// Push sidebar state to the WebView if data has changed,
    /// reposition the WebView if geometry changed, and drain IPC queue.
    #[cfg(windows)]
    pub fn push_webview_sidebar_state(&mut self) {
        if self.webview_sidebar.is_none() {
            return;
        }

        // Start the tmux discovery poller once the sidebar is live, so the
        // session list populates without the user opening the picker first.
        if self.config.tmux.as_ref().map_or(false, |t| t.enabled) {
            crate::tmux_discovery::ensure_running();
        }

        // 1. Drain queued IPC messages (safe here — outside Win32 message handlers)
        let messages: Vec<String> = if let Some(ref wv) = self.webview_sidebar {
            let mut q = wv.ipc_queue.lock().unwrap();
            q.drain(..).collect()
        } else {
            vec![]
        };
        for msg in messages {
            self.handle_sidebar_ipc(&msg);
        }

        // 2. Reposition on every paint if geometry changed
        let (x, y, w, h) = self.compute_sidebar_geometry();
        if let Some(ref mut wv) = self.webview_sidebar {
            wv.reposition(x, y, w as u16, h as u16);
        }

        // 3. Push state if data changed
        let json = self.serialize_sidebar_state();
        if let Some(ref mut wv) = self.webview_sidebar {
            wv.push_state(&json);
        }
    }

    /// Paint the solid background behind the WebView sidebar area.
    #[cfg(windows)]
    pub fn paint_sidebar_background(
        &mut self,
        layers: &mut crate::quad::TripleLayerQuadAllocator,
    ) -> anyhow::Result<()> {
        use anyhow::Context;
        let sidebar_width = self.tab_sidebar_width as f32;
        let border = self.get_os_border();
        let bg_y = border.top.get() as f32;
        let window_height = self.dimensions.pixel_height as f32;
        let bg_color = self.config.window_frame.inactive_titlebar_bg.to_linear();
        let bg_x = match self.config.tab_sidebar_position {
            config::TabSidebarPosition::Left => border.left.get() as f32,
            config::TabSidebarPosition::Right => {
                self.dimensions.pixel_width as f32
                    - sidebar_width
                    - border.right.get() as f32
            }
        };
        self.filled_rectangle(
            layers,
            0,
            euclid::rect(bg_x, bg_y, sidebar_width, window_height - bg_y),
            bg_color,
        )
        .context("webview sidebar background")?;
        Ok(())
    }

    /// Register the resize handle UI item for mouse hit-testing (WebView path).
    #[cfg(windows)]
    pub fn register_sidebar_resize_handle(&mut self) {
        use crate::termwindow::{UIItem, UIItemType, TabSidebarItem};
        let sidebar_width = self.tab_sidebar_width as f32;
        let border = self.get_os_border();
        let bg_y = border.top.get() as f32;
        let window_height = self.dimensions.pixel_height as f32;
        let handle_width = 4.0f32;
        let bg_x = match self.config.tab_sidebar_position {
            config::TabSidebarPosition::Left => border.left.get() as f32,
            config::TabSidebarPosition::Right => {
                self.dimensions.pixel_width as f32
                    - sidebar_width
                    - border.right.get() as f32
            }
        };
        let handle_x = match self.config.tab_sidebar_position {
            config::TabSidebarPosition::Left => bg_x + sidebar_width - handle_width,
            config::TabSidebarPosition::Right => bg_x,
        };
        self.ui_items.push(UIItem {
            x: handle_x as usize,
            y: bg_y as usize,
            width: (handle_width * 2.0) as usize,
            height: (window_height - bg_y) as usize,
            item_type: UIItemType::TabSidebar(TabSidebarItem::ResizeHandle),
        });
    }
}

/// Shorten a file path: replace home dir with ~, show last 2 components.
fn shorten_path(path: &str) -> String {
    let path = path.trim_end_matches('/');

    // Try to replace home dir with ~
    let home = dirs_next::home_dir()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_default();
    let display_path = if !home.is_empty() && path.starts_with(&home) {
        format!("~{}", &path[home.len()..])
    } else {
        path.to_string()
    };

    // Show last 2 path components
    let parts: Vec<&str> = display_path.split('/').filter(|s| !s.is_empty()).collect();
    if parts.len() <= 2 {
        display_path
    } else {
        format!(".../{}", parts[parts.len() - 2..].join("/"))
    }
}

/// How long a memoized git-branch lookup stays fresh. The branch for a given
/// directory changes rarely, so this keeps the synchronous filesystem walk off
/// the per-second sidebar poll on the GUI thread.
const GIT_BRANCH_CACHE_TTL: Duration = Duration::from_secs(5);

#[allow(clippy::type_complexity)]
static GIT_BRANCH_CACHE: std::sync::OnceLock<
    parking_lot::Mutex<HashMap<String, (Instant, Option<String>)>>,
> = std::sync::OnceLock::new();

/// Find the git branch for `path`, memoized per directory with a short TTL so
/// the blocking filesystem walk runs at most once per directory per
/// `GIT_BRANCH_CACHE_TTL` instead of on every sidebar poll.
pub(crate) fn find_git_branch(path: &str) -> Option<String> {
    let cache = GIT_BRANCH_CACHE.get_or_init(|| parking_lot::Mutex::new(HashMap::new()));

    if let Some((updated, branch)) = cache.lock().get(path) {
        if updated.elapsed() < GIT_BRANCH_CACHE_TTL {
            return branch.clone();
        }
    }

    let start = Instant::now();
    let branch = find_git_branch_uncached(path);
    let elapsed = start.elapsed();
    if elapsed > Duration::from_millis(100) {
        log::warn!(
            "find_git_branch({}) filesystem walk took {:?} on the GUI thread",
            path,
            elapsed
        );
    }

    cache
        .lock()
        .insert(path.to_string(), (Instant::now(), branch.clone()));
    branch
}

/// Find the git branch by walking up from the given directory
/// and reading .git/HEAD. Handles worktrees, where `.git` is a file
/// containing a `gitdir: <path>` pointer to the real git dir.
fn find_git_branch_uncached(path: &str) -> Option<String> {
    // On Windows/WSL, handle path conversion
    let mut dir = std::path::PathBuf::from(path);
    for _ in 0..20 {
        let dot_git = dir.join(".git");
        let git_head = if dot_git.is_file() {
            // Worktree: .git is a file "gitdir: /path/to/repo/.git/worktrees/<name>"
            match std::fs::read_to_string(&dot_git) {
                Ok(content) => match content.trim().strip_prefix("gitdir:") {
                    Some(gitdir) => std::path::PathBuf::from(gitdir.trim()).join("HEAD"),
                    None => dot_git.join("HEAD"),
                },
                Err(_) => dot_git.join("HEAD"),
            }
        } else {
            dot_git.join("HEAD")
        };
        if let Ok(content) = std::fs::read_to_string(&git_head) {
            let content = content.trim();
            if let Some(branch) = content.strip_prefix("ref: refs/heads/") {
                return Some(branch.to_string());
            }
            // Detached HEAD
            return Some(content.chars().take(8).collect());
        }
        if !dir.pop() {
            break;
        }
    }
    None
}

/// Uniform rounded-corner set for tiles and the flyout.
fn rounded_corners(size: f32) -> Corners {
    Corners {
        top_left: SizedPoly {
            width: Dimension::Cells(size),
            height: Dimension::Cells(size),
            poly: TOP_LEFT_ROUNDED_CORNER,
        },
        top_right: SizedPoly {
            width: Dimension::Cells(size),
            height: Dimension::Cells(size),
            poly: TOP_RIGHT_ROUNDED_CORNER,
        },
        bottom_left: SizedPoly {
            width: Dimension::Cells(size),
            height: Dimension::Cells(size),
            poly: BOTTOM_LEFT_ROUNDED_CORNER,
        },
        bottom_right: SizedPoly {
            width: Dimension::Cells(size),
            height: Dimension::Cells(size),
            poly: BOTTOM_RIGHT_ROUNDED_CORNER,
        },
    }
}

fn text_only(c: LinearRgba) -> ElementColors {
    ElementColors {
        border: BorderColor::default(),
        bg: InheritableColor::Inherited,
        text: c.into(),
    }
}

fn inline_mono(font: &Rc<LoadedFont>, text: String, color: LinearRgba) -> Element {
    Element::new(font, ElementContent::Text(text))
        .display(DisplayType::Inline)
        .colors(text_only(color))
}

/// Last path component of a shortened cwd — the project name the tile shows.
fn last_path_component(s: &str) -> String {
    s.trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|c| !c.is_empty())
        .unwrap_or(s)
        .to_string()
}

/// Inputs for one rail tile. Everything defaults to a plain resting tile;
/// callers set what differs.
struct TileArgs<'a> {
    font: &'a Rc<LoadedFont>,
    label_font: &'a Rc<LoadedFont>,
    metrics: &'a RenderMetrics,
    theme: &'a SidebarTheme,
    sidebar_width: f32,
    icon: String,
    icon_color: LinearRgba,
    label: String,
    label_color: LinearRgba,
    /// Red notification count, left end of the icon line.
    left_hint: Option<(String, LinearRgba)>,
    /// Status dot / window count, right end of the icon line.
    right_hint: Option<(String, LinearRgba)>,
    border_color: LinearRgba,
    bg: LinearRgba,
    hover_bg: Option<LinearRgba>,
    hover_border: Option<LinearRgba>,
    ctx_pct: Option<u8>,
    item: Option<TabSidebarItem>,
    /// Pane sub-tile: narrower, single line.
    half: bool,
}

impl<'a> TileArgs<'a> {
    fn new(
        font: &'a Rc<LoadedFont>,
        label_font: &'a Rc<LoadedFont>,
        metrics: &'a RenderMetrics,
        theme: &'a SidebarTheme,
        sidebar_width: f32,
    ) -> Self {
        Self {
            font,
            label_font,
            metrics,
            theme,
            sidebar_width,
            icon: String::new(),
            icon_color: theme.text_secondary,
            label: String::new(),
            label_color: theme.text_secondary,
            left_hint: None,
            right_hint: None,
            border_color: theme.border_subtle,
            bg: theme.bg_base,
            hover_bg: None,
            hover_border: None,
            ctx_pct: None,
            item: None,
            half: false,
        }
    }
}

/// One rail tile: bordered, rounded, centered in the rail. Full tiles stack
/// icon over label with hints on the icon line; half tiles are a single
/// icon+label line. All alignment is fixed mono columns — no floats, so the
/// Float::Right/max_width clamp trap cannot reappear here.
fn build_tile(a: TileArgs) -> Element {
    let theme = a.theme;
    let tile_w = if a.half { TILE_HALF_W } else { TILE_W };
    let margin_x = ((a.sidebar_width - tile_w) / 2.).max(2.);
    let cell_w = a.metrics.cell_size.width as f32;
    let inner_w = tile_w - 2. - 4.;
    let cols = if cell_w > 0. {
        ((inner_w / cell_w) as usize).max(4)
    } else {
        6
    };

    let mut lines: Vec<Element> = vec![];

    if a.half {
        let mut segs = vec![inline_mono(
            a.font,
            format!("{} ", a.icon),
            a.icon_color,
        )];
        segs.push(
            Element::new(
                a.label_font,
                ElementContent::Text(truncate_str(&a.label, 9)),
            )
            .display(DisplayType::Inline)
            .colors(text_only(a.label_color)),
        );
        if let Some((t, c)) = &a.right_hint {
            segs.push(
                Element::new(a.label_font, ElementContent::Text(format!(" {}", t)))
                    .display(DisplayType::Inline)
                    .colors(text_only(*c)),
            );
        }
        lines.push(
            Element::new(a.font, ElementContent::Children(segs))
                .display(DisplayType::Block)
                .line_height(Some(1.1)),
        );
    } else {
        // Icon line: [hint][centered icon][hint] in fixed mono columns.
        let side = 2usize;
        let mid = cols.saturating_sub(side * 2).max(1);
        let (ltext, lcolor) = match &a.left_hint {
            Some((t, c)) => (format!("{:<w$}", truncate_str(t, side), w = side), *c),
            None => (" ".repeat(side), a.icon_color),
        };
        let (rtext, rcolor) = match &a.right_hint {
            Some((t, c)) => (format!("{:>w$}", truncate_str(t, side), w = side), *c),
            None => (" ".repeat(side), a.icon_color),
        };
        let segs = vec![
            inline_mono(a.font, ltext, lcolor),
            inline_mono(a.font, format!("{:^w$}", a.icon, w = mid), a.icon_color),
            inline_mono(a.font, rtext, rcolor),
        ];
        lines.push(
            Element::new(a.font, ElementContent::Children(segs))
                .display(DisplayType::Block)
                .line_height(Some(1.1)),
        );

        // Label line, approximately centered in label-font columns. The label
        // font is proportional, so `{:^}` centering is approximate — close
        // enough at 9-ish glyphs, and exact centering would need pixel
        // measurement the box model does not expose.
        let label_metrics = RenderMetrics::with_font_metrics(&a.label_font.metrics());
        let lcell = label_metrics.cell_size.width as f32;
        let label_cols = if lcell > 0. {
            ((inner_w / lcell) as usize).max(6)
        } else {
            9
        };
        lines.push(
            Element::new(
                a.label_font,
                ElementContent::Text(format!(
                    "{:^w$}",
                    truncate_str(&a.label, label_cols),
                    w = label_cols
                )),
            )
            .display(DisplayType::Block)
            .line_height(Some(1.0))
            .colors(text_only(a.label_color)),
        );

        // Context strip: colored spaces at a tiny line height read as a bar.
        if let Some(pct) = a.ctx_pct {
            let filled = (((pct.min(100) as f32) / 100.) * cols as f32).round() as usize;
            let filled = filled.min(cols);
            let bar_color = if pct >= 90 {
                theme.accent_red
            } else if pct >= 70 {
                theme.accent_yellow
            } else {
                theme.accent_orange
            };
            let mut segs = vec![];
            if filled > 0 {
                segs.push(
                    Element::new(a.font, ElementContent::Text(" ".repeat(filled)))
                        .display(DisplayType::Inline)
                        .colors(ElementColors {
                            border: BorderColor::default(),
                            bg: bar_color.into(),
                            text: InheritableColor::Inherited,
                        }),
                );
            }
            if cols > filled {
                segs.push(
                    Element::new(a.font, ElementContent::Text(" ".repeat(cols - filled)))
                        .display(DisplayType::Inline)
                        .colors(ElementColors {
                            border: BorderColor::default(),
                            bg: theme.border_subtle.into(),
                            text: InheritableColor::Inherited,
                        }),
                );
            }
            lines.push(
                Element::new(a.font, ElementContent::Children(segs))
                    .display(DisplayType::Block)
                    .line_height(Some(0.3)),
            );
        }
    }

    let vpad = if a.half { 2. } else { 4. };
    let mut tile = Element::new(a.font, ElementContent::Children(lines))
        .display(DisplayType::Block)
        .margin(BoxDimension {
            left: Dimension::Pixels(margin_x),
            right: Dimension::Pixels(margin_x),
            top: Dimension::Pixels(0.),
            bottom: Dimension::Pixels(TILE_GAP),
        })
        .padding(BoxDimension {
            left: Dimension::Pixels(2.),
            right: Dimension::Pixels(2.),
            top: Dimension::Pixels(vpad),
            bottom: Dimension::Pixels(vpad),
        })
        .border(BoxDimension {
            left: Dimension::Pixels(1.),
            right: Dimension::Pixels(1.),
            top: Dimension::Pixels(1.),
            bottom: Dimension::Pixels(1.),
        })
        .border_corners(Some(rounded_corners(0.3)))
        .colors(ElementColors {
            border: BorderColor::new(a.border_color),
            bg: a.bg.into(),
            text: a.label_color.into(),
        })
        .min_width(Some(Dimension::Pixels(tile_w)))
        .max_width(Some(Dimension::Pixels(tile_w)));

    if let Some(item) = a.item {
        tile = tile.item_type(UIItemType::TabSidebar(item));
    }
    if a.hover_bg.is_some() || a.hover_border.is_some() {
        tile = tile.hover_colors(Some(ElementColors {
            border: BorderColor::new(a.hover_border.unwrap_or(a.border_color)),
            bg: a.hover_bg.unwrap_or(a.bg).into(),
            text: a.label_color.into(),
        }));
    }
    tile
}

/// Zone eyebrow: uppercase group label with an optional status dot — replaces
/// the old machine_header. Shared by the local group and the tmux boxes so the
/// two read as one grouped system.
fn sidebar_eyebrow(
    label_font: &Rc<LoadedFont>,
    name: &str,
    dot: Option<LinearRgba>,
    theme: &SidebarTheme,
    sidebar_width: f32,
    stale: bool,
) -> Element {
    let dimf = if stale { 0.55 } else { 1.0 };
    let mut parts = vec![];
    if let Some(dot_color) = dot {
        parts.push(
            Element::new(label_font, ElementContent::Text("\u{25cf} ".to_string()))
                .display(DisplayType::Inline)
                .colors(text_only(dim(dot_color, dimf))),
        );
    }
    parts.push(
        Element::new(
            label_font,
            ElementContent::Text(truncate_str(&name.to_uppercase(), 10)),
        )
        .display(DisplayType::Inline)
        .colors(text_only(dim(theme.text_tertiary, dimf))),
    );

    Element::new(label_font, ElementContent::Children(parts))
        .display(DisplayType::Block)
        .line_height(Some(1.3))
        .padding(BoxDimension {
            left: Dimension::Pixels(6.),
            right: Dimension::Pixels(2.),
            top: Dimension::Pixels(7.),
            bottom: Dimension::Pixels(3.),
        })
        .colors(ElementColors {
            border: BorderColor::default(),
            bg: InheritableColor::Inherited,
            text: dim(theme.text_tertiary, dimf).into(),
        })
        .min_width(Some(Dimension::Pixels(sidebar_width)))
}

/// Bottom widget dock: new terminal, theme picker, tmux refresh.
fn build_widget_dock(
    font: &Rc<LoadedFont>,
    _title_font: &Rc<LoadedFont>,
    theme: &SidebarTheme,
    sidebar_width: f32,
    tmux_enabled: bool,
) -> Element {
    // No space wrappers and slim padding: three chips at mono size overflowed
    // the 90px dock line, and inline overflow clips trailing children — the
    // theme and refresh chips were silently missing from the dock.
    let chip = |icon: &str, item: TabSidebarItem| {
        Element::new(font, ElementContent::Text(icon.to_string()))
            .display(DisplayType::Inline)
            .item_type(UIItemType::TabSidebar(item))
            .padding(BoxDimension {
                left: Dimension::Pixels(5.),
                right: Dimension::Pixels(5.),
                top: Dimension::Pixels(2.),
                bottom: Dimension::Pixels(2.),
            })
            .colors(text_only(theme.text_tertiary))
            .hover_colors(Some(ElementColors {
                border: BorderColor::default(),
                bg: theme.bg_elevated.into(),
                text: theme.text_primary.into(),
            }))
    };

    let mut chips = vec![
        chip("+", TabSidebarItem::NewTabButton),
        chip("\u{25d0}", TabSidebarItem::ThemePickerButton), // ◐
    ];
    if tmux_enabled {
        chips.push(chip("\u{21bb}", TabSidebarItem::TmuxRefreshButton)); // ↻
    }

    Element::new(font, ElementContent::Children(chips))
        .display(DisplayType::Block)
        .line_height(Some(1.3))
        .padding(BoxDimension {
            left: Dimension::Pixels(4.),
            right: Dimension::Pixels(2.),
            top: Dimension::Pixels(5.),
            bottom: Dimension::Pixels(5.),
        })
        .border(BoxDimension {
            left: Dimension::Pixels(0.),
            right: Dimension::Pixels(0.),
            top: Dimension::Pixels(1.),
            bottom: Dimension::Pixels(0.),
        })
        .margin(BoxDimension {
            left: Dimension::Pixels(0.),
            right: Dimension::Pixels(0.),
            top: Dimension::Pixels(8.),
            bottom: Dimension::Pixels(0.),
        })
        .colors(ElementColors {
            border: BorderColor::new(theme.border_subtle),
            bg: InheritableColor::Inherited,
            text: theme.text_tertiary.into(),
        })
        .min_width(Some(Dimension::Pixels(sidebar_width)))
}

/// A dim single line inside the flyout body.
fn flyout_line(font: &Rc<LoadedFont>, text: String, color: LinearRgba) -> Element {
    Element::new(font, ElementContent::Text(text))
        .display(DisplayType::Block)
        .line_height(Some(1.1))
        .colors(text_only(color))
}

/// A small action chip inside the flyout (close / mute / attach).
fn flyout_chip(
    font: &Rc<LoadedFont>,
    label: &str,
    item: UIItemType,
    theme: &SidebarTheme,
) -> Element {
    Element::new(font, ElementContent::Text(format!(" {} ", label)))
        .display(DisplayType::Inline)
        .item_type(item)
        .padding(BoxDimension {
            left: Dimension::Pixels(4.),
            right: Dimension::Pixels(4.),
            top: Dimension::Pixels(1.),
            bottom: Dimension::Pixels(1.),
        })
        .colors(ElementColors {
            border: BorderColor::default(),
            bg: LinearRgba::with_components(1., 1., 1., 0.08).into(),
            text: theme.text_secondary.into(),
        })
        .hover_colors(Some(ElementColors {
            border: BorderColor::default(),
            bg: LinearRgba::with_components(1., 1., 1., 0.16).into(),
            text: theme.text_primary.into(),
        }))
}

fn truncate_str(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars - 1).collect();
        format!("{}\u{2026}", truncated)
    }
}

/// Build a `ClaudeSessionInfo` for a pane if it hosts a Claude Code session,
/// otherwise `None`. Shared by the tab sidebar and the agent dashboard so
/// detection and user-var parsing never diverge.
///
/// Detect Claude via:
/// 1. Foreground process name ("claude" / "claude-code")
/// 2. Pane title containing "claude"
/// 3. Any process in the full tree matching Claude
/// 4. User vars with a non-empty claude_status (needed for WSL and
///    SSH where local process inspection only sees wslhost.exe /
///    ssh.exe and the real Claude process is on the other side).
///    We require a NON-EMPTY value (not just key presence) so the
///    SessionEnd hook can clear the card in-band by emitting an
///    empty claude_status — process-based clearing can't reach a
///    remote session. Do NOT key off other claude_* vars (e.g.
///    claude_model): they are never cleared and would make the
///    card persist after Claude exits (see commit 8d2de52).
pub(crate) fn claude_info_for_pane(
    pane: &std::sync::Arc<dyn mux::pane::Pane>,
) -> Option<crate::termwindow::ClaudeSessionInfo> {
    use crate::termwindow::{ClaudeSessionInfo, ClaudeStatus};

    // Freeze diagnostics: warn if process enumeration is slow (from origin's
    // freeze-diagnostics work, preserved through the refactor into this helper).
    let proc_start = Instant::now();
    let process_name = pane.get_foreground_process_name(CachePolicy::AllowStale);
    let proc_elapsed = proc_start.elapsed();
    if proc_elapsed > Duration::from_millis(100) {
        log::warn!(
            "claude_info_for_pane: get_foreground_process_name took {:?} for pane {}",
            proc_elapsed,
            pane.pane_id()
        );
    }
    let pane_title = pane.get_title();
    let user_vars = pane.copy_user_vars();
    let tree_start = Instant::now();
    let tree_names = pane.get_process_names_in_tree(CachePolicy::AllowStale);
    let tree_elapsed = tree_start.elapsed();
    if tree_elapsed > Duration::from_millis(100) {
        log::warn!(
            "claude_info_for_pane: get_process_names_in_tree took {:?} for pane {}",
            tree_elapsed,
            pane.pane_id()
        );
    }
    let has_active_claude_vars = user_vars
        .get("claude_status")
        .map_or(false, |v| !v.is_empty());
    let is_claude = process_name.as_deref().map_or(false, is_claude_process)
        || is_claude_title(&pane_title)
        || tree_names.iter().any(|n| is_claude_process(n))
        || has_active_claude_vars;

    if !is_claude {
        return None;
    }

    let status = user_vars.get("claude_status").map(|s| match s.as_str() {
        "working" => ClaudeStatus::Working,
        "waiting_input" => ClaudeStatus::WaitingInput,
        "idle" => ClaudeStatus::Idle,
        "error" => ClaudeStatus::Error,
        _ => ClaudeStatus::Working,
    });
    Some(ClaudeSessionInfo {
        model: user_vars.get("claude_model").cloned(),
        context_pct: user_vars
            .get("claude_context_pct")
            .and_then(|v| v.parse().ok()),
        cost_usd: user_vars.get("claude_cost").and_then(|v| v.parse().ok()),
        duration_ms: user_vars
            .get("claude_duration_ms")
            .and_then(|v| v.parse().ok()),
        lines_added: user_vars
            .get("claude_lines_added")
            .and_then(|v| v.parse().ok()),
        lines_removed: user_vars
            .get("claude_lines_removed")
            .and_then(|v| v.parse().ok()),
        worktree: user_vars.get("claude_worktree").cloned(),
        status,
        host: user_vars.get("claude_host").cloned(),
    })
}

/// Detect Claude Code by foreground process name.
fn is_claude_process(name: &str) -> bool {
    let basename = std::path::Path::new(name)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(name)
        .to_lowercase();
    let basename = basename.strip_suffix(".exe").unwrap_or(&basename);
    matches!(basename, "claude" | "claude-code")
}

/// Detect Claude Code by terminal title.
fn is_claude_title(title: &str) -> bool {
    let lower = title.to_lowercase();
    lower.contains("claude code")
        || lower.contains("claude-code")
        || lower.starts_with("claude ")
        || lower == "claude"
}
