//! The window: a text area over the file, and a status line under it.
//!
//! The file's line index is built here, in slices between frames, rather
//! than on a thread: the document lives inside the widget, the slices are a
//! few milliseconds each, and the window keeps drawing and taking keys while
//! the count climbs in the status line.

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
use std::path::Path;
use std::time::{Duration, Instant};

/// Bytes indexed per frame: a few milliseconds from the page cache, and the
/// window is drawn between every two.
const INDEX_SLICE: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Msg {
    Changed,
    Clipboard(ClipboardRequest),
    /// Enter in the go-to-line field.
    GoTo,
}

pub struct App {
    ui: Ui<Msg>,
    editor: NodeId,
    status: NodeId,
    /// The go-to-line field, while it is open. It takes the status line's
    /// place, and the status line comes back when it closes.
    goto: Option<NodeId>,
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
        ui.set_anchors(
            editor,
            Anchors {
                left: true,
                top: true,
                right: true,
                bottom: true,
            },
        );
        let status = ui
            .add(
                root,
                Label::new("").with_size(px(11.0)),
                Rect::new(s(8), h - status_h, w - s(16), status_h),
            )
            .expect("status");
        ui.set_anchors(
            status,
            Anchors {
                left: true,
                top: false,
                right: true,
                bottom: true,
            },
        );
        ui.focus(Some(editor));

        let mut app = Self {
            ui,
            editor,
            status,
            goto: None,
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
        self.notice = format!("line {line}");
        self.refresh_status();
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
        let deadline = Instant::now() + Duration::from_millis(6);
        let doc = self.editor().document_mut();
        while !doc.index_step(INDEX_SLICE) && Instant::now() < deadline {}
        self.refresh_status();
    }

    /// Opens the go-to-line field over the status line, or refocuses it.
    fn open_goto(&mut self) {
        if let Some(field) = self.goto {
            self.ui.focus(Some(field));
            return;
        }
        let Some(row) = self.ui.bounds(self.status) else {
            return;
        };
        let px = |v: f32| (v * self.scale + 0.5) as u16;
        let s = |v: i32| (v as f32 * self.scale + 0.5) as i32;
        let root = self.ui.root();
        let field = self.ui.add(
            root,
            TextInput::<Msg>::new()
                .with_placeholder("Go to line — Enter to jump, Esc to cancel")
                .with_submit(Msg::GoTo)
                .with_max_chars(12)
                .with_size(px(11.0)),
            Rect::new(row.x, row.y + s(2), row.width.min(s(360)), row.height - s(4)),
        );
        let Some(field) = field else {
            return;
        };
        self.ui.set_visible(self.status, false);
        self.ui.focus(Some(field));
        self.goto = Some(field);
    }

    fn close_goto(&mut self) {
        if let Some(field) = self.goto.take() {
            self.ui.remove(field);
            self.ui.set_visible(self.status, true);
            self.ui.focus(Some(self.editor));
        }
    }

    /// Enter in the go-to-line field: jump, or say why not.
    fn go_to(&mut self) {
        let text = self
            .goto
            .and_then(|f| self.ui.widget::<TextInput<Msg>>(f))
            .map(|f| f.text().trim().to_string())
            .unwrap_or_default();
        self.close_goto();
        match text.parse::<usize>() {
            Ok(n) if n > 0 => {
                self.editor().go_to(n - 1);
                self.notice = format!("line {n}");
            }
            _ => self.notice = format!("not a line number: {text:?}"),
        }
        self.refresh_status();
    }

    fn save(&mut self) {
        let result = self.editor().document_mut().save();
        self.notice = match result {
            Ok(()) => "saved".into(),
            Err(e) => e,
        };
        self.refresh_status();
    }

    fn refresh_status(&mut self) {
        let editor = self.editor();
        let caret = editor.caret();
        let modified = editor.document().is_modified();
        let mut doc = editor.document_mut();
        let error = doc.take_error();
        let (scanned, total) = doc.index_progress();
        let lines = if doc.is_indexed() {
            let n = squint_core_lines(&mut doc);
            format!("{n} lines")
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
            Msg::Changed => {}
            Msg::GoTo => self.go_to(),
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
}

/// The document's line count, once it is known.
fn squint_core_lines(doc: &mut FileDocument) -> usize {
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
                } if self.goto.is_some() => {
                    self.close_goto();
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
                        KeyCode::L => {
                            self.open_goto();
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
        if !self.editor_ref().document().is_indexed() {
            // Straight back: there is a slice of the file to count.
            return Some(Duration::ZERO);
        }
        let now = self.started.elapsed().as_millis() as u64;
        self.ui
            .next_wake_ms()
            .map(|w| Duration::from_millis(w.saturating_sub(now)))
    }
}
