//! Worktree manager overlay: lists the repository's git worktrees with their
//! dirty state and offers two destructive lifecycle actions — "merge & remove"
//! (fold the branch into the main worktree, then delete) and "discard"
//! (force-remove the worktree and its branch). Both require confirmation.
//!
//! Runs on a dedicated overlay thread (see `start_overlay`), so the blocking
//! `git` calls here never stall the GUI thread.

use crate::worktree::{self, GitEnv, WorktreeInfo};
use mux::termwiztermtab::TermWizTerminal;
use termwiz::cell::{AttributeChange, CellAttributes};
use termwiz::color::ColorAttribute;
use termwiz::input::{InputEvent, KeyCode, KeyEvent};
use termwiz::surface::{Change, Position};
use termwiz::terminal::Terminal;

struct ManagerState {
    env: GitEnv,
    repo_root: String,
    repo_name: String,
    worktrees: Vec<WorktreeInfo>,
    selected: usize,
}

impl ManagerState {
    fn refresh(&mut self) -> anyhow::Result<()> {
        self.worktrees = worktree::list_worktrees(&self.env, &self.repo_root)?;
        if self.selected >= self.worktrees.len() {
            self.selected = self.worktrees.len().saturating_sub(1);
        }
        Ok(())
    }

    fn main_worktree(&self) -> Option<&WorktreeInfo> {
        self.worktrees.iter().find(|w| w.is_main)
    }

    fn render(&self, term: &mut TermWizTerminal) -> anyhow::Result<()> {
        let mut changes = vec![
            Change::ClearScreen(ColorAttribute::Default),
            Change::CursorPosition {
                x: Position::Absolute(0),
                y: Position::Absolute(0),
            },
            Change::Text(format!("Worktree Manager — {}\r\n", self.repo_name)),
        ];

        let base = self
            .main_worktree()
            .and_then(|w| w.branch.clone())
            .unwrap_or_else(|| "(detached)".to_string());
        changes.push(Change::Text(format!("Base branch: {base}\r\n\r\n")));

        for (idx, wt) in self.worktrees.iter().enumerate() {
            let selected = idx == self.selected;
            if selected {
                changes.push(AttributeChange::Reverse(true).into());
            }

            let marker = if selected { ">" } else { " " };
            let branch = wt.branch.as_deref().unwrap_or("(detached)");
            let tag = if wt.is_main { " (main)" } else { "" };
            let status = if wt.is_bare {
                "bare"
            } else if wt.dirty {
                "● dirty"
            } else {
                "✓ clean"
            };
            changes.push(Change::Text(format!(
                "{marker} {branch}{tag}\r\n    {}\r\n    {status}\r\n",
                wt.path
            )));

            if selected {
                changes.push(AttributeChange::Reverse(false).into());
                changes.push(Change::AllAttributes(CellAttributes::default()));
            }
        }

        changes.push(Change::Text(
            "\r\n↑/↓ or j/k select · m merge & remove · d discard · r refresh · Esc close\r\n"
                .to_string(),
        ));
        term.render(&changes)?;
        Ok(())
    }
}

fn render_line(term: &mut TermWizTerminal, text: &str) -> anyhow::Result<()> {
    term.render(&[Change::Text(format!("{}\r\n", text))])?;
    Ok(())
}

/// Render a yes/no prompt and read a single key. Returns true only for y/Y.
fn confirm(term: &mut TermWizTerminal, lines: &[String]) -> anyhow::Result<bool> {
    let mut changes = vec![
        Change::ClearScreen(ColorAttribute::Default),
        Change::CursorPosition {
            x: Position::Absolute(0),
            y: Position::Absolute(0),
        },
    ];
    for line in lines {
        changes.push(Change::Text(format!("{line}\r\n")));
    }
    changes.push(Change::Text(
        "\r\nPress y to confirm, any other key to cancel.\r\n".to_string(),
    ));
    term.render(&changes)?;

    while let Ok(Some(event)) = term.poll_input(None) {
        if let InputEvent::Key(KeyEvent { key, .. }) = event {
            return Ok(matches!(key, KeyCode::Char('y') | KeyCode::Char('Y')));
        }
    }
    Ok(false)
}

/// Show a result message and wait for any key.
fn show_result(term: &mut TermWizTerminal, message: &str) -> anyhow::Result<()> {
    render_line(term, "")?;
    render_line(term, message)?;
    render_line(term, "Press any key to continue")?;
    while let Ok(Some(event)) = term.poll_input(None) {
        if matches!(event, InputEvent::Key(_)) {
            break;
        }
    }
    Ok(())
}

