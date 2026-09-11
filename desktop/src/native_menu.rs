//! The menus in the system's menu bar, on macOS.
//!
//! The bar is built once from the list [`menu::titles`](crate::menu::titles)
//! makes, and kept in step with it by changing rows in place — enabled,
//! ticked, the recent files — rather than by building another, which could
//! land while one of its menus is open.
//!
//! A chosen row arrives as a callback on the main thread, from AppKit and not
//! through winit, so nothing tells the window's loop to take a frame: an idle
//! window would act on ⌘S when the mouse next moved over it. [`wake`] is the
//! nudge that makes it act now.

use crate::menu::{Command, Entry, Role, System, Title};
use muda::accelerator::Accelerator;
use muda::{
    AboutMetadata, CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu,
};
use std::collections::HashMap;
use std::sync::{Mutex, Once};

/// Rows chosen since the window last looked.
static CHOSEN: Mutex<Vec<MenuId>> = Mutex::new(Vec::new());

enum Row {
    Plain(MenuItem),
    Check(CheckMenuItem),
}

pub struct NativeMenu {
    /// The bar. Held so it lives as long as the window.
    _menu: Menu,
    /// The menus in it, for the same reason.
    _submenus: Vec<Submenu>,
    /// Which command each row's id is.
    commands: HashMap<MenuId, Command>,
    /// The rows whose state can change, by what they do.
    rows: HashMap<Command, Row>,
    /// Open Recent, whose rows are replaced when the list changes, and the
    /// rows it holds now.
    recent: Option<(Submenu, Vec<Entry>)>,
}

impl NativeMenu {
    /// Builds the bar from `titles` and gives it to the application.
    pub fn new(titles: &[Title]) -> Self {
        static HANDLER: Once = Once::new();
        HANDLER.call_once(|| {
            MenuEvent::set_event_handler(Some(|event: MenuEvent| {
                if let Ok(mut chosen) = CHOSEN.lock() {
                    chosen.push(event.id);
                }
                wake();
            }));
        });

        let mut this = Self {
            _menu: Menu::new(),
            _submenus: Vec::new(),
            commands: HashMap::new(),
            rows: HashMap::new(),
            recent: None,
        };
        let mut roles = Vec::new();
        for title in titles {
            let submenu = Submenu::new(&title.label, true);
            this.fill(&submenu, &title.entries, true);
            let _ = this._menu.append(&submenu);
            roles.push((submenu.clone(), title.role));
            this._submenus.push(submenu);
        }
        this._menu.init_for_nsapp();
        for (submenu, role) in roles {
            match role {
                Role::Window => submenu.set_as_windows_menu_for_nsapp(),
                Role::Help => submenu.set_as_help_menu_for_nsapp(),
                Role::App | Role::Plain => {}
            }
        }
        this
    }

    /// The commands chosen since the last call, oldest first.
    pub fn chosen(&self) -> Vec<Command> {
        let ids = CHOSEN
            .lock()
            .map(|mut chosen| std::mem::take(&mut *chosen))
            .unwrap_or_default();
        ids.iter()
            .filter_map(|id| self.commands.get(id).copied())
            .collect()
    }

    /// Brings the rows up to date with `titles`, which must be the same menus
    /// the bar was built from.
    pub fn sync(&mut self, titles: &[Title]) {
        for title in titles {
            self.sync_entries(&title.entries);
        }
    }

    fn sync_entries(&mut self, entries: &[Entry]) {
        for entry in entries {
            match entry {
                Entry::Command {
                    command,
                    enabled,
                    checked,
                    ..
                } => match self.rows.get(command) {
                    Some(Row::Plain(row)) => row.set_enabled(*enabled),
                    Some(Row::Check(row)) => {
                        row.set_enabled(*enabled);
                        row.set_checked(checked.unwrap_or(false));
                    }
                    None => {}
                },
                Entry::Submenu { entries, .. } if is_recent(entries) => {
                    let Some((submenu, shown)) = self.recent.take() else {
                        continue;
                    };
                    if *entries != shown {
                        while submenu.remove_at(0).is_some() {}
                        self.fill(&submenu, entries, false);
                    }
                    self.recent = Some((submenu, entries.clone()));
                }
                Entry::Submenu { entries, .. } => self.sync_entries(entries),
                Entry::System(_) | Entry::Separator => {}
            }
        }
    }

    /// Adds `entries` to `parent`. `track` keeps the rows to be updated in
    /// place; the recent files are replaced whole instead.
    fn fill(&mut self, parent: &Submenu, entries: &[Entry], track: bool) {
        for entry in entries {
            match entry {
                Entry::Command {
                    command,
                    label,
                    keys,
                    enabled,
                    checked,
                } => {
                    let id = MenuId::new(format!("{command:?}"));
                    let keys = accelerator(keys);
                    let row = match checked {
                        Some(on) => {
                            let row =
                                CheckMenuItem::with_id(id.clone(), label, *enabled, *on, keys);
                            let _ = parent.append(&row);
                            Row::Check(row)
                        }
                        None => {
                            let row = MenuItem::with_id(id.clone(), label, *enabled, keys);
                            let _ = parent.append(&row);
                            Row::Plain(row)
                        }
                    };
                    self.commands.insert(id, *command);
                    if track {
                        self.rows.insert(*command, row);
                    }
                }
                Entry::Submenu { label, entries } => {
                    let submenu = Submenu::new(label, true);
                    let _ = parent.append(&submenu);
                    if is_recent(entries) {
                        self.fill(&submenu, entries, false);
                        self.recent = Some((submenu.clone(), entries.clone()));
                    } else {
                        self.fill(&submenu, entries, track);
                    }
                    self._submenus.push(submenu);
                }
                Entry::System(system) => {
                    let _ = parent.append(&predefined(*system));
                }
                Entry::Separator => {
                    let _ = parent.append(&PredefinedMenuItem::separator());
                }
            }
        }
    }
}

