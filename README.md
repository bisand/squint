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
  The layout follows the project's [EditorConfig](https://editorconfig.org):
  `indent_style`, `indent_size`, `tab_width`, `end_of_line` and
  `insert_final_newline`, from the `.editorconfig` files above the file being
  formatted (above the output file, for `--format` with an output path). With
  none, it is two spaces, LF and a final newline. The editor's tab stops come
  from the same place: `tab_width`, or a numeric `indent_size`, and every four
  columns otherwise.
- **Highlighting is syntect's**, with the Sublime Text grammars `bat` uses and
  the `base16-ocean.dark` theme. The grammars load on a thread, and only for a
  file that has one: a log never pays for them. A grammar carries state from
  line to line, so the state is kept every 512 lines by a parse from the top
  in the background, the same trick as the line index. A line that parse has
  not reached is coloured from 256 lines above it, which is right unless a
  comment or string opened further up, and is put right when the parse gets
  there. Files over 16 MB are only ever coloured that way, and a line longer
  than 16 KB is left uncoloured.

## Layout

- [`core/`](core/) — the engine as a plain Rust crate: `Source` (file or
  memory), `LineIndex`, `Document`. No UI, no threads of its own; a front end
  drives the index scan in slices from a worker.
- [`desktop/`](desktop/) — the app: one window drawn by
  [DeniseUI](https://github.com/bisand/denise), its `TextArea` widget editing
  the engine's document through the toolkit's `TextDocument` trait, so the
  widget never learns how big the file is. The line index is built in slices
  between frames while the status line counts up. Copy, cut and paste go
  through the system clipboard. The menus — File, Edit, View, Tools, Help,
  and the application and Window menus on macOS — are the system's menu bar
  on macOS and DeniseUI's `MenuBar` along the top of the window everywhere
  else (`SQUINT_MENU=window` puts them in the window on macOS too). Both are
  built from one list in [`desktop/src/menu.rs`](desktop/src/menu.rs), so they
  offer the same commands with the same keys. Opening, saving and the
  unsaved-changes question use the platform's own dialogs, and Open Recent is
  kept in the user's configuration directory.

| Keys (⌘ on macOS, Ctrl elsewhere) | |
|---|---|
| ⌘N / ⌘O | New file / open a file |
| ⌘S / ⇧⌘S | Save / save as |
| ⌘W, ⌘Q | Close the window / quit, asking first about unsaved changes |
| ⌘F | Find. Enter for the next match, Shift+Enter for the previous, Esc to close |
| ⌘G / ⇧⌘G, F3 / ⇧F3 | Next / previous match of the last search, with the field closed |
| ⌘L | Go to line |
| ⇧⌘F | Format JSON or XML into a new file, and open it |
| ⌘Z / ⇧⌘Z | Undo / redo; Ctrl+Y redoes too, off macOS |
| ⌘= / ⌘- / ⌘0 | Bigger / smaller / the usual text size |
| F10 | Into the menu bar in the window, off macOS |

Find walks the file in slices between frames, like the index, so a search
through gigabytes keeps the window drawing and shows how far it has got. A
query with no capital letters ignores ASCII case. While the find field is
open, every match on the lines on screen is marked as you type, by the same
rules; the one a search landed on is selected over the marks.

```bash
cargo run --release -- /var/log/system.log
cargo run --release -- --time /var/log/system.log        # no window: how long the parts take
cargo run --release -- --snapshot out.ppm 2 some.log 1 error   # no window: one frame at 2x, at the first "error"
cargo run --release -- --snapshot menu.ppm 2 some.log --menu File   # no window: with the File menu open in the window
cargo run --release -- --format dump.json pretty.json          # no window: pretty-print to a file, or to stdout
```

## Roadmap

1. IME composition forwarded from winit into the text area.
2. Highlighting for logs, which syntect has no grammar for: ctail's line-local
   rules, through the same `spans` hook.
3. Conflict detection when the file changes underneath.
4. More than one window: New and Open replace the file in this one.
5. Long lines: a single-line multi-gigabyte JSON still has to be formatted
   before it can be paged through; chunked lines would let it be read as is.

## License

MIT, see [LICENSE](LICENSE).
