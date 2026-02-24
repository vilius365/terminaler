use crate::termwindow::box_model::*;
use crate::termwindow::modal::Modal;
use crate::termwindow::render::corners::{
    BOTTOM_LEFT_ROUNDED_CORNER, BOTTOM_RIGHT_ROUNDED_CORNER, TOP_LEFT_ROUNDED_CORNER,
    TOP_RIGHT_ROUNDED_CORNER,
};
use crate::termwindow::{DimensionContext, TermWindow, UIItem, UIItemType};
use crate::utilsprites::RenderMetrics;
use config::keyassignment::{KeyAssignment, SpawnCommand, SpawnTabDomain};
use config::{configuration, Dimension};
use mux::domain::DomainState;
use mux::Mux;
use std::cell::{Ref, RefCell};
use terminaler_term::{KeyCode, KeyModifiers, MouseEvent};
use window::color::LinearRgba;

pub struct DropdownEntry {
    pub label: String,
    pub action: KeyAssignment,
    pub is_default: bool,
}

pub struct ProfileDropdown {
    element: RefCell<Option<Vec<ComputedElement>>>,
    pub entries: Vec<DropdownEntry>,
    selected_row: RefCell<usize>,
    anchor_x: f32,
    anchor_y: f32,
}

impl ProfileDropdown {
    pub fn new(term_window: &TermWindow, anchor_item: &UIItem) -> Self {
        let anchor_x = anchor_item.x as f32;
        let anchor_y = (anchor_item.y + anchor_item.height) as f32;

        // If tab bar is at bottom, position dropdown above the button
        let anchor_y = if term_window.config.tab_bar_at_bottom {
            anchor_item.y as f32
        } else {
            anchor_y
        };

        let entries = Self::build_entries(term_window);

        Self {
            element: RefCell::new(None),
            entries,
            selected_row: RefCell::new(0),
            anchor_x,
            anchor_y,
        }
    }

    fn build_entries(term_window: &TermWindow) -> Vec<DropdownEntry> {
        let config = configuration();
        let mut entries = vec![];

        let default_action = term_window.default_profile_action.as_ref();

        // Default shell entry (what "+" currently does)
        let default_shell_action = KeyAssignment::SpawnTab(SpawnTabDomain::CurrentPaneDomain);
        entries.push(DropdownEntry {
            label: "Default Shell".to_string(),
            action: default_shell_action.clone(),
            is_default: default_action.is_none()
                || default_action == Some(&default_shell_action),
        });

        // launch_menu items (user-configured profiles)
        for item in &config.launch_menu {
            let label = match item.label.as_ref() {
                Some(label) => label.to_string(),
                None => match item.args.as_ref() {
                    Some(args) => args.join(" "),
                    None => "(default shell)".to_string(),
                },
            };
            let action = KeyAssignment::SpawnCommandInNewTab(item.clone());
            let is_default = default_action == Some(&action);
            entries.push(DropdownEntry {
                label,
                action,
                is_default,
            });
        }

        // Attached domains
        let mux = Mux::get();
        for dom in mux.iter_domains() {
            if dom.spawnable() && dom.state() == DomainState::Attached {
                let name = dom.domain_name().to_string();
                // Skip the "local" domain — it's already the default shell
                if name == "local" {
                    continue;
                }
                let action = KeyAssignment::SpawnCommandInNewTab(SpawnCommand {
                    domain: SpawnTabDomain::DomainName(name.clone()),
                    ..SpawnCommand::default()
                });
                let is_default = default_action == Some(&action);
                entries.push(DropdownEntry {
                    label: format!("New Tab ({})", name),
                    action,
                    is_default,
                });
            }
        }

        entries
    }

