use mux::pane::CachePolicy;
use mux::Mux;
use mux::termwiztermtab::TermWizTerminal;
use mux::window::WindowId as MuxWindowId;
use std::time::Duration;
use termwiz::cell::{AttributeChange, Intensity};
use termwiz::color::ColorAttribute;
use termwiz::input::{InputEvent, KeyCode, KeyEvent, Modifiers};
use termwiz::surface::{Change, CursorVisibility, Position};
use termwiz::terminal::{ScreenSize, Terminal};

/// Snapshot of TermWindow state captured before launching the overlay.
#[derive(Clone, Debug)]
pub struct DebugSnapshot {
    pub fps: f32,
    pub last_frame_ms: f64,
    pub window_id: MuxWindowId,
    pub terminal_size: (usize, usize), // (cols, rows)
    pub pixel_size: (usize, usize),    // (width, height)
    pub dpi: u32,
    pub font_name: String,
    pub font_size: f64,
    pub gpu_info: String,
    pub config_file: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Window,
    Tabs,
    Panes,
    Config,
}

impl Section {
    fn all() -> &'static [Section] {
        &[Section::Window, Section::Tabs, Section::Panes, Section::Config]
    }

    fn label(&self) -> &str {
        match self {
            Section::Window => "Window",
            Section::Tabs => "Tabs",
            Section::Panes => "Panes",
            Section::Config => "Config",
        }
    }
}

pub fn debug_overlay(
    mut term: TermWizTerminal,
    snapshot: DebugSnapshot,
) -> anyhow::Result<()> {
    term.set_raw_mode()?;
    let mut active_section = 0usize;
    let mut scroll_offset = 0usize;

    loop {
        let size = term.get_screen_size()?;
        let lines = build_content(&snapshot, active_section, size.cols);

        render(&mut term, &lines, active_section, scroll_offset, &size)?;

        match term.poll_input(Some(Duration::from_millis(500))) {
            Ok(Some(event)) => match event {
                InputEvent::Key(KeyEvent {
                    key: KeyCode::Escape,
                    ..
                })
                | InputEvent::Key(KeyEvent {
                    key: KeyCode::Char('q'),
                    ..
                }) => {
                    return Ok(());
                }
                InputEvent::Key(KeyEvent {
                    key: KeyCode::LeftArrow,
                    ..
                })
                | InputEvent::Key(KeyEvent {
                    key: KeyCode::Tab,
                    modifiers: Modifiers::SHIFT,
                    ..
                }) => {
                    active_section = if active_section == 0 {
                        Section::all().len() - 1
                    } else {
                        active_section - 1
                    };
                    scroll_offset = 0;
                }
                InputEvent::Key(KeyEvent {
                    key: KeyCode::Tab, ..
                })
                | InputEvent::Key(KeyEvent {
                    key: KeyCode::RightArrow,
                    ..
                }) => {
                    active_section = (active_section + 1) % Section::all().len();
                    scroll_offset = 0;
                }
                InputEvent::Key(KeyEvent {
                    key: KeyCode::DownArrow,
                    ..
                })
                | InputEvent::Key(KeyEvent {
                    key: KeyCode::Char('j'),
                    ..
                }) => {
                    scroll_offset = scroll_offset.saturating_add(1);
                }
                InputEvent::Key(KeyEvent {
                    key: KeyCode::UpArrow,
                    ..
                })
                | InputEvent::Key(KeyEvent {
                    key: KeyCode::Char('k'),
                    ..
                }) => {
                    scroll_offset = scroll_offset.saturating_sub(1);
                }
                InputEvent::Key(KeyEvent {
                    key: KeyCode::PageDown,
                    ..
                }) => {
                    scroll_offset = scroll_offset.saturating_add(size.rows.saturating_sub(5));
                }
                InputEvent::Key(KeyEvent {
                    key: KeyCode::PageUp,
                    ..
                }) => {
                    scroll_offset = scroll_offset.saturating_sub(size.rows.saturating_sub(5));
                }
                _ => {}
            },
            Ok(None) => {
                // Timeout — re-render to refresh live data (Mux queries)
            }
            Err(_) => return Ok(()),
        }
    }
}

