//! squint — a text editor for files too big for text editors.
//!
//! One window drawn by DeniseUI, the document engine from `squint-core`
//! underneath, and no webview.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod document;
mod fonts;

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
    if args.first().map(String::as_str) == Some("--snapshot") {
        // `squint --snapshot out.ppm [scale] [file]`: one frame, no window.
        // How a layout is reviewed over SSH and how the README's pictures
        // are made.
        let out = args.get(1).cloned().unwrap_or_else(|| "squint.ppm".into());
        let scale: f32 = args.get(2).and_then(|a| a.parse().ok()).unwrap_or(2.0);
        return snapshot(&out, scale, args.get(3).map(PathBuf::from));
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
    let open_path = path.clone();
    let open = move |size, scale| app::App::new(size, scale, open_path.as_deref());
    match run_with(app::App::config(present), open) {
        Err(Error::Gpu(reason) | Error::Present(reason)) if present == Present::Gpu => {
            eprintln!("squint: cannot draw through the GPU ({reason}); drawing in software");
            let open = move |size, scale| app::App::new(size, scale, path.as_deref());
            run_with(app::App::config(Present::Software), open)?;
            Ok(())
        }
        Err(e) => Err(e.into()),
        Ok(()) => Ok(()),
    }
}

/// Draws one frame into a PPM file, with no window and no event loop.
fn snapshot(out: &str, scale: f32, path: Option<PathBuf>) -> Result<(), Box<dyn std::error::Error>> {
    use denise::{BufferAge, Frame, PixelFormat, Size};
    use std::io::Write as _;

    let logical = app::App::config(Present::Software).size;
    let size = Size::new(
        (logical.width as f32 * scale + 0.5) as u32,
        (logical.height as f32 * scale + 0.5) as u32,
    );
    let mut app = app::App::new(size, scale, path.as_deref());
    app.index_all();
    let mut pixels = vec![0u32; (size.width * size.height) as usize];
    {
        let mut frame = Frame::new(
            &mut pixels,
            size,
            size.width,
            PixelFormat::Xrgb8888,
            BufferAge::Undefined,
        )
        .expect("frame");
        app.paint_into(&mut frame);
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
