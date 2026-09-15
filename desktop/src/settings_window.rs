//! The settings window: the form in a window of its own, beside the editor's.
//!
//! Where every desktop keeps its settings, and what DeniseUI's
//! [`Modality::Owned`] is for — the window belongs to the editor, stays above
//! it and closes with it, and the editor keeps taking input the whole time, so
//! a theme can be tried against the file it will be read in.
//!
//! The two windows are two [`DeniseApp`]s in one process and share no tree, so
//! they speak through a [`Link`]: the settings window says what it has done,
//! the editor drains that once a frame. Everything the settings window says is
//! about the settings; it knows nothing else about the editor.

use crate::fonts;
use crate::settings::Settings;
use crate::settings_form::{Form, FormMsg, Outcome};
use denise::{BufferAge, DamageTracker, ElementState, Frame, InputEvent, KeyCode, Pen, Rect, Size};
use denise_text::TextStyle;
use denise_ui::Ui;
use denise_winit::{DeniseApp, Modality, Present, WindowConfig, WindowRequest};
use rfd::FileDialog;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The window's size in logical pixels: the form's rows and the list of
/// sections, with room for the longest line about a setting.
pub const SIZE: Size = Size::new(760, 600);

/// What the settings window tells the editor.
#[derive(Clone, Debug, PartialEq)]
pub enum Word {
    /// Take these settings up and write them: Save or Apply.
    Apply(Settings),
    /// Draw in this theme without keeping it, while it is being chosen.
    Preview(Settings),
    /// Open the settings file in a tab.
    EditFile,
    /// The window has gone. Whatever a preview changed goes back to what the
    /// settings say, which after a Save is what was just saved.
    Closed,
}

/// What the settings window says to the editor: a queue one pushes and the
/// other drains once a frame.
///
/// A queue rather than a shared `Settings` both windows write: what the form
/// does is a sequence of things — apply, then open the file, then close — and
/// the last value alone cannot say that. A poisoned lock loses words rather
/// than taking the editor down with it; the settings are on disk either way.
#[derive(Default)]
pub struct Link {
    said: Mutex<Vec<Word>>,
}

impl Link {
    /// Says one thing to the editor, to be heard on its next frame.
    pub fn say(&self, word: Word) {
        if let Ok(mut said) = self.said.lock() {
            said.push(word);
        }
    }

    /// Everything said since the last time, in order.
    pub fn drain(&self) -> Vec<Word> {
        self.said
            .lock()
            .map(|mut said| std::mem::take(&mut *said))
            .unwrap_or_default()
    }
}

pub struct SettingsWindow {
    ui: Ui<FormMsg>,
    form: Form,
    link: Arc<Link>,
    started: Instant,
    exit: bool,
}

impl SettingsWindow {
    /// The window to open for these settings, beside the editor's own.
    pub fn request(
        settings: Settings,
        file: Option<PathBuf>,
        link: Arc<Link>,
        present: Present,
    ) -> WindowRequest {
        let config = WindowConfig {
            title: "Settings".into(),
            size: SIZE,
            present,
            app_id: Some(crate::app::APP_ID.into()),
            ..WindowConfig::default()
        };
        WindowRequest::new(config, move |size, scale| {
            SettingsWindow::new(size, scale, settings, file, link)
        })
        .with_modality(Modality::Owned)
    }

    /// The window, once the surface it draws to is known. Drawn in the
    /// settings' own theme and face, like the editor: a window that ignored
    /// them while they are being chosen would be the odd one out.
    pub fn new(
        size: Size,
        scale: f32,
        settings: Settings,
        file: Option<PathBuf>,
        link: Arc<Link>,
    ) -> Self {
        let mut ui: Ui<FormMsg> = Ui::new(size, settings.theme().scaled(scale));
        ui.show_cursor(false);
        let ui_px = (settings.appearance.ui_size as f32 * scale + 0.5) as u16;
        let face = settings
            .appearance
            .ui_font
            .as_deref()
            .and_then(fonts::load_named)
            .or_else(|| fonts::load(fonts::UI));
        let chrome = match face {
            Some((_, source)) => {
                let font = ui.add_font(source);
                ui.set_default_font(font);
                TextStyle {
                    font,
                    size_px: ui_px,
                }
            }
            None => TextStyle::built_in(ui_px),
        };
        let form = Form::build_in(&mut ui, &settings, file, scale, chrome).expect("the form");
        Self {
            ui,
            form,
            link,
            started: Instant::now(),
            exit: false,
        }
    }

    /// Shows the section of that name. For a snapshot. Whether there is one.
    pub fn show_section(&mut self, name: &str) -> bool {
        self.form.show_section(name, &mut self.ui)
    }

    /// Draws the window into `frame`, without an event loop. For a snapshot.
    pub fn paint_into(&mut self, frame: &mut Frame<'_>) {
        self.ui.paint(frame);
    }

    /// Acts on something the form said, and passes on what the editor has to
    /// know about.
    fn act(&mut self, msg: FormMsg) {
        let before = self.form.draft().appearance.clone();
        let outcome = self.form.handle(msg, &mut self.ui);
        if self.form.draft().appearance != before {
            // A theme being chosen is shown in the editor as well as here, and
            // is not kept until Save or Apply.
            self.link.say(Word::Preview(self.form.draft().clone()));
        }
        match outcome {
            None => {}
            Some(Outcome::Apply) => self.applied(),
            Some(Outcome::Save) => {
                self.applied();
                self.exit = true;
            }
            // Closing says `Closed` on the way out, which puts back whatever a
            // preview changed.
            Some(Outcome::Close) => self.exit = true,
            Some(Outcome::EditFile) => {
                self.applied();
                self.link.say(Word::EditFile);
                self.exit = true;
            }
            Some(Outcome::PickFont { text }) => self.pick_font(text),
        }
    }

