//! What the menus offer: one list of commands, drawn by the system's menu bar
//! on macOS and by DeniseUI's along the top of the window everywhere else.
//!
//! The list is built from [`State`] whenever it is wanted — a menu opening in
//! the window, or the system's bar being brought up to date — so what is
//! enabled and what is ticked is always what is true at that moment. Both bars
//! read the same list, and the keys a row shows are the keys that do it: the
//! window catches the ones in [`shortcut`], and the text area the edits.

use denise::{KeyCode, Modifiers};
use denise_ui::widgets::MenuItem;
use std::path::{Path, PathBuf};

/// The project's page, for Help.
pub const HOME: &str = "https://github.com/bisand/squint";

/// Where a problem is reported.
pub const ISSUES: &str = "https://github.com/bisand/squint/issues";

/// Something a menu row, or its keys, asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Command {
    New,
    Open,
    /// The recent file at this place in the list.
    OpenRecent(usize),
    ClearRecent,
    Close,
    Save,
    SaveAs,
    Revert,
    Quit,
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    SelectAll,
    Find,
    FindNext,
    FindPrevious,
    GoToLine,
    ZoomIn,
    ZoomOut,
    ActualSize,
    LineNumbers,
    Format,
    ReadOnly,
    CopyPath,
    Reveal,
    Help,
    ReportIssue,
    About,
}

/// A row the system provides and carries out itself, on macOS: hiding the
/// application, the window's own buttons, the Services submenu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum System {
    About,
    Services,
    Hide,
    HideOthers,
    ShowAll,
    Minimize,
    Zoom,
    FullScreen,
    BringAllToFront,
}

/// One row of a menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    Command {
        command: Command,
        label: String,
        /// The keys, in the portable spelling `Cmd+Shift+S`: Command on
        /// macOS, Ctrl elsewhere. Empty for none.
        keys: &'static str,
        enabled: bool,
        /// Whether the row is a setting, and if so whether it is on.
        checked: Option<bool>,
    },
    Submenu {
        label: String,
        entries: Vec<Entry>,
    },
    System(System),
    Separator,
}

impl Entry {
    fn when(mut self, on: bool) -> Self {
        if let Entry::Command { enabled, .. } = &mut self {
            *enabled = on;
        }
        self
    }

    fn ticked(mut self, on: bool) -> Self {
        if let Entry::Command { checked, .. } = &mut self {
            *checked = Some(on);
        }
        self
    }
}

fn item(command: Command, label: &str, keys: &'static str) -> Entry {
    Entry::Command {
        command,
        label: label.into(),
        keys,
        enabled: true,
        checked: None,
    }
}

/// What a menu the system treats specially is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// The menu named for the application, first on macOS.
    App,
    /// Where macOS lists the windows.
    Window,
    /// Where macOS puts its search field.
    Help,
    Plain,
}

/// A menu: its title in the bar and its rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Title {
    pub label: String,
    pub role: Role,
    pub entries: Vec<Entry>,
}

/// What the menus reflect.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct State {
    /// Whether the document has a file, which Revert, Copy Path and Reveal
    /// need.
    pub has_file: bool,
    pub modified: bool,
    /// Whether the file looks like JSON or XML.
    pub can_format: bool,
    pub read_only: bool,
    pub line_numbers: bool,
    pub recent: Vec<PathBuf>,
}

