//! squint's front end. For now a placeholder that opens a file through the
//! engine and reports how long the parts take, so the open-time and memory
//! promise can be measured on real files before there is a window to look
//! at them in. The DeniseUI app replaces this once the toolkit has a text
//! area to build it on.

use squint_core::Document;
use std::path::Path;
use std::time::Instant;

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: squint <file>");
        std::process::exit(2);
    };
    let path = Path::new(&path);

    let t = Instant::now();
    let mut doc = match Document::open(path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("squint: {}: {e}", path.display());
            std::process::exit(1);
        }
    };
    let opened = t.elapsed();

    let t = Instant::now();
    let first: Vec<String> = (0..10)
        .map_while(|n| doc.line(n).ok().flatten())
        .collect();
    let first_lines = t.elapsed();

    let t = Instant::now();
    if let Err(e) = doc.index_complete() {
        eprintln!("squint: indexing {}: {e}", path.display());
        std::process::exit(1);
    }
    let indexed = t.elapsed();
    let lines = doc.line_count().ok().flatten().unwrap_or(0);

    println!("{}: {} bytes", path.display(), doc.len());
    println!("open        {opened:>10.3?}");
    println!("first lines {first_lines:>10.3?}");
    println!("index       {indexed:>10.3?}  ({lines} lines, {} KB held)", doc.memory_bytes() / 1024);
    for (n, line) in first.iter().enumerate() {
        println!("{:>4}  {line}", n + 1);
    }
}