    fn compute(
        term_window: &mut TermWindow,
        entries: &[DropdownEntry],
        selected_row: usize,
        anchor_x: f32,
        anchor_y: f32,
        tab_bar_at_bottom: bool,
    ) -> anyhow::Result<Vec<ComputedElement>> {
        let font = term_window
            .fonts
            .title_font()
            .expect("to resolve title font for dropdown");
        let metrics = RenderMetrics::with_font_metrics(&font.metrics());

        let solid_bg: InheritableColor = term_window
            .config
            .command_palette_bg_color
            .to_linear()
            .into();
        let solid_fg: InheritableColor = term_window
            .config
            .command_palette_fg_color
            .to_linear()
            .into();

        let mut row_elements = vec![];
        for (idx, entry) in entries.iter().enumerate() {
            let (bg, text) = if idx == selected_row {
                (solid_fg.clone(), solid_bg.clone())
            } else {
                (LinearRgba::TRANSPARENT.into(), solid_fg.clone())
            };

            // Prefix with a checkmark for the default profile
            let label = if entry.is_default {
                format!("\u{2713} {}", entry.label)
            } else {
                format!("  {}", entry.label)
            };

            row_elements.push(
                Element::new(&font, ElementContent::Text(label))
                    .item_type(UIItemType::ProfileDropdownItem(idx))
                    .colors(ElementColors {
                        border: BorderColor::default(),
                        bg,
                        text,
                    })
                    .padding(BoxDimension {
                        left: Dimension::Cells(0.75),
                        right: Dimension::Cells(1.5),
                        top: Dimension::Cells(0.35),
                        bottom: Dimension::Cells(0.35),
                    })
                    .display(DisplayType::Block),
            );
        }

        // Add separator + "Set as Default" hint at bottom
        row_elements.push(
            Element::new(
                &font,
                ElementContent::Text("Right-click to set default".to_string()),
            )
            .colors(ElementColors {
                border: BorderColor::default(),
                bg: LinearRgba::TRANSPARENT.into(),
                text: InheritableColor::Color(LinearRgba(0.5, 0.5, 0.55, 1.0)),
            })
            .padding(BoxDimension {
                left: Dimension::Cells(0.75),
                right: Dimension::Cells(0.75),
                top: Dimension::Cells(0.4),
                bottom: Dimension::Cells(0.25),
            })
            .display(DisplayType::Block),
        );

        let container = Element::new(&font, ElementContent::Children(row_elements))
            .colors(ElementColors {
                border: BorderColor::new(
                    term_window
                        .config
                        .command_palette_bg_color
                        .to_linear()
                        .into(),
                ),
                bg: term_window
                    .config
                    .command_palette_bg_color
                    .to_linear()
                    .into(),
                text: term_window
                    .config
                    .command_palette_fg_color
                    .to_linear()
                    .into(),
            })
            .padding(BoxDimension {
                left: Dimension::Cells(0.25),
                right: Dimension::Cells(0.25),
                top: Dimension::Cells(0.25),
                bottom: Dimension::Cells(0.25),
            })
            .border(BoxDimension::new(Dimension::Pixels(1.)))
            .border_corners(Some(Corners {
                top_left: SizedPoly {
                    width: Dimension::Cells(0.25),
                    height: Dimension::Cells(0.25),
                    poly: TOP_LEFT_ROUNDED_CORNER,
                },
                top_right: SizedPoly {
                    width: Dimension::Cells(0.25),
                    height: Dimension::Cells(0.25),
                    poly: TOP_RIGHT_ROUNDED_CORNER,
                },
                bottom_left: SizedPoly {
                    width: Dimension::Cells(0.25),
                    height: Dimension::Cells(0.25),
                    poly: BOTTOM_LEFT_ROUNDED_CORNER,
                },
                bottom_right: SizedPoly {
                    width: Dimension::Cells(0.25),
                    height: Dimension::Cells(0.25),
                    poly: BOTTOM_RIGHT_ROUNDED_CORNER,
                },
            }));

        let dimensions = term_window.dimensions;

        // Width: based on longest label + padding, with sensible bounds
        let max_label_chars = entries
            .iter()
            .map(|e| e.label.len() + 4) // +4 for checkmark prefix + padding
            .max()
            .unwrap_or(20)
            .max("Right-click to set default".len() + 2);
        let content_width = max_label_chars as f32 * metrics.cell_size.width as f32;
        let desired_width = content_width
            .max(250.)
            .min(dimensions.pixel_width as f32 * 0.5);

        // Clamp X so dropdown doesn't overflow right edge
        let dropdown_x = anchor_x.min(dimensions.pixel_width as f32 - desired_width);

        // Available height for the dropdown
        let max_height = if tab_bar_at_bottom {
            anchor_y
        } else {
            dimensions.pixel_height as f32 - anchor_y
        };

        // For tab-bar-at-bottom, we need to position the dropdown above,
        // but the layout engine places from top-down. We'll compute at anchor_y
        // and let it grow downward when tab bar is at top (normal case).
        // For bottom tab bar, we compute in a temp position and adjust after.
        let compute_y = if tab_bar_at_bottom { 0. } else { anchor_y };

        let computed = term_window.compute_element(
            &LayoutContext {
                height: DimensionContext {
                    dpi: dimensions.dpi as f32,
                    pixel_max: dimensions.pixel_height as f32,
                    pixel_cell: metrics.cell_size.height as f32,
                },
                width: DimensionContext {
                    dpi: dimensions.dpi as f32,
                    pixel_max: dimensions.pixel_width as f32,
                    pixel_cell: metrics.cell_size.width as f32,
                },
                bounds: euclid::rect(dropdown_x, compute_y, desired_width, max_height),
                metrics: &metrics,
                gl_state: term_window.render_state.as_ref().unwrap(),
                zindex: 100,
            },
            &container,
        )?;

        // For tab-bar-at-bottom, translate so the dropdown appears above the button
        let computed = if tab_bar_at_bottom {
            let mut c = computed;
            let dropdown_height = c.bounds.height();
            c.translate(euclid::vec2(0., anchor_y - dropdown_height));
            c
        } else {
            computed
        };

        Ok(vec![computed])
    }
}