/// The menus, left to right. `system` is for the system's menu bar, which
/// gains the application and window menus and the rows the system carries
/// out, and moves Quit and About to where macOS keeps them.
pub fn titles(state: &State, system: bool) -> Vec<Title> {
    use Command::*;
    let mac = cfg!(target_os = "macos");
    let editable = !state.read_only;
    let mut titles = Vec::new();

    if system {
        titles.push(Title {
            label: "squint".into(),
            role: Role::App,
            entries: vec![
                Entry::System(System::About),
                Entry::Separator,
                Entry::System(System::Services),
                Entry::Separator,
                Entry::System(System::Hide),
                Entry::System(System::HideOthers),
                Entry::System(System::ShowAll),
                Entry::Separator,
                item(Quit, "Quit squint", "Cmd+Q"),
            ],
        });
    }

    let mut recent: Vec<Entry> = state
        .recent
        .iter()
        .enumerate()
        .map(|(i, path)| item(OpenRecent(i), &recent_label(path), ""))
        .collect();
    if !recent.is_empty() {
        recent.push(Entry::Separator);
    }
    recent.push(item(ClearRecent, "Clear Menu", "").when(!state.recent.is_empty()));

    let mut file = vec![
        item(New, "New", "Cmd+N"),
        item(Open, "Open…", "Cmd+O"),
        Entry::Submenu {
            label: "Open Recent".into(),
            entries: recent,
        },
        Entry::Separator,
        item(
            Close,
            if system { "Close" } else { "Close Window" },
            "Cmd+W",
        ),
        item(Save, "Save", "Cmd+S"),
        item(SaveAs, "Save As…", "Cmd+Shift+S"),
        item(Revert, "Revert to Saved", "").when(state.has_file && state.modified),
    ];
    if !system {
        let quit = if cfg!(windows) { "Exit" } else { "Quit" };
        file.extend([Entry::Separator, item(Quit, quit, "Cmd+Q")]);
    }
    titles.push(Title {
        label: "File".into(),
        role: Role::Plain,
        entries: file,
    });

    titles.push(Title {
        label: "Edit".into(),
        role: Role::Plain,
        entries: vec![
            item(Undo, "Undo", "Cmd+Z").when(editable),
            item(Redo, "Redo", if mac { "Cmd+Shift+Z" } else { "Cmd+Y" }).when(editable),
            Entry::Separator,
            item(Cut, "Cut", "Cmd+X").when(editable),
            item(Copy, "Copy", "Cmd+C"),
            item(Paste, "Paste", "Cmd+V").when(editable),
            item(SelectAll, "Select All", "Cmd+A"),
            Entry::Separator,
            item(Find, "Find…", "Cmd+F"),
            item(FindNext, "Find Next", if mac { "Cmd+G" } else { "F3" }),
            item(
                FindPrevious,
                "Find Previous",
                if mac { "Cmd+Shift+G" } else { "Shift+F3" },
            ),
            item(GoToLine, "Go to Line…", "Cmd+L"),
        ],
    });

    let mut view = vec![
        item(ZoomIn, "Zoom In", "Cmd+="),
        item(ZoomOut, "Zoom Out", "Cmd+-"),
        item(ActualSize, "Actual Size", "Cmd+0"),
        Entry::Separator,
        item(LineNumbers, "Line Numbers", "").ticked(state.line_numbers),
    ];
    if system {
        view.extend([Entry::Separator, Entry::System(System::FullScreen)]);
    }
    titles.push(Title {
        label: "View".into(),
        role: Role::Plain,
        entries: view,
    });

    titles.push(Title {
        label: "Tools".into(),
        role: Role::Plain,
        entries: vec![
            item(Format, "Format JSON or XML", "Cmd+Shift+F").when(state.can_format),
            item(ReadOnly, "Read Only", "").ticked(state.read_only),
            Entry::Separator,
            item(CopyPath, "Copy File Path", "").when(state.has_file),
            item(
                Reveal,
                if mac {
                    "Reveal in Finder"
                } else {
                    "Show in Folder"
                },
                "",
            )
            .when(state.has_file),
        ],
    });

    if system {
        titles.push(Title {
            label: "Window".into(),
            role: Role::Window,
            entries: vec![
                Entry::System(System::Minimize),
                Entry::System(System::Zoom),
                Entry::Separator,
                Entry::System(System::BringAllToFront),
            ],
        });
    }

    let mut help = vec![
        item(Help, "squint Help", ""),
        item(ReportIssue, "Report an Issue…", ""),
    ];
    if !system {
        help.extend([Entry::Separator, item(About, "About squint", "")]);
    }
    titles.push(Title {
        label: "Help".into(),
        role: Role::Help,
        entries: help,
    });

    titles
}

/// A recent file as its row says it: the name, then the folder it is in, with
/// the home folder written `~`.
fn recent_label(path: &Path) -> String {
    let name = path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) else {
        return name;
    };
    let dir = match dirs::home_dir().and_then(|home| dir.strip_prefix(home).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Some(rest) => format!("~/{}", rest.display()),
        None => dir.display().to_string(),
    };
    format!("{name} — {dir}")
}

/// A menu's rows as DeniseUI draws them, and the command behind each row in
/// the order DeniseUI numbers a pick: depth first, with rules and the rows
/// that open submenus counted.
pub fn rows(entries: &[Entry]) -> (Vec<MenuItem>, Vec<Option<Command>>) {
    let mut commands = Vec::new();
    let items = rows_into(entries, &mut commands);
    (items, commands)
}

