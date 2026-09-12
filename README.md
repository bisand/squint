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
  write at disk speed is the honest option and the safe one. A format not
  typed in since is already such a file, so its save is the rename alone.
- **A file changed underneath is noticed.** The document reads its file where
  it is looked at, so a file something else rewrites is not merely out of
  date: what is on screen could become a mix of the two. Every couple of
  seconds each tab's file is looked at — its size, modification time and
  inode — and one that has changed asks whether to reload it. With Ask Before
  Reloading off in the settings, a tab with no unsaved changes reloads
  quietly; one with changes still asks.
- **Formatting is a stream, into the tab it came from.** Pretty-printing
  minified JSON or XML never builds a tree: a tokenizer streams from the
  source to a copy of the file beside it, and the tab takes that copy up as
  what it holds — the same file, the same tab, now with unsaved changes in
  it. Memory stays flat however big the input, and one undo takes the format
  back. A file with nothing typed in it is formatted on a thread of its own,
  at the speed of the disk: 400 MB of minified JSON in about a second. One
  with edits in it is stepped between frames instead, because only this
  process's piece table has those bytes. Saving renames the copy onto the
  file, so a format is written once however big it is; typing after the
  format falls back to the ordinary save, which streams the pieces.
  The layout follows the project's [EditorConfig](https://editorconfig.org):
  `indent_style`, `indent_size`, `tab_width`, `end_of_line` and
  `insert_final_newline`, from the `.editorconfig` files above the file being
  formatted (above the output file, for `--format` with an output path). With
  none, it is two spaces, LF and a final newline. The editor's tab stops come
  from the same place: `tab_width`, or a numeric `indent_size`, and every four
  columns otherwise.
- **Highlighting is syntect's**, with the Sublime Text grammars `bat` uses and
  the `base16-ocean.dark` theme, or any other syntect theme the settings name.
  The grammars load on a thread, and only for a file that has one: a log never
  pays for them. A grammar carries state from line to line, so the state is
  kept every 512 lines by a parse from the top in the background, the same
  trick as the line index. A line that parse has
  not reached is coloured from 256 lines above it, which is right unless a
  comment or string opened further up, and is put right when the parse gets
  there. Files over 16 MB are only ever coloured that way. What colouring
  costs does not grow with the file — only what is drawn is parsed — but it
  does grow with the length of a line, so the reach around a line drawn is
  held to 64 KB of text as well as to those 256 lines, by the document's own
  average line: a file whose lines are kilobytes long is coloured from a few
  lines above rather than from 256, which would be megabytes of syntect
  inside a paint. A line longer than 16 KB is left uncoloured altogether.
- **The text is drawn in Fira Code's Nerd Font**, where the machine has one:
  squint looks for the monospaced build of it first — in the places fonts
  live for everybody and in this user's own font folder — and falls back
  through the platform's usual fixed faces (SF Mono, Menlo, Monaco, DejaVu
  Sans Mono, Consolas, Courier) to Denise's built-in bitmap font. The
  settings name another, for the text and for the chrome separately.

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
  kept in the user's configuration directory. Files open in tabs — DeniseUI's
  `Tabs`, which close, drag into order, rename on a double click and take a
  colour from the menu a right click opens — and the tabs open when squint
  closes open again when it starts, at the lines they were on. A tab behind
  the others does no work until it comes to the front. What squint has been
  told to do — themes, faces, tab stops, what is watched, what is coloured —
  is one JSON file ([`settings.rs`](desktop/src/settings.rs)) edited in a
  window of its own ([`settings_form.rs`](desktop/src/settings_form.rs) and
  [`settings_window.rs`](desktop/src/settings_window.rs)), and see below.