impl Modal for ProfileDropdown {
    fn mouse_event(&self, _event: MouseEvent, _term_window: &mut TermWindow) -> anyhow::Result<()> {
        // Mouse clicks on dropdown items are handled via UIItemType::ProfileDropdownItem
        // in mouse_event_ui_item. This method handles clicks that land on the modal
        // background but not on any specific item — currently a no-op.
        Ok(())
    }

    fn key_down(
        &self,
        key: KeyCode,
        mods: KeyModifiers,
        term_window: &mut TermWindow,
    ) -> anyhow::Result<bool> {
        match (key, mods) {
            (KeyCode::Escape, KeyModifiers::NONE) => {
                term_window.cancel_modal();
            }
            (KeyCode::UpArrow, KeyModifiers::NONE) | (KeyCode::Char('k'), KeyModifiers::NONE) => {
                let mut row = self.selected_row.borrow_mut();
                *row = row.saturating_sub(1);
            }
            (KeyCode::DownArrow, KeyModifiers::NONE)
            | (KeyCode::Char('j'), KeyModifiers::NONE) => {
                let mut row = self.selected_row.borrow_mut();
                *row = (*row + 1).min(self.entries.len().saturating_sub(1));
            }
            (KeyCode::Enter, KeyModifiers::NONE) => {
                let idx = *self.selected_row.borrow();
                if let Some(entry) = self.entries.get(idx) {
                    let action = entry.action.clone();
                    term_window.cancel_modal();
                    if let Some(pane) = term_window.get_active_pane_or_overlay() {
                        if let Err(err) = term_window.perform_key_assignment(&pane, &action) {
                            log::error!("Error spawning from profile dropdown: {err:#}");
                        }
                    }
                    return Ok(true);
                }
            }
            _ => return Ok(false),
        }
        term_window.invalidate_modal();
        Ok(true)
    }

    fn computed_element(
        &self,
        term_window: &mut TermWindow,
    ) -> anyhow::Result<Ref<'_, [ComputedElement]>> {
        if self.element.borrow().is_none() {
            let tab_bar_at_bottom = term_window.config.tab_bar_at_bottom;
            let element = Self::compute(
                term_window,
                &self.entries,
                *self.selected_row.borrow(),
                self.anchor_x,
                self.anchor_y,
                tab_bar_at_bottom,
            )?;
            self.element.borrow_mut().replace(element);
        }
        Ok(Ref::map(self.element.borrow(), |v| {
            v.as_ref().unwrap().as_slice()
        }))
    }

    fn reconfigure(&self, _term_window: &mut TermWindow) {
        self.element.borrow_mut().take();
    }
}