fn rows_into(entries: &[Entry], commands: &mut Vec<Option<Command>>) -> Vec<MenuItem> {
    let mut items = Vec::with_capacity(entries.len());
    for entry in entries {
        match entry {
            Entry::Command {
                command,
                label,
                keys,
                enabled,
                checked,
            } => {
                commands.push(Some(*command));
                items.push(
                    MenuItem::new(label.clone())
                        .with_shortcut(keys)
                        .enabled(*enabled)
                        .checked(checked.unwrap_or(false)),
                );
            }
            Entry::Submenu { label, entries } => {
                commands.push(None);
                let children = rows_into(entries, commands);
                items.push(MenuItem::submenu(label.clone(), children));
            }
            Entry::Separator => {
                commands.push(None);
                items.push(MenuItem::separator());
            }
            // Only the system's bar has these, and it is not drawn here.
            Entry::System(_) => {}
        }
    }
    items
}

/// The command a key press is the shortcut for, among the ones the window
/// catches before the text area sees the key. The edits are the text area's
/// own keys and are not here: see [`edit_keys`].
pub fn shortcut(code: KeyCode, modifiers: Modifiers) -> Option<Command> {
    use Command::*;
    let primary = modifiers.contains(Modifiers::SUPER) || modifiers.contains(Modifiers::CTRL);
    let shift = modifiers.contains(Modifiers::SHIFT);
    Some(match code {
        KeyCode::F3 if shift => FindPrevious,
        KeyCode::F3 => FindNext,
        _ if !primary => return None,
        KeyCode::N => New,
        KeyCode::O => Open,
        KeyCode::W => Close,
        KeyCode::Q => Quit,
        KeyCode::S if shift => SaveAs,
        KeyCode::S => Save,
        KeyCode::F if shift => Format,
        KeyCode::F => Find,
        KeyCode::G if shift => FindPrevious,
        KeyCode::G => FindNext,
        KeyCode::L => GoToLine,
        KeyCode::Equal | KeyCode::NumpadAdd => ZoomIn,
        KeyCode::Minus | KeyCode::NumpadSubtract => ZoomOut,
        KeyCode::Digit0 => ActualSize,
        _ => return None,
    })
}

