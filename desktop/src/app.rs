//! The window: the menus, a row of tabs, the text of the tab in front, and a
//! status line under it.
//!
//! Work that walks the file — building the line index, finding text — is
//! done here in slices between frames rather than on a thread: the document
//! lives inside the widget, a slice is a few milliseconds, and the window
//! keeps drawing and taking keys while the status line counts up.
//!
//! Each tab is a text area of its own, all of them in the same place and only
//! the one in front shown. A tab behind does no work: its file is counted and
//! coloured when it comes to the front. What the tabs were — their files, the
//! names and colours they were given, the lines they were on — is kept when
//! squint closes, to open again when it starts — as is the window they were
//! in, so it opens the size it was, where it was, and maximised if that is
//! how it was left; and every file is looked at every couple of seconds, in
//! case something else has changed it.
//!
//! The menus are one list of commands (see [`menu`](crate::menu)), drawn by the
//! system's menu bar on macOS and by DeniseUI's along the top of the window
//! everywhere else. A command comes the same way from either, from its keys,
//! or from the menu a right-clicked tab opens, and [`App::run`] does it.

use crate::config::Kept;
use crate::document::FileDocument;
use crate::fonts;
use crate::menu::{self, Command, State};
#[cfg(target_os = "macos")]
use crate::native_menu::NativeMenu;
use crate::recent::Recent;
use crate::session::{self, SavedTab, Session, TabColor};
use crate::settings::{self, OpenIn, Settings};
use crate::settings_window::{Link, SettingsWindow, Word};
use crate::stamp::Stamp;
use crate::switcher::{Switcher, TabId};
use denise::{
    BufferAge, Color, DamageTracker, ElementState, Frame, InputEvent, KeyCode, Modifiers, Pen,
    Point, Rect, Size,
};
use denise_text::{FontId, TextStyle};
use denise_ui::widgets::{
    ClipboardRequest, Label, MenuBar, MenuEvent, TabEvent, Tabs, TextArea, TextDocument, TextInput,
    open_menu, open_menu_at, shortcut, tab_rect,
};
use denise_ui::{Anchors, NodeId, Ui};
use denise_winit::{DeniseApp, Present, WindowConfig, WindowRequest};
use rfd::{FileDialog, MessageButtons, MessageDialog, MessageDialogResult, MessageLevel};
use squint_core::editorconfig::{self, Properties};
use squint_core::format::{self, Format, Kind, Style};
use squint_core::project::Projection;
use squint_core::source::FileSource;
use squint_core::{Document, Find, FindStep};
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io::{self, BufWriter};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

/// What the desktop knows squint's windows by: Wayland's app_id and X11's
/// class, and the name of the desktop entry, so the two are matched.
pub const APP_ID: &str = "squint";

/// Bytes indexed per step: a few milliseconds from the page cache.
const INDEX_SLICE: usize = 8 * 1024 * 1024;

/// Bytes searched per step.
const FIND_SLICE: usize = 4 * 1024 * 1024;

/// Bytes formatted per step, between frames.
const FORMAT_SLICE: usize = 4 * 1024 * 1024;

/// Bytes formatted per step on a thread of its own, which has no frame to be
/// out of the way of: only often enough to notice it has been stopped.
const FORMAT_THREAD_SLICE: usize = 16 * 1024 * 1024;

/// How long one frame may spend walking the file, indexing or finding.
const SLICE_TIME: Duration = Duration::from_millis(6);

/// How often the editor looks at what the settings window has said, while
/// there is one open.
const HEAR_SETTINGS: Duration = Duration::from_millis(50);

/// How often the editor looks for files another squint has handed it. Soon
/// enough after a double-click, and rare enough to cost nothing idle.
const HEAR_HANDOFF: Duration = Duration::from_millis(250);

/// The sizes Zoom In and Zoom Out step through. Zoom starts from the size the
/// settings give, and ⌘0 comes back to it.
const TEXT_SIZES: &[u16] = &[8, 9, 10, 11, 12, 13, 14, 16, 18, 20, 24, 28, 32, 40, 48];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Msg {
    Changed,
    Clipboard(ClipboardRequest),
    /// Enter in the prompt's field.
    Submit,
    /// A title of the menu bar in the window was pressed.
    MenuTitle(usize),
    /// What a menu open from that bar, or from a tab, did.
    Menu(MenuEvent),
    /// What was done to the row of tabs.
    Tab(TabEvent),
    /// Enter in the field renaming a tab.
    Renamed,
}

/// Where the menus are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Menus {
    /// In the system's menu bar: macOS.
    System,
    /// DeniseUI's, along the top of the window.
    Window,
    /// None: a snapshot of a window whose menus the system draws.
    Off,
}

impl Menus {
    /// The platform's own: the system's bar on macOS, the window's everywhere
    /// else. `SQUINT_MENU=window` puts them in the window on macOS too.
    pub fn for_platform() -> Self {
        if cfg!(target_os = "macos") && std::env::var("SQUINT_MENU").as_deref() != Ok("window") {
            Menus::System
        } else {
            Menus::Window
        }
    }
}

/// What the window remembers between runs.
pub struct Remembered {
    pub recent: Recent,
    pub settings: Kept<Settings>,
    pub session: Kept<Session>,
}

impl Remembered {
    /// What the last run left.
    pub fn load() -> Self {
        Self {
            recent: Recent::load(),
            settings: Kept::load(settings::FILE),
            session: Kept::load(session::FILE),
        }
    }

    /// The window as the last run left it.
    ///
    /// Read on its own, and before [`load`](Self::load): the window has to be
    /// asked for at the size it should open at, and that is a moment before
    /// there is an application inside it to ask. One small file, read twice.
    pub fn window() -> Option<session::Window> {
        Kept::<Session>::load(session::FILE).get().window
    }

    /// Nothing remembered, and nothing written: for a snapshot.
    pub fn in_memory() -> Self {
        Self {
            recent: Recent::in_memory(),
            settings: Kept::in_memory(Settings::default()),
            session: Kept::in_memory(Session::default()),
        }
    }
}

/// What the field in the status line's place is asking for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ask {
    GoTo,
    Find,
}

/// A field in the status line's place, and a note beside it that says what
/// the last answer came to. The status line comes back when it closes.
struct Prompt {
    ask: Ask,
    field: NodeId,
    note: NodeId,
}

/// A find on its way through a file.
struct Search {
    /// The text area it is finding in, which need not stay in front.
    editor: NodeId,
    find: Find,
    query: String,
    /// Where it matched and whether it wrapped, once it has, while the index
    /// catches up far enough to say which line that is.
    hit: Option<(u64, bool)>,
}

/// A format on its way into the tab it came from.
///
/// When it is finished the tab takes the formatted document up as what it
/// holds, keeping its name and its file: what the tab shows is the same file,
/// formatted, with unsaved changes in it.
struct Formatting {
    /// The text area whose document is being formatted.
    editor: NodeId,
    kind: Kind,
    /// The layout used and where it came from, for the status line.
    note: String,
    /// The source's length, for the percentage.
    len: u64,
    work: Work,
}

/// How a format is being run.
enum Work {
    /// Projected on a thread of its own: the document is its file and nothing
    /// else, so the thread opens that file again and formats it into marks
    /// and a line index without writing a byte anywhere. What the tab takes
    /// up reads the original file and lays it out where it is looked at.
    Project {
        /// Bytes of the source formatted so far.
        done: Arc<AtomicU64>,
        /// Asks the thread to give up.
        stop: Arc<AtomicBool>,
        result: Receiver<io::Result<Projection>>,
    },
    /// Written to a copy beside the file, in slices between frames: the
    /// document has edits in it that only this process's piece table knows
    /// about, so the bytes have to come from it and there is no file to
    /// project from. Saving renames the copy into place.
    Slices {
        job: Format,
        writer: BufWriter<File>,
        part: PathBuf,
    },
}

impl Formatting {
    /// Bytes of the source formatted so far.
    fn done(&self) -> u64 {
        match &self.work {
            Work::Project { done, .. } => done.load(Ordering::Relaxed),
            Work::Slices { job, .. } => job.progress().0,
        }
    }

    /// Gives up: the thread is told to stop, and a half-written copy goes.
    fn discard(self) {
        match &self.work {
            Work::Project { stop, .. } => stop.store(true, Ordering::Relaxed),
            Work::Slices { part, .. } => {
                let _ = fs::remove_file(part);
            }
        }
    }
}

/// A menu open from the bar in the window, or from a tab.
struct OpenMenu {
    /// The command behind each row, numbered as a pick is.
    commands: Vec<Option<Command>>,
    /// What had the keyboard before the menu took it, to be given it back.
    focus: Option<NodeId>,
}

/// One tab: a file, or none yet, in a text area of its own.
struct Tab {
    id: TabId,
    editor: NodeId,
    /// The file, while it has one.
    path: Option<PathBuf>,
    /// The name it was given; the file's name while it has none.
    name: Option<String>,
    color: Option<TabColor>,
    /// What the file looks like it could be formatted as, decided when it
    /// opens.
    format_kind: Option<Kind>,
    read_only: bool,
    /// The file as it was when squint last read or wrote it.
    stamp: Option<Stamp>,
    /// Asked to stop being told about changes to this file, until it is read
    /// again.
    ignore_changes: bool,
    /// A line to go to once the index has counted that far: where a reopened
    /// or reloaded tab was.
    pending_line: Option<usize>,
}

/// A field over a tab, renaming it.
struct Rename {
    tab: TabId,
    field: NodeId,
}

/// What to do about a file something else changed.
enum Change {
    Reload,
    Keep,
    Ignore,
}

pub struct App {
    ui: Ui<Msg>,
    tabs: Vec<Tab>,
    /// Where the tab in front is in `tabs`.
    active: usize,
    /// The row of tabs.
    strip: NodeId,
    status: NodeId,
    /// DeniseUI's menu bar, when the menus are in the window.
    bar: Option<NodeId>,
    open_menu: Option<OpenMenu>,
    #[cfg(target_os = "macos")]
    native: Option<NativeMenu>,
    /// What the system's menus were last brought up to date with.
    #[cfg(target_os = "macos")]
    shown: State,
    memory: Remembered,
    /// What the settings window is saying, while there is one open.
    settings: Option<Arc<Link>>,
    /// Windows asked for and not yet opened: the runner takes them.
    windows: Vec<WindowRequest>,
    /// How this window is drawn, so one opened beside it is drawn the same
    /// way.
    present: Present,
    /// Where the settings are kept, for the settings window to say and for
    /// Edit the File… to open.
    settings_file: Option<PathBuf>,
    /// The faces added to the tree, by the file they came from: a face asked
    /// for twice is added once.
    faces: HashMap<String, FontId>,
    /// How often the tabs' files are looked at, from the settings.
    watch: Duration,
    switcher: Switcher,
    next_id: TabId,
    rename: Option<Rename>,
    /// Where a tab's text starts, below the menu bar and the tabs.
    page_top: i32,
    status_h: i32,
    prompt: Option<Prompt>,
    search: Option<Search>,
    formatting: Option<Formatting>,
    /// The last thing searched for, so ⌘G and F3 have something to find with
    /// the field closed, and the field opens holding it.
    last_query: String,
    /// The text area being marked with the query in the find field, and the
    /// query, so it is only reached — and so repainted — when either changes.
    highlighted: Option<(NodeId, String)>,
    /// The face the menus, tabs and status line are drawn in.
    chrome: TextStyle,
    /// The same face, a size down, for the tabs and their rename field.
    small: TextStyle,
    /// The face the text is drawn in, and its size in logical pixels.
    mono: TextStyle,
    text_size: u16,
    /// How tall the menu bar and the row of tabs are, so the text below them
    /// can be put back when a face changes.
    bar_h: i32,
    strip_h: i32,
    gutter: bool,
    scale: f32,
    /// The window as it should open next time: its size and corner while it is
    /// not maximised, and whether it was left that way.
    place: session::Window,
    /// The window's inner size in logical pixels as the surface last reported
    /// it — which while the window is maximised is the screen's, and not a
    /// size worth opening at.
    last_size: (u32, u32),
    title: String,
    /// What the status line says on the right: the last thing that happened.
    notice: String,
    clipboard: Option<arboard::Clipboard>,
    started: Instant,
    last_check: Instant,
    exit: bool,
}

impl App {
    /// How the window is asked for: as the last run left it, when there is a
    /// last run to go by, and 1000×700 wherever the system likes when there is
    /// not.
    ///
    /// A position the window system cannot honour — a display since unplugged —
    /// is dropped by the backend rather than here: it is the one that knows
    /// what is plugged in.
    pub fn config(present: Present, window: Option<session::Window>) -> WindowConfig {
        let window = window.and_then(session::Window::sane);
        WindowConfig {
            title: "squint".into(),
            size: window.map_or(Size::new(1000, 700), |w| Size::new(w.width, w.height)),
            position: window.and_then(|w| w.at).map(|at| Point::new(at.x, at.y)),
            maximized: window.is_some_and(|w| w.maximized),
            present,
            app_id: Some(APP_ID.into()),
            ..WindowConfig::default()
        }
    }

