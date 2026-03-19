use crate::customglyph::*;
use crate::termwindow::box_model::*;
use crate::termwindow::{SidebarTabInfo, TabSidebarItem, UIItem, UIItemType};
use crate::utilsprites::RenderMetrics;
use config::{Dimension, TabBarColors, TabSidebarPosition};
use mux::pane::CachePolicy;
use mux::tab::TabId;
use mux::Mux;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};
use terminaler_font::LoadedFont;
use terminaler_term::color::ColorPalette;
use window::color::LinearRgba;

const PLUS_BUTTON: &[Poly] = &[
    Poly {
        path: &[
            PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
            PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::One),
        ],
        intensity: BlockAlpha::Full,
        style: PolyStyle::Outline,
    },
    Poly {
        path: &[
            PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Frac(1, 2)),
            PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(1, 2)),
        ],
        intensity: BlockAlpha::Full,
        style: PolyStyle::Outline,
    },
];

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

    /// Poll CWD and git branch info for each tab, throttled to every 2 seconds.
    pub fn update_sidebar_info(&mut self) {
        if self.last_sidebar_info_poll.elapsed() < Duration::from_secs(2) {
            return;
        }
        self.last_sidebar_info_poll = Instant::now();

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

            let git_branch = cwd_path.as_deref().and_then(find_git_branch);

            // Detect Claude Code sessions on ALL panes in the tab
            use crate::termwindow::{ClaudeSessionInfo, ClaudeStatus};
            let mut pane_claude_info = std::collections::HashMap::new();
            for pane_pos in tab.iter_panes_ignoring_zoom() {
                let pane = &pane_pos.pane;
                let process_name = pane.get_foreground_process_name(CachePolicy::AllowStale);
                let pane_title = pane.get_title();
                let user_vars = pane.copy_user_vars();
                let is_claude = process_name.as_deref().map_or(false, is_claude_process)
                    || is_claude_title(&pane_title)
                    || user_vars.keys().any(|k| k.starts_with("claude_"));

                if is_claude {
                    let status = user_vars.get("claude_status").map(|s| match s.as_str() {
                        "working" => ClaudeStatus::Working,
                        "waiting_input" => ClaudeStatus::WaitingInput,
                        "idle" => ClaudeStatus::Idle,
                        "error" => ClaudeStatus::Error,
                        _ => ClaudeStatus::Working,
                    });
                    pane_claude_info.insert(
                        pane.pane_id(),
                        ClaudeSessionInfo {
                            model: user_vars.get("claude_model").cloned(),
                            context_pct: user_vars.get("claude_context_pct").and_then(|v| v.parse().ok()),
                            cost_usd: user_vars.get("claude_cost").and_then(|v| v.parse().ok()),
                            duration_ms: user_vars.get("claude_duration_ms").and_then(|v| v.parse().ok()),
                            lines_added: user_vars.get("claude_lines_added").and_then(|v| v.parse().ok()),
                            lines_removed: user_vars.get("claude_lines_removed").and_then(|v| v.parse().ok()),
                            worktree: user_vars.get("claude_worktree").cloned(),
                            status,
                        },
                    );
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

        self.tab_sidebar_info = new_info;
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

        // Accent color for active tab left border
        let accent_color = LinearRgba::with_components(0.2, 0.5, 1.0, 1.0);
        // Notification color
        let notif_color = LinearRgba::with_components(1.0, 0.3, 0.3, 1.0);

        let mut tab_elements = vec![];

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
                LinearRgba::with_components(1.0, 0.7, 0.2, 1.0)
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

            let tab_bg = if is_active {
                active_tab_colors.bg_color.to_linear()
            } else {
                bg_color
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

                    // Close button for pane (float right)
                    let pane_close = Element::new(
                        &font,
                        ElementContent::Poly {
                            line_width: metrics.underline_height.max(2),
                            poly: SizedPoly {
                                poly: X_BUTTON,
                                width: Dimension::Pixels(metrics.cell_size.height as f32 * 0.3),
                                height: Dimension::Pixels(metrics.cell_size.height as f32 * 0.3),
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
                        left: Dimension::Pixels(3.),
                        right: Dimension::Pixels(2.),
                        top: Dimension::Pixels(8.),
                        bottom: Dimension::Pixels(3.),
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
                        // === Claude Card at pane level ===
                        let model_short = claude
                            .model
                            .as_deref()
                            .unwrap_or("claude");
                        let title_element = Element::new(
                            &font,
                            ElementContent::Text(truncate_str(model_short, 32)),
                        )
                        .line_height(Some(1.0))
                        .colors(ElementColors {
                            border: BorderColor::default(),
                            bg: InheritableColor::Inherited,
                            text: LinearRgba::with_components(1.0, 0.7, 0.2, 1.0).into(),
                        });
                        pane_children.push(title_element);

                        build_claude_card_children(
                            &mut pane_children,
                            claude,
                            info,
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

                    let pane_element =
                        Element::new(&font, ElementContent::Children(pane_children))
                            .display(DisplayType::Block)
                            .item_type(UIItemType::TabSidebar(TabSidebarItem::Pane {
                                tab_idx,
                                pane_idx: pane_pos.index,
                            }))
                            .padding(BoxDimension {
                                left: Dimension::Pixels(if is_claude_pane { 12. } else { 20. }),
                                right: Dimension::Pixels(4.),
                                top: Dimension::Pixels(if is_claude_pane { 4. } else { 3. }),
                                bottom: Dimension::Pixels(if is_claude_pane { 4. } else { 3. }),
                            })
                            .border(BoxDimension {
                                left: Dimension::Pixels(if is_claude_pane { 4. } else { 2. }),
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
        let button_height = metrics.cell_size.height as f32 + 16.; // button + padding
        let tabs_min_height = window_height - button_height - 3.; // 3px top padding
        let tabs_container = Element::new(&font, ElementContent::Children(tab_elements))
            .display(DisplayType::Block)
            .min_height(Some(Dimension::Pixels(tabs_min_height.max(0.))))
            .colors(ElementColors {
                border: BorderColor::default(),
                bg: InheritableColor::Inherited,
                text: InheritableColor::Inherited,
            });

        // New tab button — centered, stuck to bottom
        let new_tab_colors = colors.new_tab();
        let new_tab_hover = colors.new_tab_hover();
        let plus_size = metrics.cell_size.height as f32 * 0.4;
        let h_padding = (sidebar_width - plus_size - 2.) / 2.; // center horizontally
        let new_tab_button = Element::new(
            &font,
            ElementContent::Poly {
                line_width: metrics.underline_height.max(2),
                poly: SizedPoly {
                    poly: PLUS_BUTTON,
                    width: Dimension::Pixels(plus_size),
                    height: Dimension::Pixels(plus_size),
                },
            },
        )
        .display(DisplayType::Block)
        .vertical_align(VerticalAlign::Middle)
        .item_type(UIItemType::TabSidebar(TabSidebarItem::NewTabButton))
        .padding(BoxDimension {
            left: Dimension::Pixels(h_padding),
            right: Dimension::Pixels(h_padding),
            top: Dimension::Pixels(6.),
            bottom: Dimension::Pixels(6.),
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
            bg: new_tab_colors.bg_color.to_linear().into(),
            text: new_tab_colors.fg_color.to_linear().into(),
        })
        .hover_colors(Some(ElementColors {
            border: BorderColor::new(LinearRgba::with_components(bg_color.0 + 0.12, bg_color.1 + 0.12, bg_color.2 + 0.12, 0.8)),
            bg: new_tab_hover.bg_color.to_linear().into(),
            text: new_tab_hover.fg_color.to_linear().into(),
        }))
        .min_width(Some(Dimension::Pixels(sidebar_width)));

        // Root container
        let root = Element::new(
            &font,
            ElementContent::Children(vec![tabs_container, new_tab_button]),
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

        let dpi = self.dimensions.dpi as f32;
        let context = LayoutContext {
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

        let mut computed = self.compute_element(&context, &root)?;

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
    fn pane_state_for_tab(&self, tab_id: TabId) -> Option<std::cell::Ref<'_, crate::termwindow::PaneState>> {
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
            LinearRgba::with_components(0.3, 0.8, 0.4, 1.0),
        ),
        Some(ClaudeStatus::WaitingInput) => (
            "\u{25cf}",  // ●
            "awaiting input",
            LinearRgba::with_components(1.0, 0.8, 0.2, 1.0),
        ),
        Some(ClaudeStatus::Idle) => (
            "\u{2714}",  // ✔
            "idle",
            LinearRgba::with_components(0.5, 0.5, 0.5, 1.0),
        ),
        Some(ClaudeStatus::Error) => (
            "\u{2717}",  // ✗
            "error",
            notif_color,
        ),
        None => (
            "\u{25b6}",  // ▶
            "active",
            LinearRgba::with_components(0.3, 0.8, 0.4, 1.0),
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

    // Worktree/project + branch
    let project = claude
        .worktree
        .as_deref()
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
        children.push(
            Element::new(detail_font, ElementContent::Text(project_line))
                .display(DisplayType::Block)
                .line_height(Some(0.9))
                .colors(ElementColors {
                    border: BorderColor::default(),
                    bg: InheritableColor::Inherited,
                    text: dimmed_color.into(),
                }),
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
            LinearRgba::with_components(1.0, 0.8, 0.2, 1.0)
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

    // Cost + duration + lines
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
    if claude.lines_added.is_some() || claude.lines_removed.is_some() {
        let added = claude.lines_added.unwrap_or(0);
        let removed = claude.lines_removed.unwrap_or(0);
        if added > 0 || removed > 0 {
            stats.push(format!("+{} -{}", added, removed));
        }
    }
    if !stats.is_empty() {
        children.push(
            Element::new(
                detail_font,
                ElementContent::Text(truncate_str(
                    &stats.join(" \u{00b7} "),
                    38,
                )),
            )
            .display(DisplayType::Block)
            .line_height(Some(0.9))
            .colors(ElementColors {
                border: BorderColor::default(),
                bg: InheritableColor::Inherited,
                text: dimmed_color.into(),
            }),
        );
    }
}

/// Get the accent color for a Claude session based on its status.
fn claude_status_accent(claude: &crate::termwindow::ClaudeSessionInfo, active: bool) -> LinearRgba {
    use crate::termwindow::ClaudeStatus;
    let base = match claude.status {
        Some(ClaudeStatus::Working) => LinearRgba::with_components(0.3, 0.8, 0.4, 1.0),      // green
        Some(ClaudeStatus::WaitingInput) => LinearRgba::with_components(1.0, 0.8, 0.2, 1.0),  // yellow
        Some(ClaudeStatus::Idle) => LinearRgba::with_components(0.5, 0.5, 0.5, 1.0),          // gray
        Some(ClaudeStatus::Error) => LinearRgba::with_components(1.0, 0.3, 0.3, 1.0),         // red
        None => LinearRgba::with_components(0.3, 0.8, 0.4, 1.0),                              // green (default)
    };
    if active {
        base
    } else {
        LinearRgba::with_components(base.0 * 0.7, base.1 * 0.7, base.2 * 0.7, 0.6)
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

/// Find the git branch by walking up from the given directory
/// and reading .git/HEAD.
fn find_git_branch(path: &str) -> Option<String> {
    // On Windows/WSL, handle path conversion
    let mut dir = std::path::PathBuf::from(path);
    for _ in 0..20 {
        let git_head = dir.join(".git").join("HEAD");
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
fn truncate_str(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars - 1).collect();
        format!("{}\u{2026}", truncated)
    }
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