fn render(
    term: &mut TermWizTerminal,
    lines: &[ContentLine],
    active_section: usize,
    scroll_offset: usize,
    size: &ScreenSize,
) -> anyhow::Result<()> {
    let mut changes: Vec<Change> = vec![
        Change::ClearScreen(ColorAttribute::Default),
        Change::CursorVisibility(CursorVisibility::Hidden),
    ];

    // Title bar
    changes.push(Change::CursorPosition {
        x: Position::Absolute(0),
        y: Position::Absolute(0),
    });
    changes.push(AttributeChange::Reverse(true).into());
    let title = format!(" Terminaler Debug Console {:>w$}", "",
        w = size.cols.saturating_sub(27));
    changes.push(Change::Text(truncate(&title, size.cols)));
    changes.push(AttributeChange::Reverse(false).into());

    // Tab bar (row 1)
    changes.push(Change::CursorPosition {
        x: Position::Absolute(0),
        y: Position::Absolute(1),
    });
    for (idx, section) in Section::all().iter().enumerate() {
        if idx == active_section {
            changes.push(AttributeChange::Reverse(true).into());
        }
        changes.push(Change::Text(format!(" {} ", section.label())));
        if idx == active_section {
            changes.push(AttributeChange::Reverse(false).into());
        }
        changes.push(Change::Text(" ".to_string()));
    }

    // Separator
    changes.push(Change::CursorPosition {
        x: Position::Absolute(0),
        y: Position::Absolute(2),
    });
    changes.push(Change::Text("\u{2500}".repeat(size.cols)));

    // Content area (rows 3..rows-1)
    let content_start = 3;
    let content_rows = size.rows.saturating_sub(content_start + 1);
    let visible_lines = &lines[scroll_offset.min(lines.len())..];

    for (i, line) in visible_lines.iter().take(content_rows).enumerate() {
        changes.push(Change::CursorPosition {
            x: Position::Absolute(1),
            y: Position::Absolute(content_start + i),
        });
        match line {
            ContentLine::Header(text) => {
                changes.push(AttributeChange::Intensity(Intensity::Bold).into());
                changes.push(Change::Text(truncate(text, size.cols.saturating_sub(2))));
                changes.push(AttributeChange::Intensity(Intensity::Normal).into());
            }
            ContentLine::KeyValue(key, value) => {
                changes.push(AttributeChange::Foreground(ColorAttribute::PaletteIndex(6)).into()); // cyan
                changes.push(Change::Text(format!("{:<20}", truncate(key, 20))));
                changes.push(AttributeChange::Foreground(ColorAttribute::Default).into());
                changes.push(Change::Text(truncate(value, size.cols.saturating_sub(23))));
            }
            ContentLine::Plain(text) => {
                changes.push(Change::Text(truncate(text, size.cols.saturating_sub(2))));
            }
            ContentLine::Blank => {}
            ContentLine::Separator => {
                changes.push(AttributeChange::Foreground(ColorAttribute::PaletteIndex(8)).into());
                changes.push(Change::Text("\u{2508}".repeat(size.cols.saturating_sub(2))));
                changes.push(AttributeChange::Foreground(ColorAttribute::Default).into());
            }
        }
    }

    // Status bar
    changes.push(Change::CursorPosition {
        x: Position::Absolute(0),
        y: Position::Absolute(size.rows.saturating_sub(1)),
    });
    changes.push(AttributeChange::Reverse(true).into());
    let status = format!(
        " Tab/Arrow: switch section | j/k/Up/Down: scroll | q/Esc: close {:>w$}",
        format!("line {}/{}", scroll_offset + 1, lines.len().max(1)),
        w = size.cols.saturating_sub(66).max(0),
    );
    changes.push(Change::Text(truncate(&status, size.cols)));
    changes.push(AttributeChange::Reverse(false).into());

    term.render(&changes)?;
    term.flush()?;
    Ok(())
}

#[derive(Clone, Debug)]
enum ContentLine {
    Header(String),
    KeyValue(String, String),
    Plain(String),
    Blank,
    Separator,
}

fn build_content(
    snapshot: &DebugSnapshot,
    active_section: usize,
    _cols: usize,
) -> Vec<ContentLine> {
    let section = Section::all()[active_section];
    match section {
        Section::Window => build_window_section(snapshot),
        Section::Tabs => build_tabs_section(snapshot),
        Section::Panes => build_panes_section(snapshot),
        Section::Config => build_config_section(snapshot),
    }
}