    pub fn new(
        size: Size,
        scale: f32,
        path: Option<&Path>,
        menus: Menus,
        memory: Remembered,
        present: Present,
    ) -> Self {
        let px = |v: f32| (v * scale + 0.5) as u16;
        let s = |v: i32| (v as f32 * scale + 0.5) as i32;
        let settings = memory.settings.get().clone();
        let mut ui: Ui<Msg> = Ui::new(size, settings.theme().scaled(scale));
        ui.show_cursor(false);
        let mut faces = HashMap::new();
        let ui_px = px(settings.appearance.ui_size as f32);
        let chrome = match face(
            &mut ui,
            &mut faces,
            settings.appearance.ui_font.as_deref(),
            fonts::UI,
        )
        .0
        {
            Some(font) => {
                ui.set_default_font(font);
                TextStyle {
                    font,
                    size_px: ui_px,
                }
            }
            None => TextStyle::built_in(ui_px),
        };
        let small = TextStyle {
            size_px: px(settings.appearance.ui_size as f32 - 1.0),
            ..chrome
        };
        let text_size = settings.appearance.text_size;
        let mono = match face(
            &mut ui,
            &mut faces,
            settings.appearance.text_font.as_deref(),
            fonts::MONO,
        )
        .0
        {
            Some(font) => TextStyle {
                font,
                size_px: px(text_size as f32),
            },
            None => TextStyle::built_in(px(text_size as f32)),
        };

        let root = ui.root();
        let (w, h) = (size.width as i32, size.height as i32);
        let status_h = status_height(settings.appearance.ui_size, scale);
        let bar = (menus == Menus::Window).then(|| {
            let labels: Vec<String> = menu::titles(&State::default(), false)
                .into_iter()
                .map(|t| t.label)
                .collect();
            let widget = MenuBar::new(labels, Msg::MenuTitle).with_style(chrome);
            let theme = *ui.theme();
            let height = widget.preferred_height(&theme, ui.text_mut());
            let id = ui
                .add(root, widget, Rect::new(0, 0, w, height))
                .expect("menu bar");
            ui.set_anchors(id, TOP_ROW);
            (id, height)
        });
        let bar_h = bar.map_or(0, |(_, height)| height);
        let strip_widget = Tabs::with_events(Vec::<String>::new(), Msg::Tab)
            .with_close_buttons(true)
            .with_style(small);
        let strip_h = strip_widget.strip_height(ui.theme());
        let strip = ui
            .add(root, strip_widget, Rect::new(0, bar_h, w, strip_h))
            .expect("tabs");
        ui.set_anchors(strip, TOP_ROW);
        let status = ui
            .add(
                root,
                Label::new("").with_style(TextStyle {
                    size_px: px(settings.appearance.ui_size as f32 - 2.0),
                    ..chrome
                }),
                Rect::new(s(8), h - status_h, w - s(16), status_h),
            )
            .expect("status");
        ui.set_anchors(status, BOTTOM_ROW);

        let memory_file = memory.settings.file().map(Path::to_path_buf);
        // What was asked for, until the window system says what it actually
        // gave: a surface half the size of a saved window means the window is
        // that size, and the corner arrives with the first `SurfaceMoved`.
        let last_size = (logical(size.width, scale), logical(size.height, scale));
        let place = memory
            .session
            .get()
            .window
            .and_then(session::Window::sane)
            .unwrap_or(session::Window {
                width: last_size.0,
                height: last_size.1,
                at: None,
                maximized: false,
            });
        let mut app = Self {
            ui,
            tabs: Vec::new(),
            active: 0,
            strip,
            status,
            bar: bar.map(|(id, _)| id),
            open_menu: None,
            #[cfg(target_os = "macos")]
            native: None,
            #[cfg(target_os = "macos")]
            shown: State::default(),
            memory,
            settings: None,
            windows: Vec::new(),
            present,
            settings_file: memory_file,
            faces,
            watch: Duration::from_secs(settings.general.watch_seconds as u64),
            switcher: Switcher::default(),
            next_id: 0,
            rename: None,
            page_top: bar_h + strip_h,
            status_h,
            prompt: None,
            search: None,
            formatting: None,
            last_query: String::new(),
            highlighted: None,
            chrome,
            small,
            mono,
            text_size,
            bar_h,
            strip_h,
            gutter: settings.editor.line_numbers,
            scale,
            place,
            last_size,
            title: "squint".into(),
            notice: String::new(),
            clipboard: arboard::Clipboard::new().ok(),
            started: Instant::now(),
            last_check: Instant::now(),
            exit: false,
        };
        let front = app.restore(path);
        app.show_tab(front, true);
        // A face the settings name and this machine has not is worth saying at
        // launch too, over whatever the tabs had to say for themselves.
        if let Some(complaint) = app.apply_settings() {
            app.notice = complaint;
        }
        // The system's window tabs would take Ctrl+Tab from squint's own.
        #[cfg(target_os = "macos")]
        if menus != Menus::Off {
            crate::native_menu::forbid_window_tabs();
        }
        #[cfg(target_os = "macos")]
        if menus == Menus::System {
            let state = app.menu_state();
            app.native = Some(NativeMenu::new(&menu::titles(&state, true)));
            app.shown = state;
        }
        app.refresh_status();
        app
    }

    /// Opens the tabs the last run left, when the settings say to, and the
    /// file named when squint was started. Returns the tab to put in front.
    fn restore(&mut self, path: Option<&Path>) -> usize {
        let session = self.memory.session.get().clone();
        let at_line = self.memory.settings.get().general.reopen_at_line;
        let mut front = None;
        if self.memory.settings.get().general.reopen_tabs {
            for (i, saved) in session.tabs.iter().enumerate() {
                // A file that has gone since is left out, quietly: it was
                // somebody's decision to remove it.
                let Ok(doc) = FileDocument::open(&saved.path) else {
                    continue;
                };
                let index = self.add_tab(doc);
                let tab = &mut self.tabs[index];
                tab.name = saved.name.clone();
                tab.color = saved.color;
                tab.pending_line = (at_line && saved.line > 0).then_some(saved.line);
                if i == session.active {
                    front = Some(index);
                }
            }
        }
        if let Some(path) = path {
            match self.tab_with(path) {
                Some(index) => front = Some(index),
                None => match FileDocument::open(path) {
                    Ok(doc) => {
                        front = Some(self.add_tab(doc));
                        self.memory.recent.add(path);
                    }
                    Err(e) => self.notice = format!("{}: {e}", path.display()),
                },
            }
        }
        if self.tabs.is_empty() {
            self.add_tab(FileDocument::empty());
            if self.notice.is_empty() {
                self.notice = format!("no file: {} opens one", shortcut("Cmd+O"));
            }
        }
        front.unwrap_or(0)
    }

    /// Indexes the whole file now. For a snapshot, which has no frames to
    /// spread it over.
    pub fn index_all(&mut self) {
        while !self.editor_ref().document().is_indexed() {
            self.pump_index();
        }
        self.settle_pending_line();
    }

    /// Draws the window into `frame`, without an event loop.
    pub fn paint_into(&mut self, frame: &mut Frame<'_>) {
        self.ui.paint(frame);
    }

    /// Jumps to 1-based `line`, as the go-to-line field would.
    pub fn go_to_line(&mut self, line: usize) {
        self.editor().go_to(line.saturating_sub(1));
        self.say(format!("line {line}"));
    }

    /// Opens the find field holding `query`, finds forwards from the caret
    /// and waits for the answer. For a snapshot, which has no frames to
    /// spread the search over; the file must already be indexed.
    pub fn find_now(&mut self, query: &str) {
        self.open_prompt(Ask::Find);
        if let Some(field) = self.prompt.as_ref().map(|p| p.field)
            && let Some(input) = self.ui.widget_mut::<TextInput<Msg>>(field)
        {
            input.set_text(query);
        }
        self.sync_highlight();
        self.find(true);
        while self.search.is_some() {
            self.pump_find();
        }
    }