| Keys (⌘ on macOS, Ctrl elsewhere) | |
|---|---|
| ⌘N or ⌘T / ⌘O | New tab / open a file in a tab |
| ⌘S / ⇧⌘S | Save / save as |
| ⌘W, ⌘Q | Close the tab / quit, asking first about unsaved changes |
| ⌘F | Find. Enter for the next match, Shift+Enter for the previous, Esc to close |
| ⌘G / ⇧⌘G, F3 / ⇧F3 | Next / previous match of the last search, with the field closed |
| ⌘L | Go to line |
| ⇧⌘F | Format JSON or XML in the tab it is in; ⌘Z takes it back |
| ⌘Z / ⇧⌘Z | Undo / redo; Ctrl+Y redoes too, off macOS |
| ⌘= / ⌘- / ⌘0 | Bigger / smaller / the usual text size |
| ⌘, | Settings |
| F10 | Into the menu bar in the window, off macOS |
| Ctrl+Tab | The last tab; with Ctrl held, on along the row, backwards with Shift. The control key on macOS too |

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
cargo run --release -- --snapshot tabs.ppm 2 --session tabs.json    # no window: with the tabs a session file lists
cargo run --release -- --snapshot set.ppm 2 --settings Appearance   # no window: the settings window, at a section
cargo run --release -- --format dump.json pretty.json          # no window: pretty-print to a file, or to stdout
```

## Settings

Everything squint can be told is in one JSON file —
`settings.json` in the user's configuration directory
(`~/Library/Application Support/squint` on macOS, `~/.config/squint`
elsewhere). The file is the settings' real home: it can be edited by hand, a
key it does not mention is that setting's default, a value out of range is
brought back into it, and anything squint cannot read at all is the defaults.

Settings… (⌘, — in the application menu on macOS, under Tools everywhere else)
is an editor for that file, a section at a time, in a window of its own: it is
DeniseUI's `Modality::Owned`, so it stays above the editor and closes with it
while the editor keeps taking input, and a theme can be tried against the file
it will be read in. Save writes the file and applies it, Apply does both and
stays open, Cancel drops what was typed — as does closing the window — and
Edit the File… opens `settings.json` itself in a tab.

The two windows are two applications in one process with no tree between them,
so they speak through a small queue: the settings window says what it has done
and the editor takes it up on its next frame. The settings it edits are the
ones it opened with, so what Save writes is what it showed.

- **General** — reopening the tabs at launch and at the line they were on,
  whether a file takes the empty tab or always a new one, how many recent
  files are kept, and how often (and whether) the tabs' files are looked at
  for changes made by something else.
- **Editor** — line numbers, tab stops and whether they come from the file's
  project's `.editorconfig`, and whether files open read only.
- **Appearance** — the theme, the face the text is drawn in and its size, and
  the face and size of the menus, tabs and status line, which are chosen
  separately from the text's. Each face dropdown offers the faces squint
  reaches for by itself first, then every face installed on the machine, in
  order — the list scrolls — and Choose a File… takes any TrueType or
  OpenType file, wherever it is. Themes are DeniseUI's `dark`, `light` and
  `high-contrast`, plus any number written in the settings file: New Theme or
  Duplicate makes one, and its name, whether it is light or dark and its nine
  seed colours are edited here — shown as they are chosen, and put back by
  Cancel. The faces are the settings', not a theme's: a theme is colours.
- **Highlighting** — whether syntect colours the text, which of its themes it
  colours with, and the size past which a file is left alone. That size is a
  gigabyte and it is there for taste rather than for speed: colouring a file
  costs what is on screen, whatever the file weighs.
- **Formatting** — what ⇧⌘F writes: whether the source's project's
  `.editorconfig` decides the layout, and the indent, line breaks and final
  newline used where it does not.

Opening a *file* never opens a second window — squint is one editor window
with a row of tabs, and more than one is the roadmap's third item rather than
a setting.

## Roadmap

1. IME composition forwarded from winit into the text area.
2. Highlighting for logs, which syntect has no grammar for: ctail's line-local
   rules, through the same `spans` hook.
3. More than one window, and tabs dragged from one to another.
4. Long lines: a single-line multi-gigabyte JSON still has to be formatted
   before it can be paged through; chunked lines would let it be read as is.

## License

MIT, see [LICENSE](LICENSE).
