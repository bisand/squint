//! The window: a text area over the file, and a status line under it.
//!
//! Work that walks the file — building the line index, finding text — is
//! done here in slices between frames rather than on a thread: the document
//! lives inside the widget, a slice is a few milliseconds, and the window
//! keeps drawing and taking keys while the status line counts up.

use crate::document::FileDocument;
use crate::fonts;
use denise::{
    BufferAge, DamageTracker, ElementState, Frame, InputEvent, KeyCode, Modifiers, Pen, Rect,
    Size, theme,
};
use denise_text::TextStyle;
use denise_ui::widgets::{ClipboardRequest, Label, TextArea, TextInput};
use denise_ui::{Anchors, NodeId, Ui};
use denise_winit::{DeniseApp, Present, WindowConfig};
use squint_core::{Find, FindStep};
use std::path::Path;
use std::time::{Duration, Instant};

/// Bytes indexed per step: a few milliseconds from the page cache.
const INDEX_SLICE: usize = 8 * 1024 * 1024;

/// Bytes searched per step.
const FIND_SLICE: usize = 4 * 1024 * 1024;

/// How long one frame may spend walking the file, indexing or finding.
const SLICE_TIME: Duration = Duration::from_millis(6);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Msg {
    Changed,
    Clipboard(ClipboardRequest),
    /// Enter in the prompt's field.
    Submit,
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

/// A find on its way through the file.
struct Search {
    find: Find,
    query: String,
    /// Where it matched and whether it wrapped, once it has, while the index
    /// catches up far enough to say which line that is.
    hit: Option<(u64, bool)>,
}

pub struct App {
    ui: Ui<Msg>,
    editor: NodeId,
    status: NodeId,
    prompt: Option<Prompt>,
    search: Option<Search>,
    /// The last thing searched for, so ⌘G and F3 have something to find with
    /// the field closed, and the field opens holding it.
    last_query: String,
    scale: f32,
    title: String,
    /// What the status line says on the right: the last thing that happened.
    notice: String,
    clipboard: Option<arboard::Clipboard>,
    started: Instant,
    exit: bool,
}

impl App {
    pub fn config(present: Present) -> WindowConfig {
        WindowConfig {
            title: "squint".into(),
            size: Size::new(1000, 700),
            present,
            ..WindowConfig::default()
        }
    }

    pub fn new(size: Size, scale: f32, path: Option<&Path>) -> Self {
        let px = |v: f32| (v * scale + 0.5) as u16;
        let s = |v: i32| (v as f32 * scale + 0.5) as i32;
        let mut ui: Ui<Msg> = Ui::new(size, theme::DARK.scaled(scale));
        ui.show_cursor(false);
        if let Some((_, source)) = fonts::load(fonts::UI) {
            let id = ui.add_font(source);
            ui.set_default_font(id);
        }
        let mono = match fonts::load(fonts::MONO) {
            Some((_, source)) => TextStyle {
                font: ui.add_font(source),
                size_px: px(13.0),
            },
            None => TextStyle::built_in(px(13.0)),
        };

        let (doc, mut notice) = match path {
            Some(path) => match FileDocument::open(path) {
                Ok(doc) => (doc, String::new()),
                Err(e) => (FileDocument::empty(), format!("{}: {e}", path.display())),
            },
            None => (FileDocument::empty(), String::new()),
        };
        let title = match doc.path() {
            Some(p) => format!("{} — squint", name_of(p)),
            None => "squint".into(),
        };
        if notice.is_empty() && doc.path().is_none() {
            notice = "no file: squint <file>".into();
        }

        let root = ui.root();
        let (w, h) = (size.width as i32, size.height as i32);
        let status_h = s(24);
        let editor = ui
            .add(
                root,
                TextArea::new(doc)
                    .with_style(mono)
                    .with_change(Msg::Changed)
                    .with_clipboard(Msg::Clipboard),
                Rect::new(0, 0, w, h - status_h),
            )
            .expect("editor");
        ui.set_anchors(editor, ALL_SIDES);
        let status = ui
            .add(
                root,
                Label::new("").with_size(px(11.0)),
                Rect::new(s(8), h - status_h, w - s(16), status_h),
            )
            .expect("status");
        ui.set_anchors(status, BOTTOM_ROW);
        ui.focus(Some(editor));

        let mut app = Self {
            ui,
            editor,
            status,
            prompt: None,
            search: None,
            last_query: String::new(),
            scale,
            title,
            notice,
            clipboard: arboard::Clipboard::new().ok(),
            started: Instant::now(),
            exit: false,
        };
        app.refresh_status();
        app
    }

    /// Indexes the whole file now. For a snapshot, which has no frames to
    /// spread it over.
    pub fn index_all(&mut self) {
        while !self.editor_ref().document().is_indexed() {
            self.pump_index();
        }
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
        self.find(true);
        while self.search.is_some() {
            self.pump_find();
        }
    }

    fn editor(&mut self) -> &mut TextArea<Msg, FileDocument> {
        self.ui
            .widget_mut::<TextArea<Msg, FileDocument>>(self.editor)
            .expect("editor")
    }

    fn editor_ref(&self) -> &TextArea<Msg, FileDocument> {
        self.ui
            .widget::<TextArea<Msg, FileDocument>>(self.editor)
            .expect("editor")
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
            Ask::GoTo => ("Go to line — Enter to jump, Esc to close", 12, String::new()),
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
            self.ui.focus(Some(self.editor));
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
        let editor = self.editor();
        let from = match editor.selection() {
            Some((start, end)) => {
                if forward {
                    end
                } else {
                    start
                }
            }
            None => editor.caret(),
        };
        let doc = editor.document_mut();
        let Some(origin) = doc.offset_of(from) else {
            self.say("the caret's line has not been counted yet".into());
            return;
        };
        let find = doc.find(&query, origin, forward);
        self.search = Some(Search {
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
        if search.hit.is_none() {
            let deadline = Instant::now() + SLICE_TIME;
            let doc = self.editor().document_mut();
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
        if !self.editor_ref().document().is_indexed() {
            self.say("found — counting lines to reach it…".into());
            self.search = Some(search);
            return;
        }
        let end = at + search.query.len() as u64;
        let doc = self.editor().document_mut();
        let (from, to) = (doc.pos_of(at), doc.pos_of(end));
        match (from, to) {
            (Some(from), Some(to)) => {
                self.editor().select_range(from, to);
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

    // ---- the rest -----------------------------------------------------------

    fn save(&mut self) {
        let result = self.editor().document_mut().save();
        self.say(match result {
            Ok(()) => "saved".into(),
            Err(e) => e,
        });
    }

    fn refresh_status(&mut self) {
        let editor = self.editor();
        let caret = editor.caret();
        let modified = editor.document().is_modified();
        let doc = editor.document_mut();
        let error = doc.take_error();
        let (scanned, total) = doc.index_progress();
        let lines = if doc.is_indexed() {
            format!("{} lines", line_count(doc))
        } else {
            let percent = if total == 0 { 100 } else { scanned * 100 / total };
            format!("counting… {percent}%")
        };
        let held = doc.memory_bytes() / 1024;
        if let Some(error) = error {
            self.notice = error;
        }
        let mark = if modified { " •" } else { "" };
        let text = format!(
            "Ln {}, Col {}   {lines}   {held} KB held{mark}   {}",
            caret.line + 1,
            caret.col + 1,
            self.notice
        );
        if let Some(label) = self.ui.widget_mut::<Label>(self.status) {
            label.set_text(text);
        }
        let base = self.title.trim_start_matches('•').trim_start().to_string();
        self.title = if modified { format!("• {base}") } else { base };
    }

    fn handle(&mut self, msg: Msg) {
        match msg {
            // The offsets a search has covered moved with the edit.
            Msg::Changed => self.search = None,
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
        }
    }

    /// Whether the find field has the keyboard.
    fn typing_a_find(&self) -> bool {
        self.prompt
            .as_ref()
            .is_some_and(|p| p.ask == Ask::Find && self.ui.focused() == Some(p.field))
    }
}

const ALL_SIDES: Anchors = Anchors {
    left: true,
    top: true,
    right: true,
    bottom: true,
};

const BOTTOM_ROW: Anchors = Anchors {
    left: true,
    top: false,
    right: true,
    bottom: true,
};

/// The document's line count, once it is known.
fn line_count(doc: &mut FileDocument) -> usize {
    use denise_ui::widgets::TextDocument;
    doc.line_count().unwrap_or(0)
}

fn name_of(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

impl DeniseApp for App {
    fn update(&mut self, events: &[InputEvent], damage: &mut DamageTracker) {
        let mut forwarded: Vec<InputEvent> = Vec::with_capacity(events.len());
        for event in events {
            match event {
                InputEvent::CloseRequested => {
                    self.exit = true;
                    continue;
                }
                InputEvent::Key {
                    code: KeyCode::Escape,
                    state: ElementState::Down,
                    ..
                } if self.prompt.is_some() => {
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
                InputEvent::Key {
                    code: KeyCode::F3,
                    state: ElementState::Down,
                    modifiers,
                    ..
                } => {
                    self.find(!modifiers.contains(Modifiers::SHIFT));
                    continue;
                }
                InputEvent::Key {
                    code,
                    state: ElementState::Down,
                    modifiers,
                    ..
                } if modifiers.contains(Modifiers::SUPER) || modifiers.contains(Modifiers::CTRL) => {
                    match code {
                        KeyCode::S => {
                            self.save();
                            continue;
                        }
                        KeyCode::F => {
                            self.open_prompt(Ask::Find);
                            continue;
                        }
                        KeyCode::G => {
                            self.find(!modifiers.contains(Modifiers::SHIFT));
                            continue;
                        }
                        KeyCode::L => {
                            self.open_prompt(Ask::GoTo);
                            continue;
                        }
                        KeyCode::Q => {
                            self.exit = true;
                            continue;
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
            forwarded.push(event.clone());
        }
        self.ui.handle(&forwarded);
        self.ui.tick(self.started.elapsed().as_millis() as u64);
        let messages: Vec<Msg> = self.ui.drain_messages().collect();
        for msg in messages {
            self.handle(msg);
        }
        self.pump_index();
        self.pump_find();
        if !forwarded.is_empty() {
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

    fn exit_requested(&self) -> bool {
        self.exit
    }

    fn title(&self) -> Option<&str> {
        Some(&self.title)
    }

    fn next_frame_in(&self) -> Option<Duration> {
        if !self.editor_ref().document().is_indexed() || self.search.is_some() {
            // Straight back: there is a slice of the file to walk.
            return Some(Duration::ZERO);
        }
        let now = self.started.elapsed().as_millis() as u64;
        self.ui
            .next_wake_ms()
            .map(|w| Duration::from_millis(w.saturating_sub(now)))
    }
}