    fn applied(&mut self) {
        self.link.say(Word::Apply(self.form.draft().clone()));
    }

    /// Picks a TrueType file for the text, or for the chrome. The whole path
    /// is what is kept: a face chosen from a folder squint does not look in
    /// has nothing else to be called.
    fn pick_font(&mut self, text: bool) {
        let title = if text {
            "A face for the text"
        } else {
            "A face for the menus, tabs and status line"
        };
        let picked = FileDialog::new()
            .set_title(title)
            .add_filter("TrueType fonts", &["ttf"])
            .pick_file();
        if let Some(path) = picked {
            self.form
                .set_font(&mut self.ui, text, path.display().to_string());
        }
    }
}

impl DeniseApp for SettingsWindow {
    fn update(&mut self, events: &[InputEvent], damage: &mut DamageTracker) {
        let mut forwarded: Vec<InputEvent> = Vec::with_capacity(events.len());
        for event in events {
            match event {
                // The runner closes the window; `exiting` says so.
                InputEvent::CloseRequested => continue,
                // Escape is Cancel, unless a dropdown is open — that is the
                // tree's to close.
                InputEvent::Key {
                    code: KeyCode::Escape,
                    state: ElementState::Down,
                    ..
                } if !self.ui.popup_open() => {
                    self.exit = true;
                    continue;
                }
                _ => {}
            }
            forwarded.push(event.clone());
        }
        self.ui.handle(&forwarded);
        self.ui.tick(self.started.elapsed().as_millis() as u64);
        // A form message can make the tree say something else — a section
        // picked builds rows that report themselves — so it drains until it
        // is quiet, and never for ever.
        for _ in 0..8 {
            let said: Vec<FormMsg> = self.ui.drain_messages().collect();
            if said.is_empty() {
                break;
            }
            for msg in said {
                self.act(msg);
            }
        }
        self.form.poll(&mut self.ui);
        self.form.resized(&mut self.ui);
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

    /// Every way out reaches here — Save, Cancel, the close button, the editor
    /// closing under it — and the editor has to hear about all of them: a
    /// preview left behind would be a theme nobody chose.
    fn exiting(&mut self) {
        self.link.say(Word::Closed);
    }

    fn next_frame_in(&self) -> Option<Duration> {
        let now = self.started.elapsed().as_millis() as u64;
        let animation = self
            .ui
            .next_wake_ms()
            .map(|wake| Duration::from_millis(wake.saturating_sub(now)));
        // A change to the defaults finishes on its thread, which cannot wake
        // the window: it looks until it has.
        let defaults = self.form.busy().then_some(Duration::from_millis(100));
        [animation, defaults].into_iter().flatten().min()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings_form::Action;

    fn window(settings: Settings) -> (SettingsWindow, Arc<Link>) {
        let link = Arc::new(Link::default());
        let form = SettingsWindow::new(SIZE, 1.0, settings, None, Arc::clone(&link));
        (form, link)
    }

    /// Save hands the settings to the editor and closes the window; the way
    /// out says so, so a preview cannot be left behind.
    #[test]
    fn save_hands_the_settings_over_and_closes() {
        let (mut window, link) = window(Settings::default());
        window.form.draft_mut().appearance.text_size = 21;
        window.act(FormMsg::Button(Action::Save));
        assert!(window.exit_requested());
        window.exiting();
        let said = link.drain();
        assert_eq!(said.len(), 2, "{said:?}");
        match &said[0] {
            Word::Apply(settings) => assert_eq!(settings.appearance.text_size, 21),
            other => panic!("expected the settings, got {other:?}"),
        }
        assert_eq!(said[1], Word::Closed);
    }

    /// The settings window belongs to squint as far as the desktop can tell,
    /// not to an application of its own.
    #[test]
    fn the_settings_window_is_squints() {
        let request = SettingsWindow::request(
            Settings::default(),
            None,
            Arc::new(Link::default()),
            Present::Software,
        );
        assert_eq!(request.config.app_id.as_deref(), Some(crate::app::APP_ID));
    }

    /// Cancel hands nothing over, and closing puts the editor back.
    #[test]
    fn cancel_hands_nothing_over() {
        let (mut window, link) = window(Settings::default());
        window.act(FormMsg::Button(Action::Cancel));
        assert!(window.exit_requested());
        window.exiting();
        assert_eq!(link.drain(), [Word::Closed]);
    }

    /// A theme chosen is shown in the editor too, before it is kept.
    #[test]
    fn a_theme_being_chosen_reaches_the_editor() {
        let (mut window, link) = window(Settings::default());
        window.show_section("Appearance");
        link.drain();
        window.act(FormMsg::Button(Action::NewTheme));
        let said = link.drain();
        match said.first() {
            Some(Word::Preview(settings)) => assert_eq!(settings.appearance.theme, "custom"),
            other => panic!("expected a preview, got {other:?}"),
        }
        assert!(!window.exit_requested(), "choosing a theme closes nothing");
    }

    /// Edit the File… writes the settings first, then asks for the file.
    #[test]
    fn editing_the_file_saves_first() {
        let (mut window, link) = window(Settings::default());
        window.act(FormMsg::Button(Action::EditFile));
        let said = link.drain();
        assert!(matches!(said.first(), Some(Word::Apply(_))), "{said:?}");
        assert_eq!(said.get(1), Some(&Word::EditFile));
        assert!(window.exit_requested());
    }
}