    /// Formats the file as ⇧⌘F does and waits for the tab to take it up. For
    /// a snapshot, which has no frames to spread the work over.
    pub fn format_now(&mut self) {
        self.start_format();
        while self.formatting.is_some() {
            self.pump_format();
            // A format on a thread is only looked in on here, so there is
            // nothing to do between looks but wait for it.
            if self.formatting.is_some() {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        self.settle_swap();
    }

    /// Loads the grammars, decides on one and parses the file from the top,
    /// waiting for all of it. For a snapshot, which has no frames to spread
    /// the work over; the file must already be indexed.
    pub fn highlight_now(&mut self) {
        let head = self.editor_ref().document().head(8192);
        let path = self.tabs[self.active].path.clone();
        if squint_core::syntax::worth_loading(path.as_deref(), &head) {
            squint_core::syntax::load();
        }
        self.editor().document_mut().decide_syntax();
        while self.editor_ref().document().highlighting() {
            self.pump_syntax();
        }
        self.refresh_status();
    }

    /// Opens the menu titled `label` from the bar in the window, as a press
    /// on its title would. For a snapshot. Whether there was one to open.
    pub fn open_menu_titled(&mut self, label: &str) -> bool {
        let index = self
            .bar
            .and_then(|bar| self.ui.widget::<MenuBar<Msg>>(bar))
            .and_then(|bar| {
                bar.titles()
                    .iter()
                    .position(|t| t.eq_ignore_ascii_case(label))
            });
        let Some(index) = index else {
            return false;
        };
        self.open_title(index);
        self.open_menu.is_some()
    }

    // ---- tabs ---------------------------------------------------------------

    fn area(&self, editor: NodeId) -> Option<&TextArea<Msg, FileDocument>> {
        self.ui.widget::<TextArea<Msg, FileDocument>>(editor)
    }

    fn area_mut(&mut self, editor: NodeId) -> Option<&mut TextArea<Msg, FileDocument>> {
        self.ui.widget_mut::<TextArea<Msg, FileDocument>>(editor)
    }

    /// The text area of the tab in front.
    fn editor_id(&self) -> NodeId {
        self.tabs[self.active].editor
    }

    fn editor(&mut self) -> &mut TextArea<Msg, FileDocument> {
        let editor = self.editor_id();
        self.area_mut(editor).expect("editor")
    }

    fn editor_ref(&self) -> &TextArea<Msg, FileDocument> {
        self.area(self.editor_id()).expect("editor")
    }

    fn index_of(&self, id: TabId) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
    }

    /// The tab that has `path` open.
    fn tab_with(&self, path: &Path) -> Option<usize> {
        self.tabs
            .iter()
            .position(|t| t.path.as_deref() == Some(path))
    }

    fn is_modified(&self, index: usize) -> bool {
        self.tabs
            .get(index)
            .and_then(|t| self.area(t.editor))
            .is_some_and(|area| area.document().is_modified())
    }

    /// An untitled tab nobody has typed in: what a file opened replaces.
    fn is_blank(&self, index: usize) -> bool {
        self.tabs.get(index).is_some_and(|t| t.path.is_none()) && !self.is_modified(index)
    }

    /// What a tab is called: the name it was given, or its file's.
    fn tab_name(&self, index: usize) -> String {
        let Some(tab) = self.tabs.get(index) else {
            return String::new();
        };
        tab.name.clone().unwrap_or_else(|| {
            tab.path
                .as_deref()
                .map_or_else(|| "Untitled".to_string(), name_of)
        })
    }

    /// What a tab says: its name, marked while it has unsaved changes.
    fn tab_label(&self, index: usize) -> String {
        let mut label = self.tab_name(index);
        if self.is_modified(index) {
            label.push_str(" •");
        }
        label
    }

    /// Where a tab's text goes, in the window as it is now.
    fn page_rect(&self) -> Rect {
        let size = self.ui.size();
        let (w, h) = (size.width as i32, size.height as i32);
        Rect::new(
            0,
            self.page_top,
            w,
            (h - self.page_top - self.status_h).max(0),
        )
    }

    fn editor_style(&self) -> TextStyle {
        TextStyle {
            size_px: (self.text_size as f32 * self.scale + 0.5) as u16,
            ..self.mono
        }
    }

    /// Adds a tab for `doc` at the end of the row, behind the others. Returns
    /// where it is.
    fn add_tab(&mut self, mut doc: FileDocument) -> usize {
        let path = doc.path().map(Path::to_path_buf);
        let format_kind = format::detect(path.as_deref(), &doc.head(8192));
        let editor = &self.memory.settings.get().editor;
        let (read_only, smooth_scroll) = (editor.read_only, editor.smooth_scroll);
        doc.allow_syntax(self.syntax_wanted(&doc));
        let area = TextArea::new(doc)
            .with_style(self.editor_style())
            .with_tab_width(self.tab_width_for(path.as_deref()))
            .with_gutter(self.gutter)
            .with_read_only(read_only)
            .with_smooth_scroll(smooth_scroll)
            .with_change(Msg::Changed)
            .with_clipboard(Msg::Clipboard);
        let root = self.ui.root();
        let page = self.page_rect();
        let editor = self.ui.add(root, area, page).expect("editor");
        self.ui.set_anchors(editor, ALL_SIDES);
        self.ui.set_visible(editor, false);
        let id = self.next_id;
        self.next_id += 1;
        self.tabs.push(Tab {
            id,
            editor,
            stamp: path.as_deref().and_then(Stamp::of),
            path,
            name: None,
            color: None,
            format_kind,
            read_only,
            ignore_changes: false,
            pending_line: None,
        });
        self.tabs.len() - 1
    }

    /// Brings tab `index` to the front. `remember` makes it the last tab for
    /// Ctrl+Tab, which a walk along the row with Ctrl held does not.
    fn show_tab(&mut self, index: usize, remember: bool) {
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        let (id, editor) = (tab.id, tab.editor);
        if let Some(old) = self.tabs.get(self.active).map(|t| t.editor)
            && old != editor
        {
            self.ui.set_visible(old, false);
        }
        self.ui.set_visible(editor, true);
        self.active = index;
        if remember {
            self.switcher.brought_forward(id);
        }
        if self.prompt.is_none() && self.rename.is_none() && self.open_menu.is_none() {
            self.ui.focus(Some(editor));
        }
        self.sync_strip();
        self.refresh_status();
    }

    /// The next tab along the row, or the one before, wrapping.
    fn step_tab(&mut self, forward: bool) {
        let n = self.tabs.len();
        if n < 2 {
            return;
        }
        let next = if forward {
            (self.active + 1) % n
        } else {
            (self.active + n - 1) % n
        };
        self.show_tab(next, true);
    }

    /// Tab with Ctrl held. See [`Switcher`].
    fn cycle_tab(&mut self, backwards: bool) {
        let row: Vec<TabId> = self.tabs.iter().map(|t| t.id).collect();
        let front = self.tabs[self.active].id;
        if let Some(to) = self.switcher.tab_pressed(&row, front, backwards)
            && let Some(index) = self.index_of(to)
        {
            self.show_tab(index, false);
        }
    }

    fn ctrl_released(&mut self) {
        let front = self.tabs[self.active].id;
        self.switcher.released(front);
    }

    /// Takes tab `index` out, without asking anything, and brings forward the
    /// tab that was in front before it if it was in front.
    fn remove_tab(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        let front = index == self.active;
        let tab = self.tabs.remove(index);
        self.forget_editor(tab.editor);
        self.ui.remove(tab.editor);
        self.switcher.closed(tab.id);
        if let Some(rename) = &self.rename
            && rename.tab == tab.id
        {
            let field = rename.field;
            self.rename = None;
            self.ui.remove(field);
        }
        if self.tabs.is_empty() {
            self.active = 0;
            return;
        }
        if index < self.active {
            self.active -= 1;
        }
        if front {
            let row: Vec<TabId> = self.tabs.iter().map(|t| t.id).collect();
            let next = self
                .switcher
                .last_in_front(&row)
                .and_then(|id| self.index_of(id))
                .unwrap_or(index.min(self.tabs.len() - 1));
            self.active = next;
            self.show_tab(next, true);
        }
    }

    /// Stops whatever was working on `editor`, which is going away or getting
    /// a different document.
    fn forget_editor(&mut self, editor: NodeId) {
        if self.search.as_ref().is_some_and(|s| s.editor == editor) {
            self.search = None;
        }
        if self.formatting.as_ref().is_some_and(|f| f.editor == editor) {
            self.cancel_format("its tab changed");
        }
        if self
            .highlighted
            .as_ref()
            .is_some_and(|(node, _)| *node == editor)
        {
            self.highlighted = None;
        }
    }

    /// Closes tab `index`, asking first about unsaved changes. The last tab
    /// closes the window when there is nothing in it, and otherwise leaves an
    /// empty one.
    fn close_tab(&mut self, index: usize) {
        if !self.may_discard_tab(index) {
            return;
        }
        if self.tabs.len() == 1 && self.is_blank(0) {
            self.exit = true;
            return;
        }
        self.remove_tab(index);
        if self.tabs.is_empty() {
            let blank = self.add_tab(FileDocument::empty());
            self.show_tab(blank, true);
        }
        self.sync_strip();
        self.remember_tabs();
    }

    fn close_other_tabs(&mut self) {
        let keep = self.tabs[self.active].id;
        let others: Vec<TabId> = self
            .tabs
            .iter()
            .map(|t| t.id)
            .filter(|&id| id != keep)
            .collect();
        for id in others {
            let Some(index) = self.index_of(id) else {
                continue;
            };
            if !self.may_discard_tab(index) {
                break;
            }
            if let Some(index) = self.index_of(id) {
                self.remove_tab(index);
            }
        }
        if let Some(index) = self.index_of(keep) {
            self.show_tab(index, true);
        }
        self.sync_strip();
        self.remember_tabs();
    }

    /// Brings the row of tabs up to date: names, colours and which is in
    /// front. Reaching the strip repaints it, so it is only reached when
    /// something differs.
    fn sync_strip(&mut self) {
        let labels: Vec<String> = (0..self.tabs.len()).map(|i| self.tab_label(i)).collect();
        let colors: Vec<Option<Color>> = self
            .tabs
            .iter()
            .map(|t| t.color.map(TabColor::color))
            .collect();
        let active = self.active;
        let same = self
            .ui
            .widget::<Tabs<Msg>>(self.strip)
            .is_some_and(|strip| {
                strip.labels() == labels.as_slice()
                    && strip.colors() == colors.as_slice()
                    && strip.selected() == active
            });
        if same {
            return;
        }
        if let Some(strip) = self.ui.widget_mut::<Tabs<Msg>>(self.strip) {
            if strip.labels() != labels.as_slice() {
                strip.set_labels(labels);
            }
            if strip.colors() != colors.as_slice() {
                strip.set_colors(colors);
            }
            strip.set_selected(active);
        }
    }

    /// Takes in what the window system says the window is doing now.
    ///
    /// Only the corner and the maximised flag land straight away; the size
    /// waits for [`settle_window`](Self::settle_window), because maximising
    /// arrives as a resize and a move together and the resize is the one that
    /// comes first.
    fn note_place(&mut self, position: Point, maximized: bool) {
        if !maximized {
            self.place.at = Some(session::Spot {
                x: position.x,
                y: position.y,
            });
        }
        self.place.maximized = maximized;
    }

    /// Settles what the window should open at, once a frame's events have all
    /// been seen.
    ///
    /// A maximised window is the screen's size, not a size to open at: what is
    /// kept while it is maximised is the size and corner it had before, which
    /// are what it goes back to when it is un-maximised anyway.
    fn settle_window(&mut self) {
        if !self.place.maximized {
            (self.place.width, self.place.height) = self.last_size;
        }
    }

    /// Writes down the tabs with files and the window they are in, for the
    /// next run.
    fn remember_tabs(&mut self) {
        let mut session = Session::default();
        for (index, tab) in self.tabs.iter().enumerate() {
            let Some(path) = tab.path.clone() else {
                continue;
            };
            if index == self.active {
                session.active = session.tabs.len();
            }
            let line = tab
                .pending_line
                .or_else(|| self.area(tab.editor).map(|area| area.caret().line))
                .unwrap_or(0);
            session.tabs.push(SavedTab {
                path,
                name: tab.name.clone(),
                color: tab.color,
                line,
            });
        }
        session.window = Some(self.place);
        if session != *self.memory.session.get() {
            self.memory.session.set(session);
        }
    }

    fn tab_event(&mut self, event: TabEvent) {
        match event {
            TabEvent::Selected(index) => self.show_tab(index, true),
            TabEvent::Close(index) => self.close_tab(index),
            TabEvent::Moved { from, to } => {
                // The strip has moved its own tab already; the tabs here follow.
                if from < self.tabs.len() && to < self.tabs.len() {
                    let front = self.tabs[self.active].id;
                    let tab = self.tabs.remove(from);
                    self.tabs.insert(to, tab);
                    self.active = self.index_of(front).unwrap_or(0);
                    self.remember_tabs();
                }
            }
            TabEvent::Activated(index) => self.start_rename(index),
            TabEvent::Menu { index, at } => {
                self.show_tab(index, true);
                self.open_tab_menu(at);
            }
        }
    }

    /// The menu a right-clicked tab opens, at the pointer.
    fn open_tab_menu(&mut self, at: Point) {
        self.close_menu();
        let entries = menu::tab_entries(&self.menu_state());
        let (items, commands) = menu::rows(&entries);
        let focus = self.ui.focused();
        let style = TextStyle {
            size_px: (13.0 * self.scale + 0.5) as u16,
            ..self.small
        };
        if open_menu_at(
            &mut self.ui,
            self.strip,
            Rect::new(at.x, at.y, 1, 1),
            &items,
            style,
            Msg::Menu,
        )
        .is_some()
        {
            self.open_menu = Some(OpenMenu { commands, focus });
        }
    }

    /// Puts a field over tab `index`, holding its name.
    fn start_rename(&mut self, index: usize) {
        self.finish_rename(true);
        if index >= self.tabs.len() {
            return;
        }
        self.show_tab(index, true);
        let Some(tab) = tab_rect(&mut self.ui, self.strip, index) else {
            return;
        };
        let s = |v: i32| (v as f32 * self.scale + 0.5) as i32;
        let mut input = TextInput::<Msg>::new()
            .with_submit(Msg::Renamed)
            .with_max_chars(120)
            .with_style(self.small);
        input.set_text(self.tab_name(index));
        let rect = Rect::new(
            tab.x,
            tab.y + s(2),
            tab.width.max(s(160)),
            (tab.height - s(4)).max(s(16)),
        );
        let root = self.ui.root();
        let Some(field) = self.ui.add(root, input, rect) else {
            return;
        };
        self.ui.focus(Some(field));
        self.rename = Some(Rename {
            tab: self.tabs[index].id,
            field,
        });
    }

    /// Takes the rename field away, giving the tab what was typed when `keep`.
    /// Nothing typed, or the file's own name, gives it back its file's name.
    fn finish_rename(&mut self, keep: bool) {
        let Some(rename) = self.rename.take() else {
            return;
        };
        let text = self
            .ui
            .widget::<TextInput<Msg>>(rename.field)
            .map(|field| field.text().trim().to_string())
            .unwrap_or_default();
        self.ui.remove(rename.field);
        if keep && let Some(index) = self.index_of(rename.tab) {
            let file = self.tabs[index]
                .path
                .as_deref()
                .map_or_else(|| "Untitled".to_string(), name_of);
            self.tabs[index].name = (!text.is_empty() && text != file).then_some(text);
            self.sync_strip();
            self.remember_tabs();
        }
        if self.prompt.is_none() {
            let editor = self.editor_id();
            self.ui.focus(Some(editor));
        }
    }

    /// A click anywhere else ends a rename, keeping what was typed.
    fn settle_rename(&mut self) {
        if let Some(rename) = &self.rename
            && self.ui.focused() != Some(rename.field)
        {
            self.finish_rename(true);
        }
    }

    /// Goes to the line the tab in front is waiting for, once the index has
    /// counted that far.
    fn settle_pending_line(&mut self) {
        let Some(line) = self.tabs.get(self.active).and_then(|t| t.pending_line) else {
            return;
        };
        let reachable = {
            let doc = self.editor().document_mut();
            doc.is_indexed() || doc.known_lines() > line
        };
        if reachable {
            self.tabs[self.active].pending_line = None;
            self.editor().go_to(line);
        }
    }

    /// Indexes for a few milliseconds, if there is indexing left to do.
    fn pump_index(&mut self) {
        if self.editor_ref().document().is_indexed() {
            return;
        }
        let deadline = Instant::now() + SLICE_TIME;
        let doc = self.editor().document_mut();
        while !doc.index_step(INDEX_SLICE) && Instant::now() < deadline {}
        self.refresh_status();
    }

    /// Decides on a grammar once the grammars have loaded, and parses on from
    /// the top for a few milliseconds while there is parsing left. Reaching
    /// the document repaints the editor, which is what puts guessed colours
    /// right as the parse passes them.
    fn pump_syntax(&mut self) {
        let (undecided, busy, indexed) = {
            let doc = self.editor_ref().document();
            (doc.syntax_undecided(), doc.highlighting(), doc.is_indexed())
        };
        if undecided && self.editor().document_mut().decide_syntax() {
            self.refresh_status();
        }
        if busy && indexed {
            let deadline = Instant::now() + SLICE_TIME;
            self.editor().document_mut().highlight_until(deadline);
        }
    }

    // ---- the menus ----------------------------------------------------------

    /// What the menus should show now.
    fn menu_state(&self) -> State {
        let tab = &self.tabs[self.active];
        State {
            has_file: tab.path.is_some(),
            modified: self.is_modified(self.active),
            can_format: tab.format_kind.is_some(),
            read_only: tab.read_only,
            line_numbers: self.gutter,
            recent: self.memory.recent.paths().to_vec(),
            tabs: self.tabs.len(),
            tab_color: tab.color,
        }
    }

    /// Opens menu `index` of the bar in the window, closing any other.
    fn open_title(&mut self, index: usize) {
        let Some(bar) = self.bar else {
            return;
        };
        let focus = match self.open_menu.take() {
            Some(open) => {
                self.ui.close_popup();
                open.focus
            }
            None => self.ui.focused(),
        };
        let titles = menu::titles(&self.menu_state(), false);
        let Some(title) = titles.get(index) else {
            return;
        };
        let (items, commands) = menu::rows(&title.entries);
        if open_menu(&mut self.ui, bar, index, &items, Msg::Menu).is_some() {
            self.open_menu = Some(OpenMenu { commands, focus });
            self.light_title(Some(index));
        } else {
            self.ui.focus(focus);
            self.light_title(None);
        }
    }

    fn menu_event(&mut self, event: MenuEvent) {
        match event {
            MenuEvent::Title(index) => self.open_title(index),
            MenuEvent::Dismissed => self.close_menu(),
            MenuEvent::Picked(row) => {
                let command = self
                    .open_menu
                    .as_ref()
                    .and_then(|open| open.commands.get(row).copied().flatten());
                self.close_menu();
                if let Some(command) = command {
                    self.run(command);
                }
            }
        }
    }

    /// Closes the open menu, and gives the keyboard back.
    fn close_menu(&mut self) {
        if let Some(open) = self.open_menu.take() {
            self.ui.close_popup();
            self.ui.focus(open.focus);
        }
        self.light_title(None);
    }

    /// Escape closes a menu inside the tree without a word to anybody; the
    /// bar's title is put out and the keyboard given back once it has.
    fn settle_menu(&mut self) {
        if self.open_menu.is_some() && !self.ui.popup_open() {
            self.close_menu();
        }
    }

    fn light_title(&mut self, index: Option<usize>) {
        if let Some(bar) = self.bar
            && let Some(bar) = self.ui.widget_mut::<MenuBar<Msg>>(bar)
        {
            bar.set_open(index);
        }
    }

    /// Brings the system's menus up to date with what is true now, when that
    /// has changed.
    #[cfg(target_os = "macos")]
    fn sync_menus(&mut self) {
        if self.native.is_none() {
            return;
        }
        let state = self.menu_state();
        if state != self.shown {
            let titles = menu::titles(&state, true);
            if let Some(native) = &mut self.native {
                native.sync(&titles);
            }
            self.shown = state;
        }
    }

    /// Does what a menu row, or its keys, asks for.
    fn run(&mut self, command: Command) {
        match command {
            Command::New => {
                let index = self.add_tab(FileDocument::empty());
                self.show_tab(index, true);
                self.say("new tab".into());
            }
            Command::Open => {
                if let Some(path) = self.pick_open() {
                    self.open_path(&path);
                }
            }
            Command::OpenRecent(n) => {
                if let Some(path) = self.memory.recent.paths().get(n).cloned() {
                    self.open_path(&path);
                }
            }
            Command::ClearRecent => self.memory.recent.clear(),
            Command::Close => self.close_tab(self.active),
            Command::CloseOtherTabs => self.close_other_tabs(),
            Command::Quit => {
                if self.may_discard_all() {
                    self.exit = true;
                }
            }
            Command::Save => self.save(),
            Command::SaveAs => self.save_as(),
            Command::Revert => self.reload_from_disk(true),
            Command::ReloadFromDisk => self.reload_from_disk(false),
            Command::Undo
            | Command::Redo
            | Command::Cut
            | Command::Copy
            | Command::Paste
            | Command::SelectAll => self.press(command),
            Command::Find => self.open_prompt(Ask::Find),
            Command::FindNext => self.find(true),
            Command::FindPrevious => self.find(false),
            Command::GoToLine => self.open_prompt(Ask::GoTo),
            Command::ZoomIn => self.zoom(1),
            Command::ZoomOut => self.zoom(-1),
            Command::ActualSize => self.zoom(0),
            Command::LineNumbers => {
                // A View menu that is also a setting: what it does is what the
                // settings say from now on.
                self.gutter = !self.gutter;
                let on = self.gutter;
                self.memory.settings.update(|s| s.editor.line_numbers = on);
                self.each_editor(|area| area.set_gutter(on));
            }
            Command::NextTab => self.step_tab(true),
            Command::PreviousTab => self.step_tab(false),
            Command::RenameTab => self.start_rename(self.active),
            Command::SetTabColor(color) => {
                self.tabs[self.active].color = color;
                self.sync_strip();
                self.remember_tabs();
            }
            Command::Format => self.start_format(),
            Command::ReadOnly => {
                let tab = &mut self.tabs[self.active];
                tab.read_only = !tab.read_only;
                let read_only = tab.read_only;
                self.editor().set_read_only(read_only);
                self.say(if read_only { "read only" } else { "editable" }.into());
            }
            Command::CopyPath => self.copy_path(),
            Command::Reveal => {
                let path = self.tabs[self.active].path.clone();
                if let Some(path) = path
                    && let Err(e) = reveal(&path)
                {
                    self.say(format!("showing {}: {e}", path.display()));
                }
            }
            Command::Settings => self.open_settings(),
            Command::Help => self.open_url(&format!("{}#readme", menu::HOME)),
            Command::ReportIssue => self.open_url(menu::ISSUES),
            Command::About => {
                MessageDialog::new()
                    .set_level(MessageLevel::Info)
                    .set_title("About squint")
                    .set_description(format!(
                        "squint {}\n\n{}\n\n{}",
                        env!("CARGO_PKG_VERSION"),
                        env!("CARGO_PKG_DESCRIPTION"),
                        menu::HOME
                    ))
                    .set_buttons(MessageButtons::Ok)
                    .show();
            }
        }
    }

    /// Does an edit by pressing its keys, so it happens exactly as they make
    /// it happen, to whatever has the keyboard.
    fn press(&mut self, command: Command) {
        let Some((code, modifiers)) = menu::edit_keys(command) else {
            return;
        };
        let key = |state| InputEvent::Key {
            code,
            state,
            repeat: false,
            modifiers,
        };
        self.ui
            .handle(&[key(ElementState::Down), key(ElementState::Up)]);
    }

    /// Acts on what the tree and the system's menus have said, until they
    /// have nothing more to say: a command can make the tree say something,
    /// as an edit pressed into the text area does. Whether anything was said.
    fn drain(&mut self) -> bool {
        let mut acted = false;
        // A command answered by another is fine; one that never stops is a
        // bug, and should not also hang the window.
        for _ in 0..8 {
            let messages: Vec<Msg> = self.ui.drain_messages().collect();
            #[cfg(target_os = "macos")]
            let chosen = self
                .native
                .as_ref()
                .map(NativeMenu::chosen)
                .unwrap_or_default();
            #[cfg(not(target_os = "macos"))]
            let chosen: Vec<Command> = Vec::new();
            let opened = crate::handoff::take();
            if messages.is_empty() && chosen.is_empty() && opened.is_empty() {
                break;
            }
            acted = true;
            for msg in messages {
                self.handle(msg);
            }
            for command in chosen {
                self.run(command);
            }
            for path in opened {
                self.open_path(&path);
            }
        }
        acted
    }

    // ---- files --------------------------------------------------------------

    /// Whether tab `index` may be put away: it has no unsaved changes, or the
    /// user said to save them and they are saved, or to lose them. The tab is
    /// brought to the front to be asked about.
    fn may_discard_tab(&mut self, index: usize) -> bool {
        if !self.is_modified(index) {
            return true;
        }
        self.show_tab(index, true);
        let name = self.tab_name(index);
        let answer = MessageDialog::new()
            .set_level(MessageLevel::Warning)
            .set_title("squint")
            .set_description(format!(
                "Do you want to save the changes you made to {name}?\n\n\
                 Your changes will be lost if you don't save them."
            ))
            .set_buttons(MessageButtons::YesNoCancelCustom(
                "Save".into(),
                "Don't Save".into(),
                "Cancel".into(),
            ))
            .show();
        match answer {
            MessageDialogResult::Yes => {}
            MessageDialogResult::Custom(button) if button == "Save" => {}
            MessageDialogResult::No => return true,
            MessageDialogResult::Custom(button) if button == "Don't Save" => return true,
            _ => return false,
        }
        self.save();
        !self.is_modified(index)
    }

    /// [`may_discard_tab`](Self::may_discard_tab) for every tab, stopping at
    /// the first Cancel.
    fn may_discard_all(&mut self) -> bool {
        let ids: Vec<TabId> = self.tabs.iter().map(|t| t.id).collect();
        ids.into_iter().all(|id| {
            self.index_of(id)
                .is_none_or(|index| self.may_discard_tab(index))
        })
    }

    fn pick_open(&self) -> Option<PathBuf> {
        let mut dialog = FileDialog::new().set_title("Open");
        if let Some(dir) = self.tabs[self.active]
            .path
            .as_deref()
            .and_then(Path::parent)
        {
            dialog = dialog.set_directory(dir);
        }
        dialog.pick_file()
    }

    /// Brings `path` to the front: the tab that has it, or a new one, which
    /// takes the place of an empty untitled tab in front. Whether it opened.
    fn open_path(&mut self, path: &Path) -> bool {
        if let Some(index) = self.tab_with(path) {
            self.show_tab(index, true);
            return true;
        }
        match FileDocument::open(path) {
            Ok(doc) => {
                let reuse = self.memory.settings.get().general.open_in == OpenIn::ReuseEmptyTab;
                let blank =
                    (reuse && self.is_blank(self.active)).then(|| self.tabs[self.active].id);
                let index = self.add_tab(doc);
                self.show_tab(index, true);
                if let Some(blank) = blank
                    && let Some(at) = self.index_of(blank)
                {
                    self.remove_tab(at);
                }
                self.memory.recent.add(path);
                self.sync_strip();
                self.remember_tabs();
                self.say(String::new());
                true
            }
            Err(e) => {
                if !path.exists() {
                    self.memory.recent.remove(path);
                }
                self.say(format!("{}: {e}", path.display()));
                false
            }
        }
    }

    /// Takes on what a tab's file decides: its tab stops, and whether it can
    /// be formatted.
    fn named(&mut self, index: usize) {
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        let (path, editor) = (tab.path.clone(), tab.editor);
        let tab_width = self.tab_width_for(path.as_deref());
        let Some(area) = self.area_mut(editor) else {
            return;
        };
        area.set_tab_width(tab_width);
        let head = area.document().head(8192);
        self.tabs[index].format_kind = format::detect(path.as_deref(), &head);
    }

    fn save(&mut self) {
        let Some(path) = self.tabs[self.active].path.clone() else {
            return self.save_as();
        };
        let result = self.editor().document_mut().save();
        match result {
            Ok(()) => {
                self.saved(&path);
                self.say("saved".into());
            }
            Err(e) => self.say(e),
        }
    }

    fn save_as(&mut self) {
        let current = self.tabs[self.active].path.clone();
        let mut dialog = FileDialog::new().set_title("Save As");
        match &current {
            Some(path) => {
                if let Some(dir) = path.parent() {
                    dialog = dialog.set_directory(dir);
                }
                dialog = dialog.set_file_name(name_of(path));
            }
            None => dialog = dialog.set_file_name("Untitled.txt"),
        }
        let Some(path) = dialog.save_file() else {
            return;
        };
        let result = self.editor().document_mut().save_as(&path);
        match result {
            Ok(()) => {
                let index = self.active;
                self.tabs[index].path = Some(path.clone());
                self.named(index);
                self.saved(&path);
                self.memory.recent.add(&path);
                self.sync_strip();
                self.remember_tabs();
                self.say(format!("saved as {}", path.display()));
            }
            Err(e) => self.say(e),
        }
    }

    /// The file in front was just written: what is on disk is what squint has.
    fn saved(&mut self, path: &Path) {
        let tab = &mut self.tabs[self.active];
        tab.stamp = Stamp::of(path);
        tab.ignore_changes = false;
    }

    /// Reads the file in front again. With changes in it, asks first — and
    /// Revert does nothing without any.
    fn reload_from_disk(&mut self, revert: bool) {
        let index = self.active;
        if self.tabs[index].path.is_none() {
            return;
        }
        let modified = self.is_modified(index);
        if revert && !modified {
            return;
        }
        if modified && !confirm_revert(&self.tab_name(index)) {
            return;
        }
        if self.reload(index) {
            self.say(if revert { "reverted" } else { "reloaded" }.into());
        }
    }

    /// Opens tab `index`'s file again in the same tab, at the same line.
    /// Whether it opened.
    fn reload(&mut self, index: usize) -> bool {
        let Some((path, editor)) = self
            .tabs
            .get(index)
            .and_then(|t| Some((t.path.clone()?, t.editor)))
        else {
            return false;
        };
        let doc = match FileDocument::open(&path) {
            Ok(doc) => doc,
            Err(e) => {
                self.say(format!("{}: {e}", path.display()));
                return false;
            }
        };
        let head = doc.head(8192);
        let line = self.area(editor).map_or(0, |area| area.caret().line);
        self.forget_editor(editor);
        let read_only = self.tabs[index].read_only;
        if let Some(area) = self.area_mut(editor) {
            area.set_document(doc);
            area.set_read_only(read_only);
        }
        let tab = &mut self.tabs[index];
        tab.format_kind = format::detect(Some(&path), &head);
        tab.stamp = Stamp::of(&path);
        tab.ignore_changes = false;
        tab.pending_line = (line > 0).then_some(line);
        true
    }

    /// Looks at every tab's file, every couple of seconds, and deals with the
    /// ones something else has changed: a tab with no changes of its own
    /// reloads quietly unless the settings say to ask, and one with changes
    /// always asks.
    fn check_files(&mut self) {
        if self.last_check.elapsed() < self.watch || self.rename.is_some() || self.ui.popup_open() {
            return;
        }
        self.last_check = Instant::now();
        if !self.memory.settings.get().general.watch_files {
            return;
        }
        let ids: Vec<TabId> = self.tabs.iter().map(|t| t.id).collect();
        for id in ids {
            let Some(index) = self.index_of(id) else {
                continue;
            };
            let tab = &self.tabs[index];
            if tab.ignore_changes {
                continue;
            }
            let Some(path) = tab.path.clone() else {
                continue;
            };
            let now = Stamp::of(&path);
            if now == tab.stamp {
                continue;
            }
            self.tabs[index].stamp = now;
            let name = self.tab_name(index);
            if now.is_none() {
                self.say(format!("{name} is no longer on disk"));
                continue;
            }
            let modified = self.is_modified(index);
            if !modified && !self.memory.settings.get().general.ask_before_reloading {
                if self.reload(index) {
                    self.say(format!("reloaded {name}: it changed on disk"));
                }
                continue;
            }
            self.show_tab(index, true);
            match ask_about_change(&name, modified) {
                Change::Reload => {
                    if self.reload(index) {
                        self.say(format!("reloaded {name}"));
                    }
                }
                Change::Keep => self.say(format!(
                    "{name} changed on disk: Reload from Disk shows it as it is"
                )),
                Change::Ignore => {
                    self.tabs[index].ignore_changes = true;
                    self.say(format!("not watching {name} until it is reloaded"));
                }
            }
        }
    }

    fn copy_path(&mut self) {
        let Some(path) = self.tabs[self.active].path.clone() else {
            return;
        };
        let text = path.display().to_string();
        let copied = self.clipboard.as_mut().map(|c| c.set_text(text.clone()));
        self.say(match copied {
            Some(Ok(())) => format!("copied {text}"),
            Some(Err(e)) => format!("clipboard: {e}"),
            None => "there is no clipboard to copy to".into(),
        });
    }

    fn open_url(&mut self, url: &str) {
        if let Err(e) = open_url(url) {
            self.say(format!("opening {url}: {e}"));
        }
    }

    // ---- the settings -------------------------------------------------------

    /// Asks for a settings window, and says so if there is one already: a
    /// window cannot be raised from here, and a second one editing the same
    /// file would be two drafts of it.
    fn open_settings(&mut self) {
        if self.settings.is_some() {
            self.say("the settings window is already open".into());
            return;
        }
        let link = Arc::new(Link::default());
        self.windows.push(SettingsWindow::request(
            self.memory.settings.get().clone(),
            self.settings_file.clone(),
            Arc::clone(&link),
            self.present,
        ));
        self.settings = Some(link);
    }

    /// Acts on what the settings window has said since the last frame. It
    /// edits the settings as they were when it opened, so what it hands over
    /// is what it showed — the same bargain as a file edited behind an open
    /// editor.
    fn hear_settings(&mut self) {
        let Some(link) = &self.settings else {
            return;
        };
        let mut closed = false;
        for word in link.drain() {
            match word {
                Word::Apply(settings) => {
                    if settings != *self.memory.settings.get() {
                        self.memory.settings.set(settings);
                    }
                    let complaint = self.apply_settings();
                    self.say(complaint.unwrap_or_else(|| match &self.settings_file {
                        Some(file) => format!("settings saved to {}", file.display()),
                        None => "settings applied".into(),
                    }));
                }
                // Shown, not kept: only the theme, which is what the settings
                // window offers to try. Everything else waits for Apply.
                Word::Preview(settings) => {
                    let theme = settings.theme().scaled(self.scale);
                    if *self.ui.theme() != theme {
                        self.ui.set_theme(theme);
                    }
                }
                Word::EditFile => {
                    // The settings' real home, opened as what it is: a file —
                    // written first, so there is one even if nothing was
                    // changed and nothing had been kept before.
                    self.memory.settings.save();
                    match self.settings_file.clone() {
                        Some(file) => {
                            self.open_path(&file);
                        }
                        None => self.say("this squint keeps no settings file".into()),
                    }
                }
                // Whatever a preview changed goes back to what is kept, which
                // after a Save is what was just saved.
                Word::Closed => closed = true,
            }
        }
        if closed {
            self.settings = None;
            let _ = self.apply_settings();
        }
    }

    /// Makes the window what the settings say: the theme, the faces, the
    /// text, the tab stops, the highlighting and how often files are looked
    /// at. Everything a setting decides is decided here, so applying them is
    /// one call whether they come from the settings window or from the file at
    /// launch. Answers with what it could not do, if anything.
    fn apply_settings(&mut self) -> Option<String> {
        let settings = self.memory.settings.get().clone();
        self.watch = Duration::from_secs(settings.general.watch_seconds.max(1) as u64);
        self.memory.recent.set_limit(settings.general.recent_files);

        let theme = settings.theme().scaled(self.scale);
        if *self.ui.theme() != theme {
            self.ui.set_theme(theme);
        }

        let px = |v: f32| (v * self.scale + 0.5) as u16;
        let ui_size = settings.appearance.ui_size as f32;
        let (chrome_font, chrome_missing) = face(
            &mut self.ui,
            &mut self.faces,
            settings.appearance.ui_font.as_deref(),
            fonts::UI,
        );
        let chrome = match chrome_font {
            Some(font) => {
                self.ui.set_default_font(font);
                TextStyle {
                    font,
                    size_px: px(ui_size),
                }
            }
            None => TextStyle::built_in(px(ui_size)),
        };
        let mono_px = px(settings.appearance.text_size as f32);
        let (mono_font, mono_missing) = face(
            &mut self.ui,
            &mut self.faces,
            settings.appearance.text_font.as_deref(),
            fonts::MONO,
        );
        let mono = match mono_font {
            Some(font) => TextStyle {
                font,
                size_px: mono_px,
            },
            None => TextStyle::built_in(mono_px),
        };
        let chrome_changed = chrome != self.chrome;
        self.chrome = chrome;
        self.small = TextStyle {
            size_px: px(ui_size - 1.0),
            ..chrome
        };
        self.mono = mono;
        self.text_size = settings.appearance.text_size;
        if chrome_changed {
            self.restyle_chrome();
        }

        // The text: the face and its size, the gutter, the tab stops, how the
        // wheel scrolls it, and whether syntect is colouring it.
        squint_core::syntax::set_theme(&settings.highlighting.theme);
        self.gutter = settings.editor.line_numbers;
        let style = self.editor_style();
        let gutter = self.gutter;
        let smooth_scroll = settings.editor.smooth_scroll;
        let tabs: Vec<(NodeId, u8)> = self
            .tabs
            .iter()
            .map(|tab| (tab.editor, self.tab_width_for(tab.path.as_deref())))
            .collect();
        for (editor, tab_width) in tabs {
            let wanted = self
                .area(editor)
                .is_some_and(|area| self.syntax_wanted(&area.document()));
            if let Some(area) = self.area_mut(editor) {
                area.set_style(style);
                area.set_gutter(gutter);
                area.set_tab_width(tab_width);
                area.set_smooth_scroll(smooth_scroll);
                // Even when nothing about the highlighting changed: the theme
                // may have, and colours already parsed were parsed with the
                // old one.
                area.document_mut().allow_syntax(wanted);
            }
        }
        self.refresh_status();
        mono_missing
            .or(chrome_missing)
            .map(|name| format!("no face called {name} on this machine: drawing in another"))
    }

    /// Whether a document should be coloured: the settings say so, and it is
    /// not bigger than they allow.
    fn syntax_wanted(&self, doc: &FileDocument) -> bool {
        let highlighting = &self.memory.settings.get().highlighting;
        highlighting.enabled && doc.len() <= highlighting.max_mb.saturating_mul(1 << 20)
    }

    /// Tab stops for a file: what its project says, when the settings follow
    /// `.editorconfig` and it says anything, and the settings otherwise.
    fn tab_width_for(&self, path: Option<&Path>) -> u8 {
        let editor = &self.memory.settings.get().editor;
        let from_project = editor
            .follow_editorconfig
            .then(|| path.map(editorconfig::properties_for))
            .flatten()
            .and_then(|props| props.tab_width());
        from_project.unwrap_or(editor.tab_width)
    }

    /// Draws the menus, the tabs and the status line in the face the settings
    /// now name. DeniseUI's menu bar takes its face when it is made, so that
    /// one is made again.
    fn restyle_chrome(&mut self) {
        let w = self.ui.size().width as i32;
        if let Some(bar) = self.bar {
            let labels: Vec<String> = menu::titles(&State::default(), false)
                .into_iter()
                .map(|t| t.label)
                .collect();
            self.ui.remove(bar);
            let widget = MenuBar::new(labels, Msg::MenuTitle).with_style(self.chrome);
            let theme = *self.ui.theme();
            let height = widget.preferred_height(&theme, self.ui.text_mut());
            let root = self.ui.root();
            match self.ui.add(root, widget, Rect::new(0, 0, w, height)) {
                Some(id) => {
                    self.ui.set_anchors(id, TOP_ROW);
                    self.bar = Some(id);
                    self.bar_h = height;
                }
                None => {
                    self.bar = None;
                    self.bar_h = 0;
                }
            }
        }
        let small = self.small;
        if let Some(strip) = self.ui.widget_mut::<Tabs<Msg>>(self.strip) {
            strip.set_style(small);
        }
        let theme = *self.ui.theme();
        if let Some(strip) = self.ui.widget::<Tabs<Msg>>(self.strip) {
            self.strip_h = strip.strip_height(&theme);
        }
        let status_style = TextStyle {
            size_px: (self.chrome.size_px as f32 - 2.0 * self.scale).max(6.0) as u16,
            ..self.chrome
        };
        if let Some(label) = self.ui.widget_mut::<Label>(self.status) {
            label.set_style(status_style);
        }
        self.status_h = status_height(self.memory.settings.get().appearance.ui_size, self.scale);
        self.relayout();
    }

    /// Puts the menu bar, the tabs, the text and the status line back where
    /// they belong: after a face has changed their heights.
    fn relayout(&mut self) {
        let size = self.ui.size();
        let (w, h) = (size.width as i32, size.height as i32);
        let s = |v: i32| (v as f32 * self.scale + 0.5) as i32;
        if let Some(bar) = self.bar {
            self.ui.set_layout(bar, Rect::new(0, 0, w, self.bar_h));
        }
        self.ui
            .set_layout(self.strip, Rect::new(0, self.bar_h, w, self.strip_h));
        self.page_top = self.bar_h + self.strip_h;
        self.ui.set_layout(
            self.status,
            Rect::new(s(8), h - self.status_h, w - s(16), self.status_h),
        );
        let page = self.page_rect();
        let editors: Vec<NodeId> = self.tabs.iter().map(|t| t.editor).collect();
        for editor in editors {
            self.ui.set_layout(editor, page);
        }
    }

    // ---- the view -----------------------------------------------------------

    /// One size bigger for `1`, one smaller for `-1`, the usual for `0`. Every
    /// tab's text is the same size.
    fn zoom(&mut self, step: i32) {
        let now = self.text_size;
        let usual = self.memory.settings.get().appearance.text_size;
        let size = match step {
            0 => Some(usual),
            s if s > 0 => TEXT_SIZES.iter().copied().find(|&t| t > now),
            _ => TEXT_SIZES.iter().rev().copied().find(|&t| t < now),
        }
        .unwrap_or(now);
        self.text_size = size;
        let style = self.editor_style();
        self.each_editor(|area| area.set_style(style));
        self.say(format!("text size {size}"));
    }

    fn each_editor(&mut self, mut apply: impl FnMut(&mut TextArea<Msg, FileDocument>)) {
        let editors: Vec<NodeId> = self.tabs.iter().map(|t| t.editor).collect();
        for editor in editors {
            if let Some(area) = self.area_mut(editor) {
                apply(area);
            }
        }
    }

    // ---- the prompt -------------------------------------------------------

    /// Opens a field in the status line's place, or refocuses the one open.
    fn open_prompt(&mut self, ask: Ask) {
        if let Some(prompt) = &self.prompt {
            if prompt.ask == ask {
                let field = prompt.field;
                self.ui.focus(Some(field));
                return;
            }
            self.close_prompt();
        }
        let Some(row) = self.ui.bounds(self.status) else {
            return;
        };
        let px = |v: f32| (v * self.scale + 0.5) as u16;
        let s = |v: i32| (v as f32 * self.scale + 0.5) as i32;
        let (hint, max_chars, text) = match ask {
            Ask::GoTo => (
                "Go to line — Enter to jump, Esc to close",
                12,
                String::new(),
            ),
            Ask::Find => (
                "Find — Enter for next, Shift+Enter for previous, Esc to close",
                512,
                self.last_query.clone(),
            ),
        };
        let mut input = TextInput::<Msg>::new()
            .with_placeholder(hint)
            .with_submit(Msg::Submit)
            .with_max_chars(max_chars)
            .with_size(px(11.0));
        input.set_text(text);
        let root = self.ui.root();
        let field_w = row.width.min(s(440));
        let field_rect = Rect::new(row.x, row.y + s(2), field_w, row.height - s(4));
        let Some(field) = self.ui.add(root, input, field_rect) else {
            return;
        };
        let gap = s(12);
        let note_rect = Rect::new(
            row.x + field_w + gap,
            row.y,
            (row.width - field_w - gap).max(0),
            row.height,
        );
        let Some(note) = self
            .ui
            .add(root, Label::new("").with_size(px(11.0)), note_rect)
        else {
            self.ui.remove(field);
            return;
        };
        self.ui.set_anchors(
            field,
            Anchors {
                left: true,
                top: false,
                right: false,
                bottom: true,
            },
        );
        self.ui.set_anchors(note, BOTTOM_ROW);
        self.ui.set_visible(self.status, false);
        self.ui.focus(Some(field));
        self.prompt = Some(Prompt { ask, field, note });
    }

    fn close_prompt(&mut self) {
        if let Some(prompt) = self.prompt.take() {
            self.ui.remove(prompt.field);
            self.ui.remove(prompt.note);
            self.ui.set_visible(self.status, true);
            let editor = self.editor_id();
            self.ui.focus(Some(editor));
            self.refresh_status();
        }
    }

    /// What is typed in the prompt's field.
    fn prompt_text(&self) -> String {
        self.prompt
            .as_ref()
            .and_then(|p| self.ui.widget::<TextInput<Msg>>(p.field))
            .map(|f| f.text().to_string())
            .unwrap_or_default()
    }

    fn submit(&mut self) {
        match self.prompt.as_ref().map(|p| p.ask) {
            Some(Ask::GoTo) => self.go_to(),
            Some(Ask::Find) => self.find(true),
            None => {}
        }
    }

    /// Says what happened: in the note beside an open prompt, and in the
    /// status line either way.
    fn say(&mut self, text: String) {
        self.notice = text;
        if let Some(note) = self.prompt.as_ref().map(|p| p.note)
            && let Some(label) = self.ui.widget_mut::<Label>(note)
        {
            label.set_text(self.notice.clone());
        }
        self.refresh_status();
    }

    // ---- going to a line ----------------------------------------------------

    /// Enter in the go-to-line field: jump, or say why not.
    fn go_to(&mut self) {
        let text = self.prompt_text().trim().to_string();
        self.close_prompt();
        match text.parse::<usize>() {
            Ok(n) if n > 0 => self.go_to_line(n),
            _ => self.say(format!("not a line number: {text:?}")),
        }
    }

    // ---- finding ------------------------------------------------------------

    /// Starts a search from the caret: forwards from the end of the
    /// selection, backwards from its start, so a match that is already
    /// selected is stepped past rather than found again.
    fn find(&mut self, forward: bool) {
        let query = match self.prompt.as_ref().map(|p| p.ask) {
            Some(Ask::Find) => self.prompt_text(),
            _ => self.last_query.clone(),
        };
        if query.is_empty() {
            self.open_prompt(Ask::Find);
            self.say("type something to find".into());
            return;
        }
        self.last_query = query.clone();
        let editor = self.editor_id();
        let area = self.editor();
        let from = match area.selection() {
            Some((start, end)) => {
                if forward {
                    end
                } else {
                    start
                }
            }
            None => area.caret(),
        };
        let doc = area.document_mut();
        let Some(origin) = doc.offset_of(from) else {
            self.say("the caret's line has not been counted yet".into());
            return;
        };
        let find = doc.find(&query, origin, forward);
        self.search = Some(Search {
            editor,
            find,
            query,
            hit: None,
        });
        self.pump_find();
    }

    /// Searches for a few milliseconds, and selects the match when there is
    /// one and the index can say which line it is on.
    fn pump_find(&mut self) {
        let Some(mut search) = self.search.take() else {
            return;
        };
        let editor = search.editor;
        if search.hit.is_none() {
            let deadline = Instant::now() + SLICE_TIME;
            let Some(area) = self.area_mut(editor) else {
                return;
            };
            let doc = area.document_mut();
            let mut missing = false;
            loop {
                match doc.find_step(&mut search.find, FIND_SLICE) {
                    FindStep::Pending if Instant::now() < deadline => {}
                    FindStep::Pending => break,
                    FindStep::Found { at, wrapped } => {
                        search.hit = Some((at, wrapped));
                        break;
                    }
                    FindStep::NotFound => {
                        missing = true;
                        break;
                    }
                }
            }
            if missing {
                self.say(format!("no match for {:?}", search.query));
                return;
            }
        }
        let Some((at, wrapped)) = search.hit else {
            let (done, total) = search.find.progress();
            let percent = done * 100 / total.max(1);
            self.say(format!("finding {:?}… {percent}%", search.query));
            self.search = Some(search);
            return;
        };
        // A match past what the index has scanned cannot be given a line
        // without counting there by hand, which on a big file is the slow
        // read the index exists to avoid. The index is fast; wait for it.
        let indexed = self
            .area(editor)
            .is_some_and(|area| area.document().is_indexed());
        if !indexed {
            self.say("found — counting lines to reach it…".into());
            self.search = Some(search);
            return;
        }
        let end = at + search.query.len() as u64;
        let Some(area) = self.area_mut(editor) else {
            return;
        };
        let (from, to) = {
            let doc = area.document_mut();
            (doc.pos_of(at), doc.pos_of(end))
        };
        match (from, to) {
            (Some(from), Some(to)) => {
                area.select_range(from, to);
                let place = format!("line {}", from.line + 1);
                self.say(if wrapped {
                    format!("{place} — wrapped around")
                } else {
                    place
                });
            }
            _ => self.say(format!("found {:?} but could not place it", search.query)),
        }
    }

    // ---- formatting ---------------------------------------------------------

    /// Pretty-prints the file as JSON or XML into a new file, a slice per
    /// frame, and opens that in a tab of its own when it is complete.
    fn start_format(&mut self) {
        if self.formatting.is_some() {
            return;
        }
        let editor = self.editor_id();
        let path = self.tabs[self.active].path.clone();
        let head = self.editor_ref().document().head(8192);
        let Some(kind) = format::detect(path.as_deref(), &head) else {
            self.say("not JSON or XML: nothing to format".into());
            return;
        };
        // The settings' layout, with the file's project's over it where the
        // settings follow one.
        let settings = self.memory.settings.get();
        let props = settings
            .formatting
            .follow_editorconfig
            .then(|| path.as_deref().map(editorconfig::properties_for))
            .flatten()
            .unwrap_or_default();
        let style = settings.format_style().with_editorconfig(&props);
        let note = style_note(style, &props);
        let (len, edited) = {
            let doc = self.editor_ref().document();
            (doc.len(), doc.is_modified())
        };
        // Untouched, and a file of its own: the file is projected on a thread,
        // which reads it once and writes nothing. A document with edits in it
        // has bytes only this process's piece table holds, so there is nothing
        // to project from: those are formatted into a copy beside the file,
        // stepped between frames like the index and the find.
        let work = match (path, edited) {
            (Some(src), false) => project_on_thread(src, kind, style),
            (path, _) => {
                let (part, file) = match make_part(path.as_deref()) {
                    Ok(made) => made,
                    Err(e) => {
                        self.say(format!("formatting: {e}"));
                        return;
                    }
                };
                Work::Slices {
                    job: self.editor_ref().document().format(kind, style),
                    writer: BufWriter::with_capacity(1 << 20, file),
                    part,
                }
            }
        };
        self.formatting = Some(Formatting {
            editor,
            kind,
            note,
            len,
            work,
        });
        self.pump_format();
    }

    /// Looks in on the format: a few milliseconds of it where it is being
    /// stepped here, and whether the thread has finished where it is not.
    fn pump_format(&mut self) {
        let Some(mut running) = self.formatting.take() else {
            return;
        };
        if self.area(running.editor).is_none() {
            running.discard();
            return;
        }
        let mut projected = None;
        let step = match &mut running.work {
            Work::Project { result, .. } => match result.try_recv() {
                Ok(Ok(scan)) => {
                    projected = Some(scan);
                    Ok(true)
                }
                Ok(Err(e)) => Err(e),
                Err(TryRecvError::Empty) => Ok(false),
                Err(TryRecvError::Disconnected) => {
                    Err(io::Error::other("the format stopped before it finished"))
                }
            },
            Work::Slices { job, writer, .. } => {
                let deadline = Instant::now() + SLICE_TIME;
                let doc = self.area(running.editor).expect("checked").document();
                loop {
                    match doc.format_step(job, FORMAT_SLICE, writer) {
                        Ok(false) if Instant::now() < deadline => {}
                        other => break other,
                    }
                }
            }
        };
        match step {
            Ok(false) => {
                let percent = running.done() * 100 / running.len.max(1);
                let kind = running.kind.name();
                self.say(format!("formatting as {kind}… {percent}%"));
                self.formatting = Some(running);
            }
            Ok(true) => self.finish_format(running, projected),
            Err(e) => {
                running.discard();
                self.say(format!("formatting: {e}"));
            }
        }
    }

    /// Puts the formatted document in front of the tab it came from: the same
    /// file, with the format in it as unsaved changes, which one undo takes
    /// back and a save writes.
    fn finish_format(&mut self, running: Formatting, projected: Option<Projection>) {
        let Formatting {
            editor,
            kind,
            note,
            work,
            ..
        } = running;
        // A projection is a document already; a copy on disk has to be closed
        // and opened before it can be read back.
        let held = match (projected, work) {
            (Some(scan), _) => match scan.into_document() {
                Some(doc) => (doc, None),
                None => {
                    self.say("formatting: the scan did not finish".into());
                    return;
                }
            },
            (None, Work::Slices { writer, part, .. }) => {
                let opened = writer
                    .into_inner()
                    .map_err(|e| e.into_error())
                    .and_then(|file| file.sync_all())
                    .and_then(|()| Document::open(&part));
                match opened {
                    Ok(doc) => (doc, Some(part)),
                    Err(e) => {
                        let _ = fs::remove_file(&part);
                        self.say(format!("formatting: {e}"));
                        return;
                    }
                }
            }
            (None, work) => {
                drop(work);
                return;
            }
        };
        let (formatted, part) = held;
        let Some(area) = self.area_mut(editor) else {
            if let Some(part) = part {
                let _ = fs::remove_file(part);
            }
            return;
        };
        area.document_mut().format_in_place(formatted, part);
        // The formatted document is bigger than the one that was opened, and
        // may have grown past what the settings colour.
        let wanted = match self.area(editor) {
            Some(area) => self.syntax_wanted(&area.document()),
            None => return,
        };
        if let Some(area) = self.area_mut(editor) {
            area.document_mut().allow_syntax(wanted);
        }
        let kind = kind.name();
        self.say(format!("formatted as {kind} with {note}"));
    }

    fn cancel_format(&mut self, why: &str) {
        if let Some(running) = self.formatting.take() {
            running.discard();
            self.say(format!("formatting stopped: {why}"));
        }
    }

    /// Puts the view back to the top of a document that was swapped whole —
    /// formatted, or a format undone — where its line numbers mean something
    /// else now.
    fn settle_swap(&mut self) {
        let editor = self.editor_id();
        let Some(area) = self.area_mut(editor) else {
            return;
        };
        if area.document_mut().take_view_stale() {
            area.go_to(0);
        }
    }

    // ---- the rest -----------------------------------------------------------

    fn refresh_status(&mut self) {
        let name = self.tab_name(self.active);
        let editor = self.editor();
        let caret = editor.caret();
        let modified = editor.document().is_modified();
        let doc = editor.document_mut();
        let error = doc.take_error();
        let (scanned, total) = doc.index_progress();
        let lines = if doc.is_indexed() {
            format!("{} lines", doc.line_count().unwrap_or(0))
        } else {
            let percent = (scanned * 100).checked_div(total).unwrap_or(100);
            format!("counting… {percent}%")
        };
        let syntax = doc
            .syntax_name()
            .map(|name| format!("   {name}"))
            .unwrap_or_default();
        let held = doc.memory_bytes() / 1024;
        if let Some(error) = error {
            self.notice = error;
        }
        let mark = if modified { " •" } else { "" };
        let text = format!(
            "Ln {}, Col {}   {lines}{syntax}   {held} KB held{mark}   {}",
            caret.line + 1,
            caret.col + 1,
            self.notice
        );
        if let Some(label) = self.ui.widget_mut::<Label>(self.status) {
            label.set_text(text);
        }
        self.title = format!("{}{name} — squint", if modified { "• " } else { "" });
    }

    fn handle(&mut self, msg: Msg) {
        match msg {
            // What a search or a format has read moved with the edit.
            Msg::Changed => {
                let editor = self.editor_id();
                if self.search.as_ref().is_some_and(|s| s.editor == editor) {
                    self.search = None;
                }
                if self.formatting.as_ref().is_some_and(|f| f.editor == editor) {
                    self.cancel_format("the text changed");
                }
            }
            Msg::Submit => self.submit(),
            Msg::Clipboard(ClipboardRequest::Copy(text) | ClipboardRequest::Cut(text)) => {
                if let Some(clip) = &mut self.clipboard
                    && let Err(e) = clip.set_text(text)
                {
                    self.notice = format!("clipboard: {e}");
                }
            }
            Msg::Clipboard(ClipboardRequest::Paste) => {
                let text = self.clipboard.as_mut().and_then(|c| c.get_text().ok());
                if let Some(text) = text {
                    self.editor().insert_text(&text);
                }
            }
            Msg::MenuTitle(index) => self.open_title(index),
            Msg::Menu(event) => self.menu_event(event),
            Msg::Tab(event) => self.tab_event(event),
            Msg::Renamed => self.finish_rename(true),
        }
    }

    /// Marks every match of what is typed in the find field on the lines on
    /// screen of the tab in front, as it is typed, and nothing once the field
    /// closes — or in a tab that has gone behind.
    fn sync_highlight(&mut self) {
        let query = match self.prompt.as_ref().map(|p| p.ask) {
            Some(Ask::Find) => self.prompt_text(),
            _ => String::new(),
        };
        let editor = self.editor_id();
        if self
            .highlighted
            .as_ref()
            .is_some_and(|(node, marked)| *node == editor && *marked == query)
        {
            return;
        }
        if let Some((node, _)) = self.highlighted.take()
            && node != editor
            && let Some(area) = self.area_mut(node)
        {
            area.document_mut().set_highlight("");
        }
        if let Some(area) = self.area_mut(editor) {
            area.document_mut().set_highlight(&query);
        }
        self.highlighted = Some((editor, query));
    }

    /// Whether the find field has the keyboard.
    fn typing_a_find(&self) -> bool {
        self.prompt
            .as_ref()
            .is_some_and(|p| p.ask == Ask::Find && self.ui.focused() == Some(p.field))
    }

    /// Whether any tab has a file to keep an eye on, and the settings say to.
    fn watching(&self) -> bool {
        self.memory.settings.get().general.watch_files
            && self
                .tabs
                .iter()
                .any(|t| t.path.is_some() && !t.ignore_changes)
    }
}

const ALL_SIDES: Anchors = Anchors {
    left: true,
    top: true,
    right: true,
    bottom: true,
};

const TOP_ROW: Anchors = Anchors {
    left: true,
    top: true,
    right: true,
    bottom: false,
};

const BOTTOM_ROW: Anchors = Anchors {
    left: true,
    top: false,
    right: true,
    bottom: true,
};

/// Asks whether to throw the changes to `name` away.
fn confirm_revert(name: &str) -> bool {
    let answer = MessageDialog::new()
        .set_level(MessageLevel::Warning)
        .set_title("squint")
        .set_description(format!(
            "Revert {name} to the saved version?\n\nYour changes will be lost."
        ))
        .set_buttons(MessageButtons::OkCancelCustom(
            "Revert".into(),
            "Cancel".into(),
        ))
        .show();
    match answer {
        MessageDialogResult::Ok => true,
        MessageDialogResult::Custom(button) => button == "Revert",
        _ => false,
    }
}

/// Asks what to do about `name`, which something else has changed.
fn ask_about_change(name: &str, modified: bool) -> Change {
    let consequence = if modified {
        "Reloading it throws away the changes you have made to it here."
    } else {
        "What squint shows may not match it until it is reloaded."
    };
    let answer = MessageDialog::new()
        .set_level(MessageLevel::Warning)
        .set_title("squint")
        .set_description(format!(
            "{name} has been changed by another program.\n\n{consequence}"
        ))
        .set_buttons(MessageButtons::YesNoCancelCustom(
            "Reload".into(),
            "Keep".into(),
            "Ignore Further Changes".into(),
        ))
        .show();
    match answer {
        MessageDialogResult::Yes => Change::Reload,
        MessageDialogResult::Custom(button) if button == "Reload" => Change::Reload,
        MessageDialogResult::Custom(button) if button == "Ignore Further Changes" => Change::Ignore,
        _ => Change::Keep,
    }
}

/// Where a formatted copy of `path` goes: the system's temporary directory,
/// under the file's name with `-formatted` before the extension. It keeps the
/// Makes the file a format streams into: a copy beside the file being
/// formatted, so putting it in place on a save is a rename rather than a
/// second pass over the whole document.
///
/// A directory squint cannot write in — a read-only checkout, a file opened
/// from somewhere that is not ours — falls back to the temporary directory,
/// where the save costs a copy after all. The name is the file's, hidden, and
/// carries this process's id, so two squints formatting the same file do not
/// write into each other.
fn make_part(path: Option<&Path>) -> io::Result<(PathBuf, File)> {
    let name = path
        .and_then(Path::file_name)
        .map_or_else(|| "untitled".into(), |n| n.to_string_lossy().into_owned());
    let leaf = format!(".{name}.squint-formatted.{}", std::process::id());
    if let Some(dir) = path
        .and_then(Path::parent)
        .filter(|d| !d.as_os_str().is_empty())
    {
        let beside = dir.join(&leaf);
        if let Ok(file) = File::create(&beside) {
            return Ok((beside, file));
        }
    }
    let dir = std::env::temp_dir().join("squint");
    fs::create_dir_all(&dir)?;
    let fallback = dir.join(&leaf);
    let file = File::create(&fallback)?;
    Ok((fallback, file))
}

/// Projects `src` on a thread of its own: one pass through the formatter,
/// measuring and marking what comes out rather than keeping it.
///
/// The thread opens the file again rather than reading the document squint
/// has, so squint's frames are not in the way — six milliseconds a frame
/// would take three times as long as the disk does. It is only ever started
/// for a document with nothing typed in it, so the file and the document are
/// the same bytes.
fn project_on_thread(src: PathBuf, kind: Kind, style: Style) -> Work {
    let done = Arc::new(AtomicU64::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let (tx, result) = mpsc::channel();
    let (counter, flag) = (done.clone(), stop.clone());
    std::thread::spawn(move || {
        let outcome = (|| {
            let source = Arc::new(FileSource::open(&src)?);
            let mut scan = Projection::new(source, kind, style);
            loop {
                if flag.load(Ordering::Relaxed) {
                    return Err(io::Error::new(io::ErrorKind::Interrupted, "stopped"));
                }
                let finished = scan.advance(FORMAT_THREAD_SLICE)?;
                counter.store(scan.progress().0, Ordering::Relaxed);
                if finished {
                    break;
                }
            }
            Ok(scan)
        })();
        let _ = tx.send(outcome);
    });
    Work::Project { done, stop, result }
}

/// The face `wanted` names, or the first of `preferred` this machine has, as a
/// font of the tree — added once however often it is asked for.
///
/// The second half of the answer is the face that was asked for and could not
/// be used: a file removed since it was chosen, or one nothing here can read.
/// Something else is being drawn in, and saying so is better than leaving
/// somebody to wonder why their choice did nothing.
fn face(
    ui: &mut Ui<Msg>,
    faces: &mut HashMap<String, FontId>,
    wanted: Option<&str>,
    preferred: &[&str],
) -> (Option<FontId>, Option<String>) {
    let asked = wanted.and_then(fonts::load_named);
    let missing = match (wanted, &asked) {
        (Some(name), None) => Some(name.to_string()),
        _ => None,
    };
    let Some((file, source)) = asked.or_else(|| fonts::load(preferred)) else {
        return (None, missing);
    };
    if let Some(id) = faces.get(&file) {
        return (Some(*id), missing);
    }
    let id = ui.add_font(source);
    faces.insert(file, id);
    (Some(id), missing)
}

/// A style in a few words, and where it came from:
/// `4 spaces, LF from /work/proj/.editorconfig`, or from the settings.
fn style_note(style: Style, props: &Properties) -> String {
    match props.sources().first() {
        Some(file) => format!("{} from {}", style.describe(), file.display()),
        None => format!("{} from the settings", style.describe()),
    }
}

/// How tall the status line is for a chrome of this size.
/// A physical pixel count as logical pixels: what the window covers of the
/// desk, whatever the display's DPI.
fn logical(physical: u32, scale: f32) -> u32 {
    if scale <= 0.0 {
        return physical;
    }
    ((physical as f32 / scale).round() as u32).max(1)
}

fn status_height(ui_size: u16, scale: f32) -> i32 {
    ((ui_size as f32 + 11.0) * scale + 0.5) as i32
}

fn name_of(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// Starts `program` and lets it get on with it, waited for on a thread of its
/// own so it does not linger as a zombie.
fn launch<S: AsRef<OsStr>>(program: &str, args: impl IntoIterator<Item = S>) -> io::Result<()> {
    let mut child = std::process::Command::new(program).args(args).spawn()?;
    std::thread::spawn(move || child.wait());
    Ok(())
}

/// Opens `url` in the user's browser.
fn open_url(url: &str) -> io::Result<()> {
    if cfg!(target_os = "macos") {
        launch("open", [url])
    } else if cfg!(windows) {
        launch("explorer", [url])
    } else {
        launch("xdg-open", [url])
    }
}

/// Shows `path` in the platform's file manager: selected, where it can be.
fn reveal(path: &Path) -> io::Result<()> {
    if cfg!(target_os = "macos") {
        launch("open", [OsStr::new("-R"), path.as_os_str()])
    } else if cfg!(windows) {
        let mut select = OsString::from("/select,");
        select.push(path);
        launch("explorer", [select])
    } else {
        let dir = path.parent().unwrap_or(Path::new("/"));
        launch("xdg-open", [dir.as_os_str()])
    }
}

impl DeniseApp for App {
    fn update(&mut self, events: &[InputEvent], damage: &mut DamageTracker) {
        let mut forwarded: Vec<InputEvent> = Vec::with_capacity(events.len());
        for event in events {
            // What the window itself is doing. Noted rather than taken: the
            // tree wants the resize as much as the window's memory does.
            match event {
                InputEvent::SurfaceResized { size, scale_factor } => {
                    self.last_size = (
                        logical(size.width, *scale_factor),
                        logical(size.height, *scale_factor),
                    );
                }
                InputEvent::SurfaceMoved {
                    position,
                    maximized,
                } => self.note_place(*position, *maximized),
                _ => {}
            }
            // An open menu has the keyboard, Escape included.
            let menu_up = self.ui.popup_open();
            // Ctrl let go ends a walk along the tabs. A key that arrives
            // without Ctrl says so too, in case the release itself went
            // somewhere else — to another application, say.
            if let InputEvent::Key {
                code,
                state,
                modifiers,
                ..
            } = event
                && self.switcher.walking()
                && (!modifiers.contains(Modifiers::CTRL)
                    || (*state == ElementState::Up
                        && matches!(code, KeyCode::ControlLeft | KeyCode::ControlRight)))
            {
                self.ctrl_released();
            }
            match event {
                // Answered by `close_requested`, which could still say no.
                InputEvent::CloseRequested => continue,
                InputEvent::Key {
                    code: KeyCode::Escape,
                    state: ElementState::Down,
                    ..
                } if self.rename.is_some() && !menu_up => {
                    self.finish_rename(false);
                    continue;
                }
                InputEvent::Key {
                    code: KeyCode::Escape,
                    state: ElementState::Down,
                    ..
                } if self.prompt.is_some() && !menu_up => {
                    self.close_prompt();
                    continue;
                }
                // Enter alone is the field's own submit, which finds forwards.
                InputEvent::Key {
                    code: KeyCode::Enter | KeyCode::NumpadEnter,
                    state: ElementState::Down,
                    modifiers,
                    ..
                } if modifiers.contains(Modifiers::SHIFT) && self.typing_a_find() => {
                    self.find(false);
                    continue;
                }
                // Ctrl+Tab, on every platform: the control key, not ⌘.
                InputEvent::Key {
                    code: KeyCode::Tab,
                    state: ElementState::Down,
                    modifiers,
                    ..
                } if modifiers.contains(Modifiers::CTRL) && !menu_up => {
                    self.cycle_tab(modifiers.contains(Modifiers::SHIFT));
                    continue;
                }
                // F10 goes to the menu bar, as it does on Windows and on most
                // Linux desktops.
                InputEvent::Key {
                    code: KeyCode::F10,
                    state: ElementState::Down,
                    ..
                } if self.bar.is_some() && !menu_up => {
                    self.open_title(0);
                    continue;
                }
                InputEvent::Key {
                    code,
                    state: ElementState::Down,
                    modifiers,
                    ..
                } if !menu_up => {
                    if let Some(command) = menu::shortcut(*code, *modifiers) {
                        self.run(command);
                        continue;
                    }
                }
                _ => {}
            }
            forwarded.push(event.clone());
        }
        self.ui.handle(&forwarded);
        self.ui.tick(self.started.elapsed().as_millis() as u64);
        self.settle_menu();
        let acted = self.drain();
        self.settle_rename();
        self.sync_highlight();
        self.pump_index();
        self.settle_pending_line();
        self.pump_find();
        self.pump_format();
        self.settle_swap();
        self.pump_syntax();
        self.settle_window();
        self.check_files();
        self.hear_settings();
        self.sync_strip();
        #[cfg(target_os = "macos")]
        self.sync_menus();
        if !forwarded.is_empty() || acted {
            self.refresh_status();
        }
        if self.ui.needs_paint() {
            let pending = self.ui.pending_damage();
            if pending.is_empty() {
                damage.add_full();
            } else {
                for rect in pending {
                    damage.add(*rect);
                }
            }
        }
    }

    fn render(&mut self, frame: &mut Frame<'_>, _damage: &[Rect]) {
        self.ui.paint(frame);
        self.ui.presented();
    }

    fn paint(&mut self, pen: &mut Pen<'_>, age: BufferAge, _damage: &[Rect]) -> bool {
        self.ui.paint_with(pen, age);
        self.ui.presented();
        true
    }

    /// The windows asked for since the last frame: the settings window, when
    /// Settings… has been picked.
    fn take_windows(&mut self) -> Vec<WindowRequest> {
        std::mem::take(&mut self.windows)
    }

    fn exit_requested(&self) -> bool {
        self.exit
    }

    /// The close button asks about every tab's unsaved changes first, and a
    /// Cancel keeps the window.
    fn close_requested(&mut self) -> bool {
        self.may_discard_all()
    }

    /// Every way out comes through here, ⌘Q and the Dock's Quit included:
    /// the place the tabs are sure to be written down.
    fn exiting(&mut self) {
        self.remember_tabs();
        // Quit on macOS ends the process from here: nothing below is dropped
        // by itself, so the copies an unfinished or unsaved format left are
        // cleared up while there is still somewhere to do it from.
        self.cancel_format("squint is closing");
        for editor in self.tabs.iter().map(|t| t.editor).collect::<Vec<_>>() {
            if let Some(area) = self.area_mut(editor) {
                area.document_mut().discard_format();
            }
        }
    }

    fn title(&self) -> Option<&str> {
        Some(&self.title)
    }

    fn next_frame_in(&self) -> Option<Duration> {
        let doc = self.editor_ref().document();
        if !doc.is_indexed()
            || doc.highlighting()
            || self.search.is_some()
            || self.formatting.is_some()
        {
            // Straight back: there is a slice of the file to walk.
            return Some(Duration::ZERO);
        }
        if doc.syntax_undecided() {
            // The grammars are loading on their thread; look again shortly.
            return Some(Duration::from_millis(16));
        }
        drop(doc);
        let now = self.started.elapsed().as_millis() as u64;
        let animation = self
            .ui
            .next_wake_ms()
            .map(|w| Duration::from_millis(w.saturating_sub(now)));
        // And when the files are next due a look.
        let files = self
            .watching()
            .then(|| self.watch.saturating_sub(self.last_check.elapsed()));
        // And, while the settings window is open, often enough to hear it: the
        // two windows have their own frames and no way to wake each other, so
        // an editor asleep on input would not take up a Save until something
        // happened to it.
        let settings = self.settings.is_some().then_some(HEAR_SETTINGS);
        // And for files another squint hands over, for the same reason.
        let handoff = crate::handoff::listening().then_some(HEAR_HANDOFF);
        [animation, files, settings, handoff]
            .into_iter()
            .flatten()
            .min()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Kept;
    use crate::recent::Recent;
    use crate::session::Session;
    use denise_ui::widgets::Pos;

    fn window(settings: Kept<Settings>) -> App {
        window_with(settings, Menus::Off)
    }

    fn window_with(settings: Kept<Settings>, menus: Menus) -> App {
        App::new(
            Size::new(900, 600),
            1.0,
            None,
            menus,
            Remembered {
                recent: Recent::in_memory(),
                settings,
                session: Kept::in_memory(Session::default()),
            },
            Present::Software,
        )
    }

    /// What the window system says the window is doing, in the order and the
    /// batches it says it in.
    fn shaped(app: &mut App, size: Size, scale: f32, at: Point, maximized: bool) {
        let mut damage = DamageTracker::new(size);
        app.update(
            &[
                InputEvent::SurfaceResized {
                    size,
                    scale_factor: scale,
                },
                InputEvent::SurfaceMoved {
                    position: at,
                    maximized,
                },
            ],
            &mut damage,
        );
    }

    /// A window is kept in the pixels that mean the same thing on the next
    /// machine: the size logical, the corner physical.
    #[test]
    fn a_window_is_remembered_as_it_is_left() {
        let mut app = window(Kept::in_memory(Settings::default()));
        shaped(
            &mut app,
            Size::new(2400, 1600),
            2.0,
            Point::new(-1920, 40),
            false,
        );
        assert_eq!(
            (app.place.width, app.place.height),
            (1200, 800),
            "half of a Retina surface is what it covers of the desk"
        );
        assert_eq!(app.place.at, Some(session::Spot { x: -1920, y: 40 }));
        assert!(!app.place.maximized);
    }

    /// Maximising is a resize to the screen and a move saying so, in that
    /// order. The screen's size is not a size to open at, so what is kept is
    /// the one the window had before — which is where it goes back to anyway.
    #[test]
    fn a_maximised_window_keeps_the_size_it_had_before() {
        let mut app = window(Kept::in_memory(Settings::default()));
        shaped(
            &mut app,
            Size::new(1200, 800),
            1.0,
            Point::new(120, 60),
            false,
        );
        shaped(&mut app, Size::new(1920, 1080), 1.0, Point::ZERO, true);
        assert!(app.place.maximized, "and it opens maximised again");
        assert_eq!((app.place.width, app.place.height), (1200, 800));
        assert_eq!(app.place.at, Some(session::Spot { x: 120, y: 60 }));

        // Un-maximised, it is back to saying what it is.
        shaped(
            &mut app,
            Size::new(1200, 800),
            1.0,
            Point::new(120, 60),
            false,
        );
        assert!(!app.place.maximized);
        assert_eq!((app.place.width, app.place.height), (1200, 800));
    }

    /// The window closes into the same file the tabs do, and opens out of it.
    #[test]
    fn the_window_is_written_down_and_asked_for_again() {
        let mut app = window(Kept::in_memory(Settings::default()));
        shaped(
            &mut app,
            Size::new(1280, 800),
            1.0,
            Point::new(64, 32),
            false,
        );
        app.exiting();
        let left = app.memory.session.get().window.expect("a window was kept");
        assert_eq!((left.width, left.height), (1280, 800));

        let config = App::config(Present::Software, Some(left));
        assert_eq!(config.size, Size::new(1280, 800));
        assert_eq!(config.position, Some(Point::new(64, 32)));
        assert!(!config.maximized);
    }

    /// Nothing kept yet is the window squint has always opened at.
    #[test]
    fn a_first_run_opens_at_the_size_it_always_did() {
        let config = App::config(Present::Software, None);
        assert_eq!(config.size, Size::new(1000, 700));
        assert_eq!(config.position, None);
        assert!(!config.maximized);
    }

    /// The window says whose it is, by the name of squint's desktop entry.
    #[test]
    fn the_window_is_known_by_the_desktop_entry() {
        let config = App::config(Present::Software, None);
        assert_eq!(config.app_id.as_deref(), Some("squint"));
    }

    /// The settings file is what the window is: its theme, its text, its tab
    /// stops and what a tab opens as.
    #[test]
    fn the_settings_file_decides_the_window() {
        let mut settings = Settings::default();
        settings.appearance.theme = "light".into();
        settings.appearance.text_size = 20;
        settings.editor.line_numbers = false;
        settings.editor.tab_width = 8;
        settings.editor.follow_editorconfig = false;
        settings.editor.read_only = true;
        settings.general.watch_seconds = 30;
        let mut app = window(Kept::in_memory(settings));
        assert_eq!(app.ui.theme().name, "light");
        assert_eq!(app.text_size, 20);
        assert!(!app.gutter);
        assert_eq!(app.tab_width_for(Some(Path::new("/tmp/a.rs"))), 8);
        assert_eq!(app.watch, Duration::from_secs(30));
        app.run(Command::New);
        assert!(app.tabs[app.active].read_only, "a tab opens read only");
    }

    /// Settings… asks the runner for a window, and only ever one: a second
    /// would be a second draft of the same file.
    #[test]
    fn settings_asks_for_one_window() {
        let mut app = window(Kept::in_memory(Settings::default()));
        app.run(Command::Settings);
        assert_eq!(app.take_windows().len(), 1);
        assert!(app.settings.is_some(), "it is open until it says otherwise");
        app.run(Command::Settings);
        assert!(app.take_windows().is_empty(), "no second window");

        // The editor is not blocked while it is open: this is Owned, not modal.
        app.run(Command::New);
        assert_eq!(app.tabs.len(), 2);

        app.settings.clone().expect("the link").say(Word::Closed);
        app.hear_settings();
        assert!(app.settings.is_none());
        app.run(Command::Settings);
        assert_eq!(app.take_windows().len(), 1, "and it opens again after");
    }

    /// What the settings window hands over is written to the file and applied.
    #[test]
    fn what_the_settings_window_hands_over_is_written_and_applied() {
        let dir = tempfile::tempdir().expect("dir");
        let file = dir.path().join("settings.json");
        let mut app = window(Kept::load_from(Some(file.clone())));
        app.run(Command::Settings);
        let link = app.settings.clone().expect("the link");

        let mut saved = Settings::default();
        saved.appearance.text_size = 22;
        saved.editor.line_numbers = false;
        link.say(Word::Apply(saved));
        app.hear_settings();

        assert_eq!(app.text_size, 22);
        assert!(!app.gutter);
        let written: Settings =
            serde_json::from_slice(&fs::read(&file).expect("written")).expect("json");
        assert_eq!(written.appearance.text_size, 22);
        assert!(!written.editor.line_numbers);
    }

    /// A theme being chosen is shown at once and put back when the window
    /// closes without saving it, which writes nothing.
    #[test]
    fn a_previewed_theme_goes_back_when_the_window_closes() {
        let dir = tempfile::tempdir().expect("dir");
        let file = dir.path().join("settings.json");
        let mut app = window(Kept::load_from(Some(file.clone())));
        app.run(Command::Settings);
        let link = app.settings.clone().expect("the link");
        assert_eq!(app.ui.theme().name, "dark");

        let mut trying = Settings::default();
        trying.appearance.theme = "light".into();
        link.say(Word::Preview(trying));
        app.hear_settings();
        assert_eq!(app.ui.theme().name, "light", "seen while it is chosen");

        link.say(Word::Closed);
        app.hear_settings();
        assert_eq!(app.ui.theme().name, "dark", "back to what it was");
        assert!(!file.exists(), "nothing was saved");
    }

    /// A bigger chrome makes the menus and tabs taller, and the text below
    /// them moves down with them.
    #[test]
    fn a_bigger_chrome_moves_the_text_down() {
        let mut app = window_with(Kept::in_memory(Settings::default()), Menus::Window);
        let (bar, strip, top) = (app.bar_h, app.strip_h, app.page_top);
        assert!(bar > 0, "the menus are in the window");
        app.memory.settings.update(|s| s.appearance.ui_size = 22);
        app.apply_settings();
        assert!(app.bar_h > bar, "the menu bar is taller");
        assert!(app.strip_h >= strip);
        assert!(app.page_top > top);
        assert_eq!(
            app.ui.bounds(app.editor_id()).map(|r| r.y),
            Some(app.page_top),
            "the text starts under them"
        );
    }

    /// The View menu's Line Numbers is the settings' line numbers.
    #[test]
    fn line_numbers_from_the_menu_are_kept() {
        let mut app = window(Kept::in_memory(Settings::default()));
        assert!(app.gutter);
        app.run(Command::LineNumbers);
        assert!(!app.gutter);
        assert!(!app.memory.settings.get().editor.line_numbers);
    }

    /// Highlighting waits for the settings to allow it, and a file bigger than
    /// they allow is never coloured.
    #[test]
    fn the_settings_decide_what_is_coloured() {
        let dir = tempfile::tempdir().expect("dir");
        let file = dir.path().join("a.json");
        fs::write(&file, "{\"a\": 1}\n").expect("write");
        let mut settings = Settings::default();
        settings.highlighting.enabled = false;
        let mut app = window(Kept::in_memory(settings));
        assert!(app.open_path(&file));
        assert!(
            !app.editor().document_mut().decide_syntax(),
            "nothing is coloured while the settings say not to"
        );
        assert!(!app.syntax_wanted(&app.editor_ref().document()));

        // Turned on, but only for files up to nothing at all: still not this one.
        app.memory.settings.update(|s| {
            s.highlighting.enabled = true;
            s.highlighting.max_mb = 1;
        });
        assert!(app.syntax_wanted(&app.editor_ref().document()));
    }

    /// ⇧⌘F puts the format in the tab it came from: the same file, the same
    /// tab, with the formatted bytes in it as unsaved changes. Saving renames
    /// the copy that was streamed beside the file onto it.
    #[test]
    fn a_format_lands_in_the_tab_it_came_from() {
        let dir = tempfile::tempdir().expect("dir");
        let file = dir.path().join("dump.json");
        fs::write(&file, r#"{"a":[1,2]}"#).expect("write");
        let mut app = window(Kept::in_memory(Settings::default()));
        assert!(app.open_path(&file));
        let tabs = app.tabs.len();

        app.format_now();
        assert_eq!(app.tabs.len(), tabs, "no tab was opened for it");
        assert_eq!(
            app.tabs[app.active].path.as_deref(),
            Some(file.as_path()),
            "the tab is still that file"
        );
        assert!(app.is_modified(app.active), "with unsaved changes in it");
        assert!(app.tab_label(app.active).ends_with('•'));
        assert_eq!(
            app.editor().document_mut().line(0).as_deref(),
            Some("{"),
            "formatted"
        );
        assert_eq!(
            fs::read_to_string(&file).expect("read"),
            r#"{"a":[1,2]}"#,
            "and nothing written to the file yet"
        );

        app.run(Command::Save);
        assert!(!app.is_modified(app.active));
        assert_eq!(
            fs::read_to_string(&file).expect("read"),
            "{\n  \"a\": [\n    1,\n    2\n  ]\n}\n"
        );
        let strays: Vec<_> = fs::read_dir(dir.path())
            .expect("dir")
            .filter_map(|e| e.ok().map(|e| e.file_name()))
            .filter(|n| n != "dump.json")
            .collect();
        assert!(strays.is_empty(), "nothing left beside it: {strays:?}");
    }

    /// A document with typing in it cannot be formatted from its file, so the
    /// format is stepped here from the pieces — and lands the same way.
    #[test]
    fn a_format_of_an_edited_document_reads_the_document() {
        let dir = tempfile::tempdir().expect("dir");
        let file = dir.path().join("dump.json");
        fs::write(&file, r#"{"a":1}"#).expect("write");
        let mut app = window(Kept::in_memory(Settings::default()));
        assert!(app.open_path(&file));
        app.editor()
            .document_mut()
            .insert(Pos::new(0, 1), r#""b":2,"#);
        assert!(app.is_modified(app.active));

        app.format_now();
        assert!(app.formatting.is_none());
        app.index_all();
        let doc = app.editor().document_mut();
        assert_eq!(doc.line(0).as_deref(), Some("{"));
        assert_eq!(doc.line(1).as_deref(), Some(r#"  "b": 2,"#));

        // And one undo takes the format back to what was typed, not to the
        // file: the typing is still there.
        assert!(app.editor().document_mut().undo());
        assert_eq!(
            app.editor().document_mut().line(0).as_deref(),
            Some(r#"{"b":2,"a":1}"#)
        );
        assert!(app.is_modified(app.active));
    }
}
