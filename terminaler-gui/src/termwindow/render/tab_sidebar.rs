use crate::customglyph::*;
use crate::termwindow::box_model::*;
use crate::termwindow::render::corners::{
    BOTTOM_LEFT_ROUNDED_CORNER, BOTTOM_RIGHT_ROUNDED_CORNER, TOP_LEFT_ROUNDED_CORNER,
    TOP_RIGHT_ROUNDED_CORNER,
};
use crate::termwindow::{SidebarTabInfo, TabSidebarItem, UIItem, UIItemType};
use crate::utilsprites::RenderMetrics;
use config::{Dimension, TabBarColor, TabBarColors, TabSidebarPosition};
use mux::pane::CachePolicy;
use mux::tab::TabId;
use mux::Mux;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};
use terminaler_font::LoadedFont;
use terminaler_term::color::ColorPalette;
use window::color::LinearRgba;

const X_BUTTON: &[Poly] = &[
    Poly {
        path: &[
            PolyCommand::MoveTo(BlockCoord::One, BlockCoord::Zero),
            PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::One),
        ],
        intensity: BlockAlpha::Full,
        style: PolyStyle::Outline,
    },
    Poly {
        path: &[
            PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Zero),
            PolyCommand::LineTo(BlockCoord::One, BlockCoord::One),
        ],
        intensity: BlockAlpha::Full,
        style: PolyStyle::Outline,
    },
];

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

    fn build_agents_section(
        &self,
        font: &Rc<LoadedFont>,
        title_font: &Rc<LoadedFont>,
        metrics: &RenderMetrics,
        palette: &ColorPalette,
        bg_color: LinearRgba,
        text_color: LinearRgba,
        active_tab_colors: TabBarColor,
        sidebar_width: f32,
        row_budget: usize,
    ) -> Option<Element> {
        if !self.config.tmux.as_ref().map_or(false, |t| t.enabled) {
            return None;
        }

        let snaps = crate::tmux_discovery::snapshot();
        let row_count: usize = snaps.iter().map(|s| s.sessions.len()).sum();
        if row_count == 0 {
            return None;
        }

        // Palette matches the WebView sidebar's CSS variables so the two
        // platforms look like the same product. Hardcoded rather than pulled
        // from the terminal palette for exactly that reason: the Windows
        // sidebar is fixed-colour too, and the section should not restyle
        // itself per colour scheme when its Windows counterpart does not.
        // sRGB -> linear, the same transfer function SrgbaTuple::to_linear
        // applies; done inline to keep this helper dependency-free.
        let srgb_to_linear = |c: f32| -> f32 {
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        let rgb = |r: u8, g: u8, b: u8| -> LinearRgba {
            LinearRgba::with_components(
                srgb_to_linear(r as f32 / 255.),
                srgb_to_linear(g as f32 / 255.),
                srgb_to_linear(b as f32 / 255.),
                1.,
            )
        };
        let accent_orange = rgb(0xdb, 0x8b, 0x0b);
        let accent_green = rgb(0x3f, 0xb9, 0x50);
        let text_primary = rgb(0xe0, 0xe0, 0xe0);
        let text_secondary = rgb(0x99, 0x99, 0x99);
        let text_tertiary = rgb(0x66, 0x66, 0x66);
        let border_subtle = rgb(0x2e, 0x2e, 0x2e);
        let bg_base = rgb(0x12, 0x12, 0x12);

        // How many character cells fit inside a row card: the sidebar minus the
        // row's margins (10+6), padding (6+6) and border (1+1), and a further
        // 10px of slack. Derived rather than hardcoded so a resized sidebar
        // re-flows the columns.
        //
        // The slack is what keeps the floated agent badge inside the card.
        // Float::Right anchors to the content extent, which knows nothing about
        // the row's padding and border, so without it the badge overhung the
        // right edge and was clipped by the sidebar.
        let cell_w = metrics.cell_size.width as f32;
        let row_cols = if cell_w > 0. {
            (((sidebar_width - 40.) / cell_w).floor() as usize).max(12)
        } else {
            24
        };

        let mut children = vec![];
        let mut rows_emitted = 0usize;
        let mut rows_hidden = 0usize;

        // Sessions this window already hosts in a pane. Their rows are folded
        // away rather than listed a second time; see locally_attached_sessions.
        let folded = self.locally_attached_sessions();

        for snap in &snaps {
            if snap
                .sessions
                .iter()
                .all(|s| folded.contains(&(snap.box_name.clone(), s.session.clone())))
            {
                // Every session on this box is already open here, so the box
                // header would introduce a heading with nothing under it.
                continue;
            }
            if snap.sessions.is_empty() {
                continue;
            }
            // Each box header costs a row's worth of budget too.
            if rows_emitted + 1 >= row_budget {
                rows_hidden += snap.sessions.len();
                continue;
            }

            // Box header: a status dot and the machine name, mirroring
            // .tmux-box-header / .tmux-status-dot.
            let dot_color = match snap.status {
                crate::tmux_discovery::BoxStatus::Ok => accent_green,
                crate::tmux_discovery::BoxStatus::Unreachable(_) => rgb(0xf8, 0x51, 0x49),
                _ => text_tertiary,
            };
            children.push(machine_header(
                font,
                &snap.box_name,
                Some(dot_color),
                text_secondary,
                bg_color,
                sidebar_width,
            ));

            for session in &snap.sessions {
                // Already open as a pane in this window: the local row is the
                // canonical one, so this row folds away entirely.
                if folded.contains(&(snap.box_name.clone(), session.session.clone())) {
                    continue;
                }
                if rows_emitted >= row_budget {
                    rows_hidden += 1;
                    continue;
                }
                // Attached by some other client — another terminal, another
                // machine. tmux is multi-client so attaching here is still
                // valid; the row stays clickable but recedes.
                let attached_elsewhere = session.attached;
                // Columns are sized from the sidebar width rather than fixed,
                // and share it by priority: the project directory (the tmux
                // session name) is what identifies a row, so it takes the slack
                // and is the last thing to be truncated; the agent badge takes
                // only what it needs. Every row still pads to the same totals,
                // so widening the sidebar reveals more of the name without
                // changing the row's shape.
                let badge_cols = session
                    .agent
                    .as_deref()
                    .map(|a| a.chars().count().min(12))
                    .unwrap_or(0);
                // The badge floats right, so the name only has to reserve
                // enough width to keep the two from overlapping.
                let badge_field = if badge_cols > 0 { badge_cols + 2 } else { 0 };
                let name_cols = row_cols.saturating_sub(badge_field).max(6);

                let mut row_children = vec![Element::new(
                    font,
                    // Padded to name_cols so every row reserves the same width
                    // for the name and the floated badge always clears it.
                    ElementContent::Text(format!(
                        "{:<width$}",
                        truncate_str(&session.session, name_cols),
                        width = name_cols
                    )),
                )
                .display(DisplayType::Inline)
                .colors(ElementColors {
                    border: BorderColor::default(),
                    bg: InheritableColor::Inherited,
                    text: if attached_elsewhere {
                        text_tertiary
                    } else if session.attachable {
                        text_primary
                    } else {
                        text_tertiary
                    }
                    .into(),
                })];

                if let Some(agent) = &session.agent {
                    // A named interconnect instance is the more specific fact,
                    // so it gets the filled orange badge with dark text; a
                    // generic agent type stays muted. Matches
                    // .tmux-session-agent{,-instance}.
                    let (badge_bg, badge_fg) = if session.agent_is_instance {
                        (accent_orange, bg_base)
                    } else {
                        (border_subtle, accent_orange)
                    };
                    row_children.push(
                        // title_font is the window-frame font, a size down from
                        // the terminal font: the badge is supporting detail, so
                        // it should not compete with the project name.
                        Element::new(
                            title_font,
                            ElementContent::Text(format!(
                                " {:<width$} ",
                                truncate_str(agent, badge_cols),
                                width = badge_cols
                            )),
                        )
                        .display(DisplayType::Inline)
                        // Pinned to the right edge rather than trailing the
                        // name. The badge is set in title_font, a size down
                        // from the name's font, so padding the name to
                        // name_cols cannot line the badges up: each row would
                        // end at a slightly different x. Floating right anchors
                        // every badge to the same edge whatever the name.
                        .float(Float::Right)
                        .colors(ElementColors {
                            border: BorderColor::default(),
                            bg: badge_bg.into(),
                            text: badge_fg.into(),
                        }),
                    );
                }

                // Indented card with a subtle border, hovering to orange —
                // .tmux-session-row and its :hover rule.
                let mut row = Element::new(font, ElementContent::Children(row_children))
                    .display(DisplayType::Block)
                    .line_height(Some(1.2))
                    .margin(BoxDimension {
                        left: Dimension::Pixels(10.),
                        right: Dimension::Pixels(6.),
                        top: Dimension::Pixels(2.),
                        bottom: Dimension::Pixels(2.),
                    })
                    .padding(BoxDimension {
                        left: Dimension::Pixels(6.),
                        right: Dimension::Pixels(6.),
                        top: Dimension::Pixels(2.),
                        bottom: Dimension::Pixels(2.),
                    })
                    .border(BoxDimension {
                        left: Dimension::Pixels(1.),
                        right: Dimension::Pixels(1.),
                        top: Dimension::Pixels(1.),
                        bottom: Dimension::Pixels(1.),
                    })
                    .colors(ElementColors {
                        border: BorderColor::new(border_subtle),
                        bg: border_subtle.into(),
                        text: text_primary.into(),
                    })
                    // Rounded card, matching .tmux-session-row's border-radius.
                    .border_corners(Some(Corners {
                        top_left: SizedPoly {
                            width: Dimension::Cells(0.3),
                            height: Dimension::Cells(0.3),
                            poly: TOP_LEFT_ROUNDED_CORNER,
                        },
                        top_right: SizedPoly {
                            width: Dimension::Cells(0.3),
                            height: Dimension::Cells(0.3),
                            poly: TOP_RIGHT_ROUNDED_CORNER,
                        },
                        bottom_left: SizedPoly {
                            width: Dimension::Cells(0.3),
                            height: Dimension::Cells(0.3),
                            poly: BOTTOM_LEFT_ROUNDED_CORNER,
                        },
                        bottom_right: SizedPoly {
                            width: Dimension::Cells(0.3),
                            height: Dimension::Cells(0.3),
                            poly: BOTTOM_RIGHT_ROUNDED_CORNER,
                        },
                    }))
                    // Cards are uniform: the row spans the sidebar minus its
                    // own margins, padding and border, so text length changes
                    // what a row says, never its shape.
                    .min_width(Some(Dimension::Pixels(
                        (sidebar_width - 16. - 12. - 2.).max(0.),
                    )));

                if session.attachable {
                    row = row
                        .item_type(UIItemType::TabSidebar(TabSidebarItem::TmuxSession {
                            box_name: snap.box_name.clone(),
                            session: session.session.clone(),
                        }))
                        .hover_colors(Some(ElementColors {
                            border: BorderColor::new(accent_orange),
                            bg: border_subtle.into(),
                            text: text_primary.into(),
                        }));
                }

                children.push(row);
                rows_emitted += 1;
            }
        }

        if children.is_empty() {
            return None;
        }

        // Say what is not shown, so a truncated list never reads as the whole
        // picture.
        if rows_hidden > 0 {
            children.push(
                Element::new(
                    title_font,
                    ElementContent::Text(format!(
                        "  +{} more (ctrl+shift+s)",
                        rows_hidden
                    )),
                )
                .display(DisplayType::Block)
                .line_height(Some(1.2))
                .padding(BoxDimension {
                    left: Dimension::Pixels(8.),
                    right: Dimension::Pixels(6.),
                    top: Dimension::Pixels(1.),
                    bottom: Dimension::Pixels(3.),
                })
                .colors(ElementColors {
                    border: BorderColor::default(),
                    bg: bg_color.into(),
                    text: text_tertiary.into(),
                })
                .min_width(Some(Dimension::Pixels(sidebar_width))),
            );
        }

        let section = Element::new(font, ElementContent::Children(children))
            .display(DisplayType::Block)
            .colors(ElementColors {
                border: BorderColor::default(),
                bg: bg_color.into(),
                text: text_color.into(),
            })
            .min_width(Some(Dimension::Pixels(sidebar_width)));

        Some(section)
    }

    pub fn build_tab_sidebar(
        &self,
        palette: &ColorPalette,
    ) -> anyhow::Result<ComputedElement> {
        let font = self.fonts.default_font()?;
        let title_font = self.fonts.title_font()?;
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

        let colors = self
            .config
            .colors
            .as_ref()
            .and_then(|c| c.tab_bar.as_ref())
            .cloned()
            .unwrap_or_else(TabBarColors::default);

        let bg_color = if self.focused.is_some() {
            self.config.window_frame.inactive_titlebar_bg
        } else {
            self.config.window_frame.inactive_titlebar_bg
        }
        .to_linear();

        let text_color = if self.focused.is_some() {
            self.config.window_frame.active_titlebar_fg
        } else {
            self.config.window_frame.inactive_titlebar_fg
        }
        .to_linear();

        let active_tab_colors = colors.active_tab();
        let inactive_tab_colors = colors.inactive_tab();
        let inactive_tab_hover_colors = colors.inactive_tab_hover();

        let mux = Mux::get();
        let mux_window = mux
            .get_window(self.mux_window_id)
            .ok_or_else(|| anyhow::anyhow!("no mux window"))?;

        let active_tab_id = mux
            .get_active_tab_for_window(self.mux_window_id)
            .map(|t| t.tab_id());

        let dimmed_color = LinearRgba::with_components(
            text_color.0 * 0.6,
            text_color.1 * 0.6,
            text_color.2 * 0.6,
            text_color.3,
        );

        // Accent color for active tab left border (accent-blue)
        let accent_color = LinearRgba::with_components(0.302, 0.620, 1.0, 1.0);
        // Notification color (accent-red)
        let notif_color = LinearRgba::with_components(0.973, 0.318, 0.286, 1.0);

        let mut tab_elements = vec![];

        // Head this window's own panes with a group heading, matching the box
        // headings the discovered machines get below. Without it the pane list
        // reads as an unlabelled list that happens to sit above the machines,
        // which is what made the sidebar look like two competing lists. No
        // status dot: these panes are live mux state, with no poller behind
        // them whose reachability could be reported.
        //
        // Uses the same muted grey as the box headings rather than the sidebar's
        // text colour, so the two headings carry equal weight.
        tab_elements.push(machine_header(
            &font,
            "local",
            None,
            header_text_color(),
            bg_color,
            sidebar_width,
        ));

        for (tab_idx, tab) in mux_window.iter().enumerate() {
            let tab_id = tab.tab_id();
            let is_active = active_tab_id == Some(tab_id);
            let title = tab.get_title();
            let info = self.tab_sidebar_info.get(&tab_id);
            let panes = tab.iter_panes_ignoring_zoom();
            let has_multiple_panes = panes.len() > 1;

            let has_notification = self
                .pane_state_for_tab(tab_id)
                .map_or(false, |ps| ps.notification_start.is_some());

            // Build child elements for this tab entry
            let mut children = vec![];

            // Close button (float right)
            let close_hover_bg = inactive_tab_hover_colors.bg_color.to_linear();
            let close_button = Element::new(
                &font,
                ElementContent::Poly {
                    line_width: metrics.underline_height.max(2),
                    poly: SizedPoly {
                        poly: X_BUTTON,
                        width: Dimension::Pixels(metrics.cell_size.height as f32 * 0.35),
                        height: Dimension::Pixels(metrics.cell_size.height as f32 * 0.35),
                    },
                },
            )
            .zindex(1)
            .vertical_align(VerticalAlign::Middle)
            .float(Float::Right)
            .item_type(UIItemType::CloseTab(tab_idx))
            .padding(BoxDimension {
                left: Dimension::Pixels(4.),
                right: Dimension::Pixels(2.),
                top: Dimension::Pixels(4.),
                bottom: Dimension::Pixels(4.),
            })
            .border(BoxDimension {
                left: Dimension::Pixels(1.),
                right: Dimension::Pixels(1.),
                top: Dimension::Pixels(1.),
                bottom: Dimension::Pixels(1.),
            })
            .colors(ElementColors {
                border: BorderColor {
                    left: LinearRgba::with_components(bg_color.0 + 0.08, bg_color.1 + 0.08, bg_color.2 + 0.08, 0.5),
                    top: LinearRgba::with_components(bg_color.0 + 0.08, bg_color.1 + 0.08, bg_color.2 + 0.08, 0.5),
                    right: LinearRgba::with_components(bg_color.0 - 0.02, bg_color.1 - 0.02, bg_color.2 - 0.02, 0.5),
                    bottom: LinearRgba::with_components(bg_color.0 - 0.02, bg_color.1 - 0.02, bg_color.2 - 0.02, 0.5),
                },
                bg: LinearRgba::with_components(bg_color.0 + 0.04, bg_color.1 + 0.04, bg_color.2 + 0.04, 0.6).into(),
                text: dimmed_color.into(),
            })
            .hover_colors(Some(ElementColors {
                border: BorderColor::new(LinearRgba::with_components(bg_color.0 + 0.12, bg_color.1 + 0.12, bg_color.2 + 0.12, 0.8)),
                bg: close_hover_bg.into(),
                text: text_color.into(),
            }));
            children.push(close_button);

            // Check if any pane in this tab runs Claude
            let has_any_claude = info.map_or(false, |i| !i.pane_claude_info.is_empty());

            // For single-pane Claude tabs, get the Claude info to render at tab level
            let single_pane_claude = if !has_multiple_panes && has_any_claude {
                info.and_then(|i| i.pane_claude_info.values().next())
            } else {
                None
            };
            let is_claude_tab = single_pane_claude.is_some();

            // Title line — prefer CWD for non-Claude tabs
            let tab_label = if is_claude_tab {
                let model_short = single_pane_claude
                    .and_then(|c| c.model.as_deref())
                    .unwrap_or("claude");
                truncate_str(model_short, 36)
            } else if has_multiple_panes {
                let label = info
                    .map(|i| i.cwd_short.as_str())
                    .filter(|s| !s.is_empty())
                    .unwrap_or(&title);
                format!("\u{25bc} {}", truncate_str(label, 34))
            } else {
                let label = info
                    .map(|i| i.cwd_short.as_str())
                    .filter(|s| !s.is_empty())
                    .unwrap_or(&title);
                truncate_str(label, 38)
            };
            let title_color = if is_claude_tab || has_any_claude {
                // Orange/amber for Claude tabs
                LinearRgba::with_components(0.859, 0.545, 0.043, 1.0)
            } else if is_active {
                active_tab_colors.fg_color.to_linear()
            } else {
                text_color
            };
            let title_element = Element::new(&font, ElementContent::Text(tab_label))
                .line_height(Some(1.1))
                .colors(ElementColors {
                    border: BorderColor::default(),
                    bg: InheritableColor::Inherited,
                    text: title_color.into(),
                });
            children.push(title_element);

            if let Some(claude) = single_pane_claude {
                // === Single-pane Claude Card at tab level ===
                build_claude_card_children(
                    &mut children,
                    claude,
                    info,
                    info.map(|i| i.cwd_short.as_str()),
                    &font,
                    &title_font,
                    dimmed_color,
                    notif_color,
                );
            } else if !has_multiple_panes {
                // === Normal single-pane tab rendering ===
                // CWD is already shown as tab title, only add git branch
                if let Some(info) = info {
                    if let Some(ref branch) = info.git_branch {
                        let branch_text =
                            format!("\u{e0a0} {}", truncate_str(branch, 34));
                        let branch_element =
                            Element::new(&title_font, ElementContent::Text(branch_text))
                                .display(DisplayType::Block)
                                .line_height(Some(0.9))
                                .colors(ElementColors {
                                    border: BorderColor::default(),
                                    bg: InheritableColor::Inherited,
                                    text: dimmed_color.into(),
                                });
                        children.push(branch_element);
                    }
                }
            }

            // Notification badge (colored dot + count)
            if has_notification {
                let notif_count = self
                    .pane_state_for_tab(tab_id)
                    .map_or(0u32, |ps| ps.notification_count);
                let badge_text = if notif_count > 1 {
                    format!("\u{25cf} {}", notif_count)
                } else {
                    "\u{25cf}".to_string()
                };
                let notif_element =
                    Element::new(&font, ElementContent::Text(badge_text))
                        .float(Float::Right)
                        .colors(ElementColors {
                            border: BorderColor::default(),
                            bg: InheritableColor::Inherited,
                            text: notif_color.into(),
                        });
                children.push(notif_element);
            }

            // Mute/unmute notifications toggle
            let active_pane_id = panes.iter().find(|p| p.is_active).map(|p| p.pane.pane_id());
            if let Some(pid) = active_pane_id {
                let is_muted = self.pane_state(pid).notifications_muted;
                let (label, text_col, bg_col, hover_bg) = if is_muted {
                    (
                        "MUTED (click to unmute)",
                        LinearRgba::with_components(1.0, 0.7, 0.2, 1.0),
                        LinearRgba::with_components(0.6, 0.3, 0.0, 0.3),
                        LinearRgba::with_components(0.6, 0.3, 0.0, 0.5),
                    )
                } else {
                    (
                        "mute notifications",
                        dimmed_color,
                        LinearRgba::with_components(0.0, 0.0, 0.0, 0.0),
                        LinearRgba::with_components(bg_color.0 + 0.08, bg_color.1 + 0.08, bg_color.2 + 0.08, 0.5),
                    )
                };
                let mute_element = Element::new(
                    &title_font,
                    ElementContent::Text(label.to_string()),
                )
                .display(DisplayType::Block)
                .line_height(Some(0.9))
                .item_type(UIItemType::TabSidebar(TabSidebarItem::MuteNotifications {
                    pane_id: pid as usize,
                }))
                .colors(ElementColors {
                    border: BorderColor::default(),
                    bg: bg_col.into(),
                    text: text_col.into(),
                })
                .hover_colors(Some(ElementColors {
                    border: BorderColor::default(),
                    bg: hover_bg.into(),
                    text: text_color.into(),
                }));
                children.push(mute_element);
            }

            // Zoom level indicator for active tab
            if is_active {
                if let Some(active_pane) = panes.iter().find(|p| p.is_active) {
                    let pane_scale = self.pane_state(active_pane.pane.pane_id()).font_scale;
                    let global_scale = self.fonts.get_font_scale();
                    let pct = (pane_scale * global_scale * 100.0).round() as u16;
                    if pct != 100 {
                        let zoom_element = Element::new(
                            &font,
                            ElementContent::Text(format!("Zoom: {}%  (Ctrl+0 reset)", pct)),
                        )
                        .display(DisplayType::Block)
                        .line_height(Some(1.0))
                        .colors(ElementColors {
                            border: BorderColor::default(),
                            bg: LinearRgba::with_components(0.0, 0.3, 0.3, 0.3).into(),
                            text: LinearRgba::with_components(0.0, 0.9, 0.9, 1.0).into(),
                        });
                        children.push(zoom_element);
                    }
                }
            }

            let base_tab_bg = if is_active {
                active_tab_colors.bg_color.to_linear()
            } else {
                bg_color
            };
            let tab_bg = if has_notification {
                // Pulse background between normal and notification color
                let notif_start = self
                    .pane_state_for_tab(tab_id)
                    .and_then(|ps| ps.notification_start);
                let elapsed = notif_start
                    .map(|start| Instant::now().duration_since(start).as_secs_f32())
                    .unwrap_or(0.0);
                let period = 1.5_f32;
                let t = ((elapsed * std::f32::consts::TAU / period).sin() + 1.0) / 2.0;
                let blend = t * 0.35;
                LinearRgba::with_components(
                    base_tab_bg.0 + (notif_color.0 - base_tab_bg.0) * blend,
                    base_tab_bg.1 + (notif_color.1 - base_tab_bg.1) * blend,
                    base_tab_bg.2 + (notif_color.2 - base_tab_bg.2) * blend,
                    base_tab_bg.3,
                )
            } else {
                base_tab_bg
            };

            let hover_bg = inactive_tab_hover_colors.bg_color.to_linear();
            let border_left_color = if is_claude_tab {
                claude_status_accent(single_pane_claude.unwrap(), is_active)
            } else if has_any_claude {
                // Multi-pane tab with Claude — use first Claude pane's status
                let first_claude = info.and_then(|i| i.pane_claude_info.values().next());
                match first_claude {
                    Some(c) => claude_status_accent(c, is_active),
                    None => accent_color,
                }
            } else if is_active {
                accent_color
            } else {
                bg_color
            };

            let tab_element = Element::new(&font, ElementContent::Children(children))
                .display(DisplayType::Block)
                .item_type(UIItemType::TabSidebar(TabSidebarItem::Tab {
                    tab_idx,
                    active: is_active,
                }))
                .padding(if is_claude_tab {
                    BoxDimension {
                        left: Dimension::Pixels(8.),
                        right: Dimension::Pixels(4.),
                        top: Dimension::Pixels(4.),
                        bottom: Dimension::Pixels(4.),
                    }
                } else {
                    BoxDimension {
                        left: Dimension::Pixels(8.),
                        right: Dimension::Pixels(4.),
                        top: Dimension::Pixels(2.),
                        bottom: Dimension::Pixels(2.),
                    }
                })
                .border(BoxDimension {
                    left: Dimension::Pixels(4.),
                    right: Dimension::Pixels(0.),
                    top: Dimension::Pixels(0.),
                    bottom: Dimension::Pixels(0.),
                })
                .colors(ElementColors {
                    border: BorderColor {
                        left: border_left_color,
                        right: tab_bg,
                        top: tab_bg,
                        bottom: tab_bg,
                    },
                    bg: tab_bg.into(),
                    text: text_color.into(),
                })
                .hover_colors(Some(ElementColors {
                    border: BorderColor {
                        left: border_left_color,
                        right: hover_bg,
                        top: hover_bg,
                        bottom: hover_bg,
                    },
                    bg: hover_bg.into(),
                    text: text_color.into(),
                }))
                .min_width(Some(Dimension::Pixels(sidebar_width)));

            tab_elements.push(tab_element);

            // Pane sub-entries (tree children) — shown for tabs with multiple panes
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
                    let is_claude_pane = pane_claude.is_some();

                    let pane_accent_color = if let Some(claude) = pane_claude {
                        claude_status_accent(claude, is_active_pane)
                    } else if is_active_pane {
                        accent_color
                    } else {
                        LinearRgba::with_components(0.0, 0.0, 0.0, 0.0)
                    };

                    let mut pane_children = vec![];

                    // Close button for pane (float right) — full size for Claude panes
                    let close_size = if is_claude_pane { 0.35 } else { 0.3 };
                    let pane_close = Element::new(
                        &font,
                        ElementContent::Poly {
                            line_width: metrics.underline_height.max(2),
                            poly: SizedPoly {
                                poly: X_BUTTON,
                                width: Dimension::Pixels(metrics.cell_size.height as f32 * close_size),
                                height: Dimension::Pixels(metrics.cell_size.height as f32 * close_size),
                            },
                        },
                    )
                    .zindex(1)
                    .vertical_align(VerticalAlign::Middle)
                    .float(Float::Right)
                    .item_type(UIItemType::TabSidebar(TabSidebarItem::ClosePane {
                        pane_id: pane_id as usize,
                    }))
                    .padding(BoxDimension {
                        left: Dimension::Pixels(4.),
                        right: Dimension::Pixels(2.),
                        top: Dimension::Pixels(if is_claude_pane { 4. } else { 8. }),
                        bottom: Dimension::Pixels(if is_claude_pane { 4. } else { 3. }),
                    })
                    .border(BoxDimension {
                        left: Dimension::Pixels(1.),
                        right: Dimension::Pixels(1.),
                        top: Dimension::Pixels(1.),
                        bottom: Dimension::Pixels(1.),
                    })
                    .colors(ElementColors {
                        border: BorderColor {
                            left: LinearRgba::with_components(bg_color.0 + 0.08, bg_color.1 + 0.08, bg_color.2 + 0.08, 0.5),
                            top: LinearRgba::with_components(bg_color.0 + 0.08, bg_color.1 + 0.08, bg_color.2 + 0.08, 0.5),
                            right: LinearRgba::with_components(bg_color.0 - 0.02, bg_color.1 - 0.02, bg_color.2 - 0.02, 0.5),
                            bottom: LinearRgba::with_components(bg_color.0 - 0.02, bg_color.1 - 0.02, bg_color.2 - 0.02, 0.5),
                        },
                        bg: LinearRgba::with_components(bg_color.0 + 0.04, bg_color.1 + 0.04, bg_color.2 + 0.04, 0.6).into(),
                        text: dimmed_color.into(),
                    })
                    .hover_colors(Some(ElementColors {
                        border: BorderColor::new(LinearRgba::with_components(bg_color.0 + 0.12, bg_color.1 + 0.12, bg_color.2 + 0.12, 0.8)),
                        bg: close_hover_bg.into(),
                        text: text_color.into(),
                    }));
                    pane_children.push(pane_close);

                    if let Some(claude) = pane_claude {
                        // === Claude Card at pane level — full card, same as tab-level ===
                        let model_short = claude
                            .model
                            .as_deref()
                            .unwrap_or("claude");
                        let title_element = Element::new(
                            &font,
                            ElementContent::Text(truncate_str(model_short, 36)),
                        )
                        .line_height(Some(1.1))
                        .colors(ElementColors {
                            border: BorderColor::default(),
                            bg: InheritableColor::Inherited,
                            text: LinearRgba::with_components(0.859, 0.545, 0.043, 1.0).into(),
                        });
                        pane_children.push(title_element);

                        build_claude_card_children(
                            &mut pane_children,
                            claude,
                            info,
                            pane_cwd.as_deref(),
                            &font,
                            &title_font,
                            dimmed_color,
                            notif_color,
                        );
                    } else {
                        // Normal pane: tree connector + title
                        let pane_label = format!("\u{2514} {}", truncate_str(&pane_title, 30));
                        pane_children.push(
                            Element::new(&font, ElementContent::Text(pane_label)).colors(
                                ElementColors {
                                    border: BorderColor::default(),
                                    bg: InheritableColor::Inherited,
                                    text: if is_active_pane {
                                        text_color.into()
                                    } else {
                                        dimmed_color.into()
                                    },
                                },
                            ),
                        );

                        // Pane CWD
                        if let Some(ref cwd) = pane_cwd {
                            pane_children.push(
                                Element::new(
                                    &title_font,
                                    ElementContent::Text(truncate_str(cwd, 34)),
                                )
                                .colors(ElementColors {
                                    border: BorderColor::default(),
                                    bg: InheritableColor::Inherited,
                                    text: dimmed_color.into(),
                                }),
                            );
                        }
                    }

                    let pane_bg = if is_active_pane {
                        LinearRgba::with_components(
                            tab_bg.0 + 0.05,
                            tab_bg.1 + 0.05,
                            tab_bg.2 + 0.05,
                            tab_bg.3,
                        )
                    } else {
                        bg_color
                    };

                    // Claude panes get full-card styling (same as tab-level),
                    // normal panes stay compact/indented as tree children.
                    let pane_element =
                        Element::new(&font, ElementContent::Children(pane_children))
                            .display(DisplayType::Block)
                            .item_type(UIItemType::TabSidebar(TabSidebarItem::Pane {
                                tab_idx,
                                pane_idx: pane_pos.index,
                            }))
                            .padding(if is_claude_pane {
                                BoxDimension {
                                    left: Dimension::Pixels(8.),
                                    right: Dimension::Pixels(4.),
                                    top: Dimension::Pixels(4.),
                                    bottom: Dimension::Pixels(4.),
                                }
                            } else {
                                BoxDimension {
                                    left: Dimension::Pixels(20.),
                                    right: Dimension::Pixels(4.),
                                    top: Dimension::Pixels(3.),
                                    bottom: Dimension::Pixels(3.),
                                }
                            })
                            .border(BoxDimension {
                                left: Dimension::Pixels(4.),
                                right: Dimension::Pixels(0.),
                                top: Dimension::Pixels(0.),
                                bottom: Dimension::Pixels(0.),
                            })
                            .colors(ElementColors {
                                border: BorderColor {
                                    left: pane_accent_color,
                                    right: pane_bg,
                                    top: pane_bg,
                                    bottom: pane_bg,
                                },
                                bg: pane_bg.into(),
                                text: text_color.into(),
                            })
                            .hover_colors(Some(ElementColors {
                                border: BorderColor {
                                    left: pane_accent_color,
                                    right: hover_bg,
                                    top: hover_bg,
                                    bottom: hover_bg,
                                },
                                bg: hover_bg.into(),
                                text: text_color.into(),
                            }))
                            .min_width(Some(Dimension::Pixels(sidebar_width)));

                    tab_elements.push(pane_element);
                }
            }
        }

        // Wrap tab entries in a container with min_height to push + button to bottom
        // Layout context used only to measure elements before the final tree
        // is assembled; identical to the one used for the real layout below.
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

        // New tab button — centered, stuck to bottom
        let new_tab_colors = colors.new_tab();
        let new_tab_hover = colors.new_tab_hover();
        // A labelled row in the flow of the list rather than a control pinned
        // to the sidebar's bottom edge. Pinning needed the tab list stretched
        // to a predicted height, and whenever the prediction was short the
        // button was pushed off-screen; a row in document order cannot be.
        let new_tab_button = Element::new(
            &font,
            ElementContent::Text("  +  new terminal".to_string()),
        )
        .display(DisplayType::Block)
        .item_type(UIItemType::TabSidebar(TabSidebarItem::NewTabButton))
        .line_height(Some(1.2))
        .padding(BoxDimension {
            left: Dimension::Pixels(10.),
            right: Dimension::Pixels(6.),
            top: Dimension::Pixels(4.),
            bottom: Dimension::Pixels(6.),
        })
        .colors(ElementColors {
            border: BorderColor::default(),
            bg: InheritableColor::Inherited,
            text: new_tab_colors.fg_color.to_linear().into(),
        })
        .hover_colors(Some(ElementColors {
            border: BorderColor::default(),
            bg: new_tab_hover.bg_color.to_linear().into(),
            text: new_tab_hover.fg_color.to_linear().into(),
        }))
        .min_width(Some(Dimension::Pixels(sidebar_width)));

        // Claude agents / tmux sessions.
        //
        // The WebView sidebar renders this from serialize_sidebar_state(), but
        // that path is Windows-only (WebView2), so on every other platform the
        // section did not exist at all. Build it natively from the same
        // discovery snapshot the Ctrl+Shift+S picker reads, which already
        // covers every machine in the interconnect registry.
        //
        // Built here, before tabs_min_height, because the tabs container is
        // stretched to push the new-tab button to the bottom of the sidebar:
        // its height has to account for this section or the agent rows land
        // below the visible area.
        // Cap the section at half the sidebar so the tab and pane tree, which
        // is the primary content, cannot be squeezed out by a machine running
        // many agents. Rows beyond the budget are summarised rather than
        // silently dropped, and Ctrl+Shift+S still lists every session.
        let row_budget = {
            let avail = (window_height * 0.5).max(0.);
            let row_h = metrics.cell_size.height as f32 * 1.2 + 10.;
            if row_h > 0. { (avail / row_h) as usize } else { usize::MAX }
        };
        let agents_section =
            self.build_agents_section(&font, &title_font, &metrics, palette, bg_color,
                                      text_color, active_tab_colors, sidebar_width,
                                      row_budget);

        // Measure the section instead of estimating it. A hand-computed guess
        // has to match what layout actually produces to the pixel; when it came
        // up short the tabs container over-reserved and pushed the agent rows
        // off the bottom of the window, which a maximized window made obvious
        // because it fits more rows.
        let agents_height = match agents_section.as_ref() {
            Some(section) => self
                .compute_element(&context_probe, section)
                .map(|c| c.bounds.height())
                .unwrap_or(0.),
            None => 0.,
        };


        // Everything flows in document order, with nothing stretched to pin a
        // child to the window's bottom edge.
        //
        // The old layout padded this container out to
        // `window_height - button_height - agents_height` so the new-tab button
        // sat at the bottom. That is a *minimum*, so whenever the tab list's own
        // content was taller than the remainder the container grew past it and
        // pushed the button below the visible area — which is why the button
        // looked like it never rendered on Linux. It was painting the whole
        // time, just off-screen, and no amount of adjusting the reservation
        // could fix a prediction the layout is free to exceed.
        let tabs_container = Element::new(&font, ElementContent::Children(tab_elements))
            .display(DisplayType::Block)
            .colors(ElementColors {
                border: BorderColor::default(),
                bg: InheritableColor::Inherited,
                text: InheritableColor::Inherited,
            });

        // The new-terminal row closes the local group instead of floating at
        // the bottom of the sidebar, so it needs no height prediction at all.
        let mut sidebar_children = vec![tabs_container, new_tab_button];
        if let Some(section) = agents_section {
            sidebar_children.push(section);
        }

        // Root container
        let root = Element::new(
            &font,
            ElementContent::Children(sidebar_children),
        )
        .display(DisplayType::Block)
        .padding(BoxDimension {
            left: Dimension::Pixels(0.),
            right: Dimension::Pixels(0.),
            top: Dimension::Pixels(3.),
            bottom: Dimension::Pixels(0.),
        })
        .colors(ElementColors {
            border: BorderColor::default(),
            bg: bg_color.into(),
            text: text_color.into(),
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
        let tab_bar_height = if self.show_tab_bar {
            self.tab_bar_pixel_height().unwrap_or(0.)
        } else {
            0.
        };
        let bg_color = self
            .config
            .window_frame
            .inactive_titlebar_bg
            .to_linear();
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

        // The sidebar is cached until invalidated, so a poll that discovers new
        // agents would otherwise never reach the screen. Compare a cheap
        // fingerprint of the discovery snapshot and rebuild when it moves.
        if self.config.tmux.as_ref().map_or(false, |t| t.enabled) {
            let mut fingerprint = String::new();
            for snap in crate::tmux_discovery::snapshot() {
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
        Ok(())
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

/// Truncate a string to max_chars, appending "..." if truncated.
/// The muted grey both sidebar group headings use, matching the WebView
/// sidebar's --text-secondary. Defined once so the local heading and the box
/// headings cannot drift apart.
fn header_text_color() -> LinearRgba {
    fn srgb_to_linear(c: f32) -> f32 {
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }
    let c = srgb_to_linear(0x99 as f32 / 255.);
    LinearRgba::with_components(c, c, c, 1.)
}

/// A group heading in the sidebar: an optional status dot and a machine name.
///
/// Shared by the discovered boxes and by the local pane list so the two read as
/// one grouped system rather than two lists that happen to sit above each other.
/// `dot` is None for the local group, which has no poller and so nothing to
/// report a reachability status for.
fn machine_header(
    font: &Rc<LoadedFont>,
    name: &str,
    dot: Option<LinearRgba>,
    text_color: LinearRgba,
    bg_color: LinearRgba,
    sidebar_width: f32,
) -> Element {
    let mut parts = vec![];
    if let Some(dot_color) = dot {
        parts.push(
            Element::new(font, ElementContent::Text("\u{25cf} ".to_string()))
                .display(DisplayType::Inline)
                .colors(ElementColors {
                    border: BorderColor::default(),
                    bg: InheritableColor::Inherited,
                    text: dot_color.into(),
                }),
        );
    } else {
        // Keep the name aligned with the dotted headers below it.
        parts.push(
            Element::new(font, ElementContent::Text("  ".to_string()))
                .display(DisplayType::Inline)
                .colors(ElementColors {
                    border: BorderColor::default(),
                    bg: InheritableColor::Inherited,
                    text: text_color.into(),
                }),
        );
    }
    parts.push(
        Element::new(font, ElementContent::Text(truncate_str(name, 18)))
            .display(DisplayType::Inline)
            .colors(ElementColors {
                border: BorderColor::default(),
                bg: InheritableColor::Inherited,
                text: text_color.into(),
            }),
    );

    Element::new(font, ElementContent::Children(parts))
        .display(DisplayType::Block)
        .line_height(Some(1.3))
        .padding(BoxDimension {
            left: Dimension::Pixels(8.),
            right: Dimension::Pixels(6.),
            top: Dimension::Pixels(5.),
            bottom: Dimension::Pixels(2.),
        })
        .colors(ElementColors {
            border: BorderColor::default(),
            bg: bg_color.into(),
            text: text_color.into(),
        })
        .min_width(Some(Dimension::Pixels(sidebar_width)))
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