/// The key press the text area does an edit on, so a menu row can press it:
/// the edit then happens exactly as its keys make it happen, in the text or in
/// the field in the status line's place, whichever has the keyboard.
pub fn edit_keys(command: Command) -> Option<(KeyCode, Modifiers)> {
    let mac = cfg!(target_os = "macos");
    let primary = if mac {
        Modifiers::SUPER
    } else {
        Modifiers::CTRL
    };
    Some(match command {
        Command::Undo => (KeyCode::Z, primary),
        Command::Redo if mac => (KeyCode::Z, primary | Modifiers::SHIFT),
        Command::Redo => (KeyCode::Y, primary),
        Command::Cut => (KeyCode::X, primary),
        Command::Copy => (KeyCode::C, primary),
        Command::Paste => (KeyCode::V, primary),
        Command::SelectAll => (KeyCode::A, primary),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> State {
        State {
            has_file: true,
            recent: vec!["/tmp/a.log".into(), "/var/log/b.log".into()],
            ..State::default()
        }
    }

    /// Every row's label, depth first: the order DeniseUI numbers a pick in.
    fn flatten(items: &[MenuItem], out: &mut Vec<String>) {
        for item in items {
            out.push(item.label.clone());
            flatten(&item.items, out);
        }
    }

    fn commands_of(entries: &[Entry], out: &mut Vec<(String, Command, &'static str)>) {
        for entry in entries {
            match entry {
                Entry::Command {
                    command,
                    label,
                    keys,
                    ..
                } => out.push((label.clone(), *command, keys)),
                Entry::Submenu { entries, .. } => commands_of(entries, out),
                _ => {}
            }
        }
    }

    #[test]
    fn a_picked_row_is_the_command_written_on_it() {
        for system in [false, true] {
            for title in titles(&state(), system) {
                let (items, commands) = rows(&title.entries);
                let mut labels = Vec::new();
                flatten(&items, &mut labels);
                assert_eq!(labels.len(), commands.len(), "{}", title.label);
                let mut expected = Vec::new();
                commands_of(&title.entries, &mut expected);
                for (label, command, _) in expected {
                    let at = labels.iter().position(|l| *l == label).expect("row");
                    assert_eq!(commands[at], Some(command), "{label} in {}", title.label);
                }
            }
        }
    }

    #[test]
    fn the_system_bar_adds_the_application_and_window_menus_and_the_window_bar_does_without() {
        let system = titles(&state(), true);
        assert_eq!(system[0].role, Role::App);
        assert!(system.iter().any(|t| t.role == Role::Window));
        assert_eq!(system.last().map(|t| t.role), Some(Role::Help));

        let window = titles(&state(), false);
        let labels: Vec<&str> = window.iter().map(|t| t.label.as_str()).collect();
        assert_eq!(labels, ["File", "Edit", "View", "Tools", "Help"]);
        let mut all = Vec::new();
        for title in &window {
            commands_of(&title.entries, &mut all);
            assert!(
                !title.entries.iter().any(|e| matches!(e, Entry::System(_))),
                "nothing in the window is the system's to do"
            );
        }
        let commands: Vec<Command> = all.iter().map(|(_, c, _)| *c).collect();
        assert!(commands.contains(&Command::Quit));
        assert!(commands.contains(&Command::About));
    }

    #[test]
    fn recent_files_are_numbered_in_order_and_clearing_waits_for_some() {
        let mut all = Vec::new();
        for title in titles(&state(), false) {
            commands_of(&title.entries, &mut all);
        }
        let recent: Vec<(String, Command)> = all
            .iter()
            .filter(|(_, c, _)| matches!(c, Command::OpenRecent(_)))
            .map(|(l, c, _)| (l.clone(), *c))
            .collect();
        assert_eq!(
            recent,
            [
                ("a.log — /tmp".to_string(), Command::OpenRecent(0)),
                ("b.log — /var/log".to_string(), Command::OpenRecent(1)),
            ]
        );

        let empty = titles(&State::default(), false);
        let (items, _) = rows(&empty[0].entries);
        let submenu = items
            .iter()
            .find(|i| i.label == "Open Recent")
            .expect("row");
        assert_eq!(submenu.items.len(), 1, "only Clear Menu");
        assert!(!submenu.items[0].enabled);
    }

    /// The keys `spec` names, as the window would be told of them.
    fn press(spec: &str) -> (KeyCode, Modifiers) {
        let primary = if cfg!(target_os = "macos") {
            Modifiers::SUPER
        } else {
            Modifiers::CTRL
        };
        let mut modifiers = Modifiers::default();
        let mut code = None;
        for part in spec.split('+') {
            match part {
                "Cmd" => modifiers |= primary,
                "Shift" => modifiers |= Modifiers::SHIFT,
                key => {
                    code = Some(match key {
                        "A" => KeyCode::A,
                        "C" => KeyCode::C,
                        "F" => KeyCode::F,
                        "G" => KeyCode::G,
                        "L" => KeyCode::L,
                        "N" => KeyCode::N,
                        "O" => KeyCode::O,
                        "Q" => KeyCode::Q,
                        "S" => KeyCode::S,
                        "V" => KeyCode::V,
                        "W" => KeyCode::W,
                        "X" => KeyCode::X,
                        "Y" => KeyCode::Y,
                        "Z" => KeyCode::Z,
                        "=" => KeyCode::Equal,
                        "-" => KeyCode::Minus,
                        "0" => KeyCode::Digit0,
                        "F3" => KeyCode::F3,
                        other => panic!("no key called {other:?} in {spec:?}"),
                    })
                }
            }
        }
        (code.expect("a key"), modifiers)
    }

    #[test]
    fn the_keys_a_row_shows_do_what_the_row_does() {
        for system in [false, true] {
            let mut all = Vec::new();
            for title in titles(&state(), system) {
                commands_of(&title.entries, &mut all);
            }
            for (label, command, keys) in all.into_iter().filter(|(_, _, k)| !k.is_empty()) {
                let (code, modifiers) = press(keys);
                let caught = shortcut(code, modifiers) == Some(command);
                let typed = edit_keys(command) == Some((code, modifiers));
                assert!(
                    caught || typed,
                    "{label} shows {keys}, which does {:?}",
                    shortcut(code, modifiers)
                );
            }
        }
    }
}