fn build_window_section(snapshot: &DebugSnapshot) -> Vec<ContentLine> {
    let mut lines = vec![];

    lines.push(ContentLine::Header("Rendering".into()));
    lines.push(ContentLine::Blank);
    lines.push(ContentLine::KeyValue("FPS".into(), format!("{:.1}", snapshot.fps)));
    lines.push(ContentLine::KeyValue("Frame Time".into(), format!("{:.2} ms", snapshot.last_frame_ms)));
    lines.push(ContentLine::KeyValue("GPU".into(), snapshot.gpu_info.clone()));
    lines.push(ContentLine::Blank);

    lines.push(ContentLine::Header("Window".into()));
    lines.push(ContentLine::Blank);
    lines.push(ContentLine::KeyValue("Window ID".into(), format!("{}", snapshot.window_id)));
    lines.push(ContentLine::KeyValue(
        "Terminal Size".into(),
        format!("{}x{}", snapshot.terminal_size.0, snapshot.terminal_size.1),
    ));
    lines.push(ContentLine::KeyValue(
        "Pixel Size".into(),
        format!("{}x{}", snapshot.pixel_size.0, snapshot.pixel_size.1),
    ));
    lines.push(ContentLine::KeyValue("DPI".into(), format!("{}", snapshot.dpi)));
    lines.push(ContentLine::Blank);

    lines.push(ContentLine::Header("Font".into()));
    lines.push(ContentLine::Blank);
    lines.push(ContentLine::KeyValue("Family".into(), snapshot.font_name.clone()));
    lines.push(ContentLine::KeyValue("Size".into(), format!("{:.1} pt", snapshot.font_size)));

    lines
}

fn build_tabs_section(snapshot: &DebugSnapshot) -> Vec<ContentLine> {
    let mut lines = vec![];

    let mux = Mux::get();
    let window = match mux.get_window(snapshot.window_id) {
        Some(w) => w,
        None => {
            lines.push(ContentLine::Plain("Window not found".into()));
            return lines;
        }
    };

    let active_idx = window.get_active_idx();
    let tab_count = window.len();
    lines.push(ContentLine::Header(format!("Tabs ({})", tab_count)));
    lines.push(ContentLine::Blank);

    for (idx, tab) in window.iter().enumerate() {
        let tab_id = tab.tab_id();
        let title = tab.get_title();
        let is_active = idx == active_idx;
        let pane_count = tab.iter_panes_ignoring_zoom().len();
        let size = tab.get_size();

        let marker = if is_active { "\u{25b6}" } else { " " };
        lines.push(ContentLine::Plain(format!(
            "{} Tab {} (id={}) \u{2014} {}",
            marker, idx, tab_id, title
        )));
        lines.push(ContentLine::KeyValue(
            "  Size".into(),
            format!("{}x{}", size.cols, size.rows),
        ));
        lines.push(ContentLine::KeyValue("  Panes".into(), format!("{}", pane_count)));
        lines.push(ContentLine::Separator);
    }

    lines
}

