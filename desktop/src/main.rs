//! squint — a text editor for files too big for text editors.
//!
//! One window drawn by DeniseUI, the document engine from `squint-core`
//! underneath, and no webview.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod config;
mod document;
mod fonts;
mod menu;
#[cfg(target_os = "macos")]
mod native_menu;
mod recent;
mod session;
mod settings;
mod settings_form;
mod stamp;
mod switcher;

use denise_winit::{Error, Present, run_with};
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if matches!(args.first().map(String::as_str), Some("--version" | "-V")) {
        println!("squint {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if args.first().map(String::as_str) == Some("--time") {
        // `squint --time <file>`: no window, just how long the parts take.
        return time(args.get(1).map(PathBuf::from));
    }
    if args.first().map(String::as_str) == Some("--format") {
        // `squint --format <file> [out]`: no window, the file pretty-printed
        // as a stream into `out`, or to stdout.
        return format_file(
            args.get(1).map(PathBuf::from),
            args.get(2).map(PathBuf::from),
        );
    }
    if args.first().map(String::as_str) == Some("--snapshot") {
        // `squint --snapshot out.ppm [scale] [file] [line] [find] [--formatted]
        // [--menu <title>] [--session <file>] [--settings [section]]`: one
        // frame, no window, scrolled to `line` and then to the first match of
        // `find` after it; with `--formatted`, of the file as ⇧⌘F formats it;
        // with `--menu`, with that menu open from the menu bar in the window;
        // with `--session`, with the tabs a session file lists, which is read
        // and never written; with `--settings`, with the settings dialog open
        // at that section, over the settings `--settings-file` names, which is
        // also read and never written. How a layout is reviewed over SSH and
        // how the README's pictures are made.
        let formatted = args.iter().any(|a| a == "--formatted");
        let value_of = |flag: &str| {
            args.iter()
                .position(|a| a == flag)
                .and_then(|i| args.get(i + 1))
                .cloned()
        };
        let menu = value_of("--menu");
        let session = value_of("--session").map(PathBuf::from);
        let settings_file = value_of("--settings-file").map(PathBuf::from);
        // `--settings` alone is the first section; `--settings Appearance` is
        // that one. The section is not a value when what follows is a flag.
        let settings = args
            .iter()
            .position(|a| a == "--settings")
            .map(|i| args.get(i + 1).filter(|a| !a.starts_with("--")).cloned());
        let settings_section = settings.clone().flatten();
        let mut value = false;
        let args: Vec<&str> = args
            .iter()
            .map(String::as_str)
            .filter(|a| {
                if std::mem::take(&mut value) {
                    return false;
                }
                if Some(a.to_string()) == settings_section {
                    return false;
                }
                value = matches!(*a, "--menu" | "--session" | "--settings-file");
                !value && *a != "--formatted" && *a != "--settings"
            })
            .collect();
        let out = args.get(1).copied().unwrap_or("squint.ppm");
        let scale: f32 = args.get(2).and_then(|a| a.parse().ok()).unwrap_or(2.0);
        let path = args.get(3).map(PathBuf::from);
        let line: Option<usize> = args.get(4).and_then(|a| a.parse().ok());
        let query = args.get(5).map(|a| a.to_string());
        return snapshot(
            out,
            scale,
            path,
            formatted,
            line,
            query,
            menu,
            session,
            settings,
            settings_file,
        );
    }
    let path: Option<PathBuf> = args
        .iter()
        .find(|a| !a.starts_with('-'))
        .map(|a| std::fs::canonicalize(a).unwrap_or_else(|_| PathBuf::from(a)));

    // The GPU paces frames to the display; the software rasteriser is the
    // fallback for a machine with nothing that can present, and an override
    // so the two can be compared.
    let present = match std::env::var("SQUINT_PRESENT").as_deref() {
        Ok("software") => Present::Software,
        _ => Present::Gpu,
    };
    let menus = app::Menus::for_platform();
    let open_path = path.clone();
    let open = move |size, scale| {
        app::App::new(
            size,
            scale,
            open_path.as_deref(),
            menus,
            app::Remembered::load(),
        )
    };
    match run_with(app::App::config(present), open) {
        Err(Error::Gpu(reason) | Error::Present(reason)) if present == Present::Gpu => {
            eprintln!("squint: cannot draw through the GPU ({reason}); drawing in software");
            let open = move |size, scale| {
                app::App::new(size, scale, path.as_deref(), menus, app::Remembered::load())
            };
            run_with(app::App::config(Present::Software), open)?;
            Ok(())
        }
        Err(e) => Err(e.into()),
        Ok(()) => Ok(()),
    }
}

