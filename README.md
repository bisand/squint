# squint

A text editor for files too big for text editors. It opens a file of any size
instantly, holds almost none of it in memory, shows it with syntax
highlighting, and lets you tweak it and save. It is not trying to be your code
editor; it is the thing you reach for when a 4 GB log, a minified JSON dump or
a database export needs looking at, and the editor you love says "loading".

squint is a sibling of [ctail](https://github.com/bisand/ctail) and borrows
its engine ideas: read the file only where it is looked at, keep a sparse line
index instead of the lines, and show the file before it has been counted.

## How it works

- **The file is never loaded.** The document is a piece table: the original
  file on disk is one buffer, an append-only add buffer holds what you type,
  and the document is a list of pieces pointing into either. Reading a line
  reads that line, with `pread`. Undo restores an earlier piece list.
- **Lines are found through a sparse index:** the byte offset of every
  thousandth line, 8 bytes per thousand lines, built in the background. The
  first screen shows at once; the line count and the scrollbar's true extent
  arrive when the scan finishes, and everything the scan has passed is
  readable and editable before then.
- **Saving is whole-file and atomic:** the pieces stream to a temporary file
  beside the original, which is renamed into place. Inserting in the middle
  of a file shifts every byte after it, so there is no partial save; a full
  write at disk speed is the honest option and the safe one.
- **Formatting is a stream.** Pretty-printing minified JSON or XML never
  builds a tree: a tokenizer streams from the source to a formatted file,
  which then opens the normal way. Memory stays flat however big the input.
- **Highlighting is line-local** for logs, JSON, XML and CSV, and grammar
  based with state snapshots every few thousand lines for code, the same trick
  as the line index.

## Layout

- [`core/`](core/) — the engine as a plain Rust crate: `Source` (file or
  memory), `LineIndex`, `Document`. No UI, no threads of its own; a front end
  drives the index scan in slices from a worker.
- [`desktop/`](desktop/) — the app: one window drawn by
  [DeniseUI](https://github.com/bisand/denise), its `TextArea` widget editing
  the engine's document through the toolkit's `TextDocument` trait, so the
  widget never learns how big the file is. The line index is built in slices
  between frames while the status line counts up. Copy, cut and paste go
  through the system clipboard.

| Keys (⌘ on macOS, Ctrl elsewhere) | |
|---|---|
| ⌘S | Save |
| ⌘F | Find. Enter for the next match, Shift+Enter for the previous, Esc to close |
| ⌘G / ⇧⌘G, F3 / ⇧F3 | Next / previous match of the last search, with the field closed |
| ⌘L | Go to line |
| ⌘Z / ⇧⌘Z | Undo / redo |

Find walks the file in slices between frames, like the index, so a search
through gigabytes keeps the window drawing and shows how far it has got. A
query with no capital letters ignores ASCII case.

```bash
cargo run --release -- /var/log/system.log
cargo run --release -- --time /var/log/system.log        # no window: how long the parts take
cargo run --release -- --snapshot out.ppm 2 some.log 1 error   # no window: one frame at 2x, at the first "error"
```

## Roadmap

1. An open dialog, and every match on screen highlighted rather than only the
   selected one.
2. IME composition forwarded from winit into the text area.
3. Streaming pretty-printers for JSON and XML.
4. Highlighting: line-local rules first, then `syntect` with state snapshots,
   through the widget's `spans` hook.
5. Conflict detection when the file changes underneath.
6. Long lines: a single-line multi-gigabyte JSON breaks line-based paging and
   needs chunked lines.

## License

MIT, see [LICENSE](LICENSE).