fn build_panes_section(snapshot: &DebugSnapshot) -> Vec<ContentLine> {
    let mut lines = vec![];

    let mux = Mux::get();
    let window = match mux.get_window(snapshot.window_id) {
        Some(w) => w,
        None => {
            lines.push(ContentLine::Plain("Window not found".into()));
            return lines;
        }
    };

    let active_idx = window.get_active_idx();

    for (idx, tab) in window.iter().enumerate() {
        let is_active_tab = idx == active_idx;
        let tab_marker = if is_active_tab { "\u{25b6}" } else { " " };
        lines.push(ContentLine::Header(format!(
            "{} Tab {} (id={})",
            tab_marker, idx, tab.tab_id()
        )));
        lines.push(ContentLine::Blank);

        for pos in tab.iter_panes_ignoring_zoom() {
            let pane = &pos.pane;
            let pane_id = pane.pane_id();
            let title = pane.get_title();
            let dims = pane.get_dimensions();
            let process = pane
                .get_foreground_process_name(CachePolicy::AllowStale)
                .unwrap_or_else(|| "<unknown>".into());
            let cwd = pane
                .get_current_working_dir(CachePolicy::AllowStale)
                .map(|u| u.path().to_string())
                .unwrap_or_else(|| "<unknown>".into());
            let user_vars = pane.copy_user_vars();

            let active_marker = if pos.is_active { "*" } else { " " };
            lines.push(ContentLine::Plain(format!(
                " {} Pane {} \u{2014} {}",
                active_marker, pane_id, title
            )));
            lines.push(ContentLine::KeyValue(
                "    Process".into(),
                process,
            ));
            lines.push(ContentLine::KeyValue(
                "    CWD".into(),
                cwd,
            ));
            lines.push(ContentLine::KeyValue(
                "    Size".into(),
                format!(
                    "{}x{} ({}x{} px)",
                    dims.cols, dims.viewport_rows,
                    dims.pixel_width, dims.pixel_height
                ),
            ));
            lines.push(ContentLine::KeyValue(
                "    Scrollback".into(),
                format!("{} lines", dims.scrollback_rows),
            ));
            lines.push(ContentLine::KeyValue(
                "    Position".into(),
                format!(
                    "left={} top={} ({}x{})",
                    pos.left, pos.top, pos.width, pos.height
                ),
            ));
            if pos.is_zoomed {
                lines.push(ContentLine::KeyValue("    Zoomed".into(), "yes".into()));
            }
            if !user_vars.is_empty() {
                lines.push(ContentLine::KeyValue(
                    "    User Vars".into(),
                    format!("{} set", user_vars.len()),
                ));
                for (k, v) in &user_vars {
                    let display_v = if v.len() > 50 {
                        format!("{}...", &v[..47])
                    } else {
                        v.clone()
                    };
                    lines.push(ContentLine::KeyValue(
                        format!("      {}", k),
                        display_v,
                    ));
                }
            }
            lines.push(ContentLine::Separator);
        }
    }

    lines
}

fn build_config_section(snapshot: &DebugSnapshot) -> Vec<ContentLine> {
    let mut lines = vec![];
    let config = config::configuration();

    lines.push(ContentLine::Header("Configuration".into()));
    lines.push(ContentLine::Blank);
    lines.push(ContentLine::KeyValue("Config File".into(), snapshot.config_file.clone()));
    lines.push(ContentLine::Blank);

    lines.push(ContentLine::Header("Appearance".into()));
    lines.push(ContentLine::Blank);
    lines.push(ContentLine::KeyValue(
        "Color Scheme".into(),
        config.color_scheme.as_deref().unwrap_or("<default>").into(),
    ));
    lines.push(ContentLine::KeyValue(
        "Tab Bar".into(),
        if config.enable_tab_bar {
            if config.use_fancy_tab_bar { "fancy" } else { "classic" }
        } else {
            "hidden"
        }.into(),
    ));
    lines.push(ContentLine::KeyValue(
        "Tab Bar Position".into(),
        if config.tab_bar_at_bottom { "bottom" } else { "top" }.into(),
    ));
    lines.push(ContentLine::KeyValue(
        "Scrollback".into(),
        format!("{} lines", config.scrollback_lines),
    ));
    lines.push(ContentLine::KeyValue("Max FPS".into(), format!("{}", config.max_fps)));
    lines.push(ContentLine::Blank);

    lines.push(ContentLine::Header("Terminal".into()));
    lines.push(ContentLine::Blank);
    lines.push(ContentLine::KeyValue("TERM".into(), config.term.clone()));
    lines.push(ContentLine::KeyValue(
        "Cursor Style".into(),
        format!("{:?}", config.default_cursor_style),
    ));
    lines.push(ContentLine::KeyValue(
        "Cursor Blink".into(),
        format!("{} ms", config.cursor_blink_rate),
    ));
    lines.push(ContentLine::Blank);

    lines.push(ContentLine::Header("Domains".into()));
    lines.push(ContentLine::Blank);
    let mux = Mux::get();
    for domain in mux.iter_domains() {
        let state = domain.state();
        lines.push(ContentLine::KeyValue(
            format!("  {}", domain.domain_name()),
            format!("id={} state={:?}", domain.domain_id(), state),
        ));
    }

    lines
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect()
    }
}