fn do_discard(term: &mut TermWizTerminal, state: &ManagerState, wt: &WorktreeInfo) -> anyhow::Result<()> {
    if wt.is_main {
        return show_result(term, "Cannot remove the main worktree.");
    }
    let branch = wt.branch.as_deref().unwrap_or("(detached)");
    let mut prompt = vec![
        format!("Discard worktree `{branch}`?"),
        format!("  path: {}", wt.path),
        "  Force-removes the worktree and deletes the branch (irreversible).".to_string(),
    ];
    if wt.dirty {
        prompt.push("  ⚠ Uncommitted changes will be lost.".to_string());
    }
    if !confirm(term, &prompt)? {
        return Ok(());
    }
    match worktree::discard_worktree(&state.env, &state.repo_root, wt) {
        Ok(msg) => show_result(term, &msg),
        Err(e) => show_result(term, &format!("Discard failed: {e:#}")),
    }
}

fn do_merge(term: &mut TermWizTerminal, state: &ManagerState, wt: &WorktreeInfo) -> anyhow::Result<()> {
    if wt.is_main {
        return show_result(term, "Cannot merge/remove the main worktree.");
    }
    let main_path = match state.main_worktree() {
        Some(m) => m.path.clone(),
        None => return show_result(term, "No main worktree found."),
    };
    let branch = match &wt.branch {
        Some(b) => b.clone(),
        None => return show_result(term, "Worktree has a detached HEAD; nothing to merge."),
    };
    if wt.dirty {
        return show_result(
            term,
            "Worktree has uncommitted changes; commit or discard them before merging.",
        );
    }
    let base = state
        .main_worktree()
        .and_then(|w| w.branch.clone())
        .unwrap_or_else(|| "the main worktree".to_string());
    let prompt = vec![
        format!("Merge `{branch}` into `{base}` and remove the worktree + branch?"),
        format!("  path: {}", wt.path),
    ];
    if !confirm(term, &prompt)? {
        return Ok(());
    }
    match worktree::merge_and_remove(&state.env, &state.repo_root, &main_path, wt) {
        Ok(msg) => show_result(term, &msg),
        Err(e) => show_result(term, &format!("Merge & remove failed: {e:#}")),
    }
}

pub fn show_worktree_manager_overlay(
    mut term: TermWizTerminal,
    env: GitEnv,
    repo_root: anyhow::Result<String>,
) -> anyhow::Result<()> {
    term.set_raw_mode()?;
    term.render(&[Change::Title("Worktree Manager".to_string())])?;

    let repo_root = match repo_root {
        Ok(root) => root,
        Err(err) => {
            render_line(&mut term, &format!("Error: {:#}", err))?;
            render_line(&mut term, "Press any key to close")?;
            while let Ok(Some(event)) = term.poll_input(None) {
                if matches!(event, InputEvent::Key(_)) {
                    break;
                }
            }
            return Ok(());
        }
    };

    let repo_name = repo_root
        .trim_end_matches(|c| c == '/' || c == '\\')
        .rsplit(|c| c == '/' || c == '\\')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("repo")
        .to_string();

    let mut state = ManagerState {
        env,
        repo_root,
        repo_name,
        worktrees: vec![],
        selected: 0,
    };
    if let Err(err) = state.refresh() {
        render_line(&mut term, &format!("Failed to list worktrees: {:#}", err))?;
        render_line(&mut term, "Press any key to close")?;
        while let Ok(Some(event)) = term.poll_input(None) {
            if matches!(event, InputEvent::Key(_)) {
                break;
            }
        }
        return Ok(());
    }

    state.render(&mut term)?;

    while let Ok(Some(event)) = term.poll_input(None) {
        match event {
            InputEvent::Key(KeyEvent { key, .. }) => match key {
                KeyCode::Char('q') | KeyCode::Escape => break,
                KeyCode::UpArrow | KeyCode::Char('k') => {
                    state.selected = state.selected.saturating_sub(1);
                }
                KeyCode::DownArrow | KeyCode::Char('j') => {
                    if !state.worktrees.is_empty() {
                        state.selected =
                            (state.selected + 1).min(state.worktrees.len() - 1);
                    }
                }
                KeyCode::Char('r') => {
                    let _ = state.refresh();
                }
                KeyCode::Char('d') => {
                    if let Some(wt) = state.worktrees.get(state.selected).cloned() {
                        do_discard(&mut term, &state, &wt)?;
                        let _ = state.refresh();
                    }
                }
                KeyCode::Char('m') => {
                    if let Some(wt) = state.worktrees.get(state.selected).cloned() {
                        do_merge(&mut term, &state, &wt)?;
                        let _ = state.refresh();
                    }
                }
                _ => {}
            },
            _ => {}
        }
        state.render(&mut term)?;
    }

    Ok(())
}
