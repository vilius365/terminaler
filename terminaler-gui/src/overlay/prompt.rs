use crate::scripting::guiwin::GuiWin;
use config::keyassignment::PromptInputLine;
use mux::termwiztermtab::TermWizTerminal;
use crate::scripting::guiwin::MuxPane;
use termwiz::input::{InputEvent, KeyCode, KeyEvent};
use termwiz::lineedit::*;
use termwiz::surface::Change;
use termwiz::terminal::Terminal;

struct PromptHost {
    history: BasicHistory,
}

impl PromptHost {
    fn new() -> Self {
        Self {
            history: BasicHistory::default(),
        }
    }
}

impl LineEditorHost for PromptHost {
    fn history(&mut self) -> &mut dyn History {
        &mut self.history
    }

    fn resolve_action(
        &mut self,
        event: &InputEvent,
        editor: &mut LineEditor<'_>,
    ) -> Option<Action> {
        let (line, _cursor) = editor.get_line_and_cursor();
        if line.is_empty()
            && matches!(
                event,
                InputEvent::Key(KeyEvent {
                    key: KeyCode::Escape,
                    ..
                })
            )
        {
            Some(Action::Cancel)
        } else {
            None
        }
    }
}

pub fn show_line_prompt_overlay(
    mut term: TermWizTerminal,
    args: PromptInputLine,
    _window: GuiWin,
    _pane: MuxPane,
) -> anyhow::Result<()> {
    // Lua EmitEvent callback removed; prompt result is not dispatched
    term.no_grab_mouse_in_raw_mode();
    let mut text = args.description.replace("\r\n", "\n").replace("\n", "\r\n");
    text.push_str("\r\n");
    term.render(&[Change::Text(text)])?;

    let mut host = PromptHost::new();
    let mut editor = LineEditor::new(&mut term);
    editor.set_prompt(&args.prompt);
    let _line =
        editor.read_line_with_optional_initial_value(&mut host, args.initial_value.as_deref())?;

    Ok(())
}
