use crate::scripting::guiwin::GuiWin;
use config::keyassignment::Confirmation;
use mux::termwiztermtab::TermWizTerminal;
use crate::scripting::guiwin::MuxPane;
use termwiz::cell::{unicode_column_width, AttributeChange};
use termwiz::color::ColorAttribute;
use termwiz::input::{InputEvent, KeyCode, KeyEvent, MouseButtons, MouseEvent};
use termwiz::surface::{Change, CursorVisibility, Position};
use termwiz::terminal::Terminal;

pub fn run_confirmation(message: &str, term: &mut TermWizTerminal) -> anyhow::Result<bool> {
    run_confirmation_impl(message, term)
}

/// Wrap text respecting Unicode display width.
/// textwrap may miscalculate widths for emoji/CJK characters,
/// so we do our own simple word-wrapping using terminal column widths.
fn wrap_to_terminal_width(text: &str, max_width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for input_line in text.split('\n') {
        if input_line.is_empty() {
            lines.push(String::new());
            continue;
        }
        let mut current_line = String::new();
        let mut current_width = 0usize;
        for word in input_line.split_whitespace() {
            let word_width = unicode_column_width(word, None);
            if current_width > 0 && current_width + 1 + word_width > max_width {
                lines.push(current_line);
                current_line = word.to_string();
                current_width = word_width;
            } else if current_width > 0 {
                current_line.push(' ');
                current_line.push_str(word);
                current_width += 1 + word_width;
            } else {
                current_line.push_str(word);
                current_width = word_width;
            }
        }
        lines.push(current_line);
    }
    lines
}

fn run_confirmation_impl(message: &str, term: &mut TermWizTerminal) -> anyhow::Result<bool> {
    term.set_raw_mode()?;

    let size = term.get_screen_size()?;

    // Render 60% wide, centered (narrower for a dialog feel)
    let text_width = (size.cols * 60 / 100).max(30);
    let x_pos = (size.cols.saturating_sub(text_width)) / 2;

    // Wrap text using Unicode-aware width calculation
    let lines = wrap_to_terminal_width(message, text_width);

    let message_rows = lines.len();
    // Vertically center: message lines + 1 blank line + 1 button row
    let total_rows = message_rows + 2;
    let top_row = size.rows.saturating_sub(total_rows) / 2;

    let button_row = top_row + message_rows + 1;
    let mut active = ActiveButton::None;

    // Center buttons within the text area
    let buttons_total_width = 7 + 4 + 6; // " [Y]es " + "    " + " [N]o "
    let button_start = x_pos + text_width.saturating_sub(buttons_total_width) / 2;

    let yes_x = button_start;
    let yes_w = 7;

    let no_x = yes_x + yes_w + 4;
    let no_w = 6;

    #[derive(Copy, Clone, PartialEq, Eq)]
    enum ActiveButton {
        None,
        Yes,
        No,
    }

    let render = |term: &mut TermWizTerminal, active: ActiveButton| -> termwiz::Result<()> {
        let mut changes = vec![
            Change::ClearScreen(ColorAttribute::Default),
            Change::CursorVisibility(CursorVisibility::Hidden),
        ];

        // Render message lines
        for (y, row) in lines.iter().enumerate() {
            changes.push(Change::CursorPosition {
                x: Position::Absolute(x_pos),
                y: Position::Absolute(top_row + y),
            });
            changes.push(Change::Text(row.to_string()));
        }

        // Render buttons
        changes.push(Change::CursorPosition {
            x: Position::Absolute(yes_x),
            y: Position::Absolute(button_row),
        });

        if active == ActiveButton::Yes {
            changes.push(AttributeChange::Reverse(true).into());
        }
        changes.push(" [Y]es ".into());
        if active == ActiveButton::Yes {
            changes.push(AttributeChange::Reverse(false).into());
        }

        changes.push("    ".into());

        if active == ActiveButton::No {
            changes.push(AttributeChange::Reverse(true).into());
        }
        changes.push(" [N]o ".into());
        if active == ActiveButton::No {
            changes.push(AttributeChange::Reverse(false).into());
        }

        term.render(&changes)?;
        term.flush()
    };

    render(term, active)?;

    while let Ok(Some(event)) = term.poll_input(None) {
        match event {
            InputEvent::Key(KeyEvent {
                key: KeyCode::Char('y' | 'Y'),
                ..
            }) => {
                return Ok(true);
            }
            InputEvent::Key(KeyEvent {
                key: KeyCode::Char('n' | 'N'),
                ..
            })
            | InputEvent::Key(KeyEvent {
                key: KeyCode::Escape,
                ..
            }) => {
                return Ok(false);
            }
            InputEvent::Mouse(MouseEvent {
                x,
                y,
                mouse_buttons,
                ..
            }) => {
                let x = x as usize;
                let y = y as usize;
                if y == button_row && x >= yes_x && x < yes_x + yes_w {
                    active = ActiveButton::Yes;
                    if mouse_buttons == MouseButtons::LEFT {
                        return Ok(true);
                    }
                } else if y == button_row && x >= no_x && x < no_x + no_w {
                    active = ActiveButton::No;
                    if mouse_buttons == MouseButtons::LEFT {
                        return Ok(false);
                    }
                } else {
                    active = ActiveButton::None;
                }

                if mouse_buttons != MouseButtons::NONE {
                    // Treat any other mouse button as cancel
                    return Ok(false);
                }
            }
            _ => {}
        }

        render(term, active)?;
    }

    Ok(false)
}

pub fn show_confirmation_overlay(
    mut term: TermWizTerminal,
    args: Confirmation,
    _window: GuiWin,
    _pane: MuxPane,
) -> anyhow::Result<()> {
    // Lua EmitEvent callback removed; confirmation result is ignored (no-op on confirm)
    run_confirmation_impl(&args.message, &mut term).ok();
    Ok(())
}
