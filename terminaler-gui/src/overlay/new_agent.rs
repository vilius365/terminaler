//! New Claude Agent overlay: prompts for a branch name, creates a git
//! worktree for it, and spawns a Claude session tab in that worktree.
//!
//! Runs on a dedicated overlay thread (see `start_overlay`), so the
//! blocking `git worktree add` call here never stalls the GUI thread.

use crate::termwindow::TermWindowNotif;
use crate::worktree;
use mux::termwiztermtab::TermWizTerminal;
use std::path::PathBuf;
use termwiz::input::{InputEvent, KeyCode, KeyEvent};
use termwiz::lineedit::*;
use termwiz::surface::Change;
use termwiz::terminal::Terminal;
use window::WindowOps;

struct PromptHost {
    history: BasicHistory,
}

impl LineEditorHost for PromptHost {
    fn history(&mut self) -> &mut dyn History {
        &mut self.history
    }

    fn resolve_action(
        &mut self,
        event: &InputEvent,
        _editor: &mut LineEditor<'_>,
    ) -> Option<Action> {
        if matches!(
            event,
            InputEvent::Key(KeyEvent {
                key: KeyCode::Escape,
                ..
            })
        ) {
            Some(Action::Cancel)
        } else {
            None
        }
    }
}

fn render_line(term: &mut TermWizTerminal, text: &str) -> anyhow::Result<()> {
    term.render(&[Change::Text(format!("{}\r\n", text))])?;
    Ok(())
}

/// Block until any key is pressed, so error output stays readable.
fn wait_for_key(term: &mut TermWizTerminal) {
    while let Ok(Some(event)) = term.poll_input(None) {
        if matches!(event, InputEvent::Key(_)) {
            break;
        }
    }
}

pub fn show_new_agent_overlay(
    mut term: TermWizTerminal,
    env: worktree::GitEnv,
    repo_root: anyhow::Result<String>,
    worktree_root: Option<PathBuf>,
    window: ::window::Window,
) -> anyhow::Result<()> {
    term.no_grab_mouse_in_raw_mode();
    term.render(&[Change::Title("New Claude Agent".to_string())])?;

    render_line(&mut term, "New Claude Agent")?;

    let repo_root = match repo_root {
        Ok(root) => root,
        Err(err) => {
            render_line(&mut term, &format!("Error: {:#}", err))?;
            render_line(&mut term, "Press any key to close")?;
            wait_for_key(&mut term);
            return Ok(());
        }
    };

    render_line(&mut term, &format!("Repo: {}", repo_root))?;
    if let worktree::GitEnv::Wsl { distro } = &env {
        render_line(&mut term, &format!("WSL distro: {}", distro))?;
    }
    render_line(&mut term, "")?;

    let mut host = PromptHost {
        history: BasicHistory::default(),
    };
    let mut editor = LineEditor::new(&mut term);
    editor.set_prompt("Branch name (Esc=cancel): ");
    let line = editor.read_line_with_optional_initial_value(&mut host, Some("agent/"))?;

    let branch = match line {
        Some(branch) => branch.trim().to_string(),
        None => return Ok(()), // cancelled
    };

    if let Err(err) = worktree::validate_branch_name(&branch) {
        render_line(&mut term, "")?;
        render_line(&mut term, &format!("Error: {:#}", err))?;
        render_line(&mut term, "Press any key to close")?;
        wait_for_key(&mut term);
        return Ok(());
    }

    render_line(&mut term, "")?;
    render_line(&mut term, &format!("Creating worktree for `{}`...", branch))?;

    match worktree::create_worktree_in(&env, &repo_root, &branch, worktree_root.as_deref()) {
        Ok(path) => {
            render_line(&mut term, &format!("Worktree ready: {}", path))?;
            let path = PathBuf::from(path);
            window.notify(TermWindowNotif::Apply(Box::new(move |term_window| {
                term_window.spawn_claude_agent_tab(env, path);
            })));
        }
        Err(err) => {
            render_line(&mut term, &format!("Failed: {:#}", err))?;
            render_line(&mut term, "Press any key to close")?;
            wait_for_key(&mut term);
        }
    }

    Ok(())
}