/// Draws one frame into a PPM file, with no window and no event loop.
#[allow(clippy::too_many_arguments)]
fn snapshot(
    out: &str,
    scale: f32,
    path: Option<PathBuf>,
    formatted: bool,
    line: Option<usize>,
    query: Option<String>,
    menu: Option<String>,
    session: Option<PathBuf>,
    settings: Option<Option<String>>,
    settings_file: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    use denise::{BufferAge, Frame, PixelFormat, Size};
    use std::io::Write as _;

    let logical = app::App::config(Present::Software).size;
    let size = Size::new(
        (logical.width as f32 * scale + 0.5) as u32,
        (logical.height as f32 * scale + 0.5) as u32,
    );
    // The window as it is drawn: the system's menus are not in it, so a
    // snapshot where they are the system's has none, unless one is asked for.
    let menus = match app::Menus::for_platform() {
        app::Menus::System if menu.is_none() => app::Menus::Off,
        _ => app::Menus::Window,
    };
    let mut memory = app::Remembered::in_memory();
    if let Some(file) = settings_file {
        let saved: config::Kept<settings::Settings> = config::Kept::load_from(Some(file));
        memory.settings = config::Kept::in_memory(saved.get().clone());
    }
    if let Some(file) = session {
        // Read, and kept in memory: a snapshot leaves the file as it found it.
        let saved: config::Kept<session::Session> = config::Kept::load_from(Some(file));
        memory.session = config::Kept::in_memory(saved.get().clone());
    }
    let mut app = app::App::new(size, scale, path.as_deref(), menus, memory);
    if formatted {
        app.format_now();
    }
    app.index_all();
    app.highlight_now();
    let mut pixels = vec![0u32; (size.width * size.height) as usize];
    let mut paint = |app: &mut app::App| {
        let mut frame = Frame::new(
            &mut pixels,
            size,
            size.width,
            PixelFormat::Xrgb8888,
            BufferAge::Undefined,
        )
        .expect("frame");
        app.paint_into(&mut frame);
    };
    paint(&mut app);
    if let Some(line) = line {
        // A jump centres on the rows the last paint saw, so it comes after
        // one; then the frame is drawn again where it landed.
        app.go_to_line(line);
        paint(&mut app);
    }
    if let Some(query) = query {
        // The widget scrolls sideways to a match when it next paints, so
        // the frame is drawn once to settle that and once to be kept.
        app.find_now(&query);
        paint(&mut app);
        paint(&mut app);
    }
    if let Some(title) = menu {
        if !app.open_menu_titled(&title) {
            eprintln!("squint: no menu called {title:?}");
        }
        paint(&mut app);
    }
    if let Some(section) = settings {
        app.open_settings_now(section.as_deref());
        paint(&mut app);
    }
    let mut file = std::io::BufWriter::new(std::fs::File::create(out)?);
    write!(file, "P6\n{} {}\n255\n", size.width, size.height)?;
    for word in &pixels {
        file.write_all(&[(word >> 16) as u8, (word >> 8) as u8, *word as u8])?;
    }
    file.flush()?;
    eprintln!("wrote {out} at {}x{}", size.width, size.height);
    Ok(())
}

/// Pretty-prints a JSON or XML file as a stream, into a file or to stdout.
fn format_file(
    input: Option<PathBuf>,
    output: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    use squint_core::Document;
    use squint_core::editorconfig;
    use squint_core::format::{self, Style};
    use std::io::Write as _;
    use std::time::Instant;

    let Some(input) = input else {
        eprintln!("usage: squint --format <file> [out]");
        std::process::exit(2);
    };
    let doc = Document::open(&input)?;
    let head = doc.read(0, 8192)?;
    let Some(kind) = format::detect(Some(&input), &head) else {
        eprintln!("squint: {}: not JSON or XML", input.display());
        std::process::exit(1);
    };
    let started = Instant::now();
    let mut w: Box<dyn std::io::Write> = match &output {
        Some(out) => Box::new(std::io::BufWriter::with_capacity(
            1 << 20,
            std::fs::File::create(out)?,
        )),
        None => Box::new(std::io::BufWriter::with_capacity(
            1 << 20,
            std::io::stdout().lock(),
        )),
    };
    // Laid out as the project the output goes into asks, or the input's when
    // it goes to stdout.
    let props = editorconfig::properties_for(output.as_deref().unwrap_or(&input));
    let style = Style::from_editorconfig(&props);
    format::format_document(&doc, kind, style, &mut w)?;
    w.flush()?;
    let from = props
        .sources()
        .first()
        .map(|file| format!(" from {}", file.display()))
        .unwrap_or_default();
    eprintln!(
        "squint: formatted {} MB of {} with {}{from} in {:.2?}",
        doc.len() / 1_000_000,
        kind.name(),
        style.describe(),
        started.elapsed()
    );
    Ok(())
}

/// Opens a file through the engine and reports how long the parts take.
fn time(path: Option<PathBuf>) -> Result<(), Box<dyn std::error::Error>> {
    use squint_core::Document;
    use std::time::Instant;

    let Some(path) = path else {
        eprintln!("usage: squint --time <file>");
        std::process::exit(2);
    };
    let t = Instant::now();
    let mut doc = Document::open(&path)?;
    let opened = t.elapsed();
    let t = Instant::now();
    let first: Vec<String> = (0..10).map_while(|n| doc.line(n).ok().flatten()).collect();
    let first_lines = t.elapsed();
    let t = Instant::now();
    doc.index_complete()?;
    let indexed = t.elapsed();
    let lines = doc.line_count()?.unwrap_or(0);
    println!("{}: {} bytes", path.display(), doc.len());
    println!("open        {opened:>10.3?}");
    println!("first lines {first_lines:>10.3?}");
    println!(
        "index       {indexed:>10.3?}  ({lines} lines, {} KB held)",
        doc.memory_bytes() / 1024
    );
    for (n, line) in first.iter().enumerate() {
        println!("{:>4}  {line}", n + 1);
    }
    Ok(())
}
