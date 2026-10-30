use super::confirm;
use crate::TermWindow;
use mux::pane::PaneId;
use mux::tab::TabId;
use mux::termwiztermtab::TermWizTerminal;
use mux::window::WindowId;
use mux::Mux;

pub fn confirm_close_pane(
    pane_id: PaneId,
    mut term: TermWizTerminal,
    mux_window_id: WindowId,
    window: ::window::Window,
) -> anyhow::Result<()> {
    let group_size = Mux::get()
        .get_active_tab_for_window(mux_window_id)
        .map_or(1, |tab| tab.unit_panes_of(pane_id).len());
    let question = if group_size > 1 {
        format!("Really close this group of {} panes?", group_size)
    } else {
        "Really close this pane?".to_string()
    };
    if confirm::run_confirmation(&question, &mut term)? {
        promise::spawn::spawn_into_main_thread(async move {
            let mux = Mux::get();
            let tab = match mux.get_active_tab_for_window(mux_window_id) {
                Some(tab) => tab,
                None => return,
            };
            // Take the member list first: killing one member changes the group.
            for id in tab.unit_panes_of(pane_id) {
                tab.kill_pane(id);
            }
        })
        .detach();
    }
    TermWindow::schedule_cancel_overlay_for_pane(window, pane_id);

    Ok(())
}

pub fn confirm_close_tab(
    tab_id: TabId,
    mut term: TermWizTerminal,
    _mux_window_id: WindowId,
    window: ::window::Window,
) -> anyhow::Result<()> {
    if confirm::run_confirmation(
        "Really close this tab and all contained panes?",
        &mut term,
    )? {
        promise::spawn::spawn_into_main_thread(async move {
            let mux = Mux::get();
            mux.remove_tab(tab_id);
        })
        .detach();
    }
    TermWindow::schedule_cancel_overlay(window, tab_id, None);

    Ok(())
}

pub fn confirm_close_window(
    mut term: TermWizTerminal,
    mux_window_id: WindowId,
    window: ::window::Window,
    tab_id: TabId,
) -> anyhow::Result<()> {
    if confirm::run_confirmation(
        "Close this window? All tabs and panes will be terminated.",
        &mut term,
    )? {
        promise::spawn::spawn_into_main_thread(async move {
            let mux = Mux::get();
            mux.kill_window(mux_window_id);
        })
        .detach();
    }
    TermWindow::schedule_cancel_overlay(window, tab_id, None);

    Ok(())
}

pub fn confirm_quit_program(
    mut term: TermWizTerminal,
    window: ::window::Window,
    tab_id: TabId,
) -> anyhow::Result<()> {
    if confirm::run_confirmation("Quit Terminaler? All windows, tabs, and panes will be terminated.", &mut term)? {
        promise::spawn::spawn_into_main_thread(async move {
            use ::window::{Connection, ConnectionOps};
            let con = Connection::get().expect("call on gui thread");
            con.terminate_message_loop();
        })
        .detach();
    }
    TermWindow::schedule_cancel_overlay(window, tab_id, None);

    Ok(())
}