/// Whether a submenu is Open Recent: it is the one that clears itself.
fn is_recent(entries: &[Entry]) -> bool {
    entries.iter().any(|e| {
        matches!(
            e,
            Entry::Command {
                command: Command::ClearRecent,
                ..
            }
        )
    })
}

/// `Cmd+Shift+S` as muda spells it. `Cmd` is Command here, and the menus in the
/// window are where Ctrl is meant.
fn accelerator(keys: &str) -> Option<Accelerator> {
    if keys.is_empty() {
        return None;
    }
    keys.replace("Cmd", "CmdOrCtrl").parse().ok()
}

fn predefined(system: System) -> PredefinedMenuItem {
    match system {
        System::About => PredefinedMenuItem::about(
            Some("About squint"),
            Some(AboutMetadata {
                name: Some("squint".into()),
                version: Some(env!("CARGO_PKG_VERSION").into()),
                comments: Some(env!("CARGO_PKG_DESCRIPTION").into()),
                license: Some(env!("CARGO_PKG_LICENSE").into()),
                website: Some(crate::menu::HOME.into()),
                ..AboutMetadata::default()
            }),
        ),
        System::Services => PredefinedMenuItem::services(None),
        System::Hide => PredefinedMenuItem::hide(Some("Hide squint")),
        System::HideOthers => PredefinedMenuItem::hide_others(None),
        System::ShowAll => PredefinedMenuItem::show_all(None),
        System::Minimize => PredefinedMenuItem::minimize(None),
        System::Zoom => PredefinedMenuItem::maximize(Some("Zoom")),
        System::FullScreen => PredefinedMenuItem::fullscreen(None),
        System::BringAllToFront => PredefinedMenuItem::bring_all_to_front(None),
    }
}

/// Makes the window's loop take a frame, so a chosen row is acted on now.
///
/// denise-winit takes a frame when winit reports something about the window,
/// and has no door for a callback that is not winit's. Winit's window delegate
/// answers `windowDidChangeOcclusionState:` by reading whether the window is
/// visible and reporting that, which changes nothing and is still a report —
/// enough to be drawn next. A redraw request would be the obvious nudge and is
/// the wrong one: the view's own layer holds the frame, and a view asked to
/// display itself draws over it.
fn wake() {
    use objc2::runtime::{AnyObject, NSObjectProtocol};
    use objc2::{MainThreadMarker, msg_send, sel};
    use objc2_app_kit::NSApplication;

    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    let Some(window) = app.keyWindow().or_else(|| app.mainWindow()) else {
        return;
    };
    let Some(delegate) = window.delegate() else {
        return;
    };
    if !delegate.respondsToSelector(sel!(windowDidChangeOcclusionState:)) {
        return;
    }
    let nothing: Option<&AnyObject> = None;
    // SAFETY: on the main thread, to the window's own delegate, which has just
    // said it answers the selector. The argument is the notification, which
    // winit's delegate does not read.
    let () = unsafe { msg_send![&*delegate, windowDidChangeOcclusionState: nothing] };
}

/// Tells AppKit this application's windows are not tabbed together.
///
/// macOS tabs an application's windows unless told otherwise, and puts Show
/// Next Tab and Show Previous Tab of its own in the Window menu, on ⌃⇥ and
/// ⌃⇧⇥. A menu row takes its keys before the window hears of them, so squint's
/// Ctrl+Tab never arrived: the Window menu flashed instead, as AppKit's row went
/// looking for a window tab that was not there. squint's tabs are its own, so
/// the system's are turned off, and their rows and their keys go with them.
pub fn forbid_window_tabs() {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSWindow, NSWindowTabbingMode};

    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    NSWindow::setAllowsAutomaticWindowTabbing(false, mtm);
    // The window is open already, and was made while the answer was yes.
    for window in NSApplication::sharedApplication(mtm).windows().iter() {
        window.setTabbingMode(NSWindowTabbingMode::Disallowed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::menu::{State, titles};

    fn keys_of(entries: &[Entry], out: &mut Vec<&'static str>) {
        for entry in entries {
            match entry {
                Entry::Command { keys, .. } if !keys.is_empty() => out.push(keys),
                Entry::Submenu { entries, .. } => keys_of(entries, out),
                _ => {}
            }
        }
    }

    #[test]
    fn every_shortcut_is_one_the_system_menu_can_show() {
        let mut keys = Vec::new();
        for title in titles(&State::default(), true) {
            keys_of(&title.entries, &mut keys);
        }
        assert!(!keys.is_empty());
        for spec in keys {
            assert!(accelerator(spec).is_some(), "{spec}");
        }
    }
}
