# markview 

`markview` is a small Markdown viewer written in idiomatic Rust. It keeps Markdown parsing and frontend rendering separate, so the same core can target a terminal, a native macOS WebKit window, or another renderer later.

## Install locally

```sh
cargo install --path .
```

To build the macOS GUI binary:

```sh
cargo build --features gui --bin markview-gui
```

## Usage

```sh
markview README.md
cat README.md | markview --no-color
markview --width 72 notes.md
markview --html README.md > README.html

cargo run --features gui --bin markview-gui -- README.md docs/notes.md
```

The GUI renders Markdown through the system WebKit view and includes:

- Toolbar actions for Open, Refresh, Print, sidebar visibility, auto-refresh, theme selection, recent files, and find-in-document.
- Tabs for multiple open documents, including per-tab close buttons and overflow scrolling.
- A table-of-contents sidebar generated from document headings.
- Auto-refresh for file-backed tabs when files change on disk, with a modified indicator when auto-refresh is disabled.
- Scroll preservation when switching tabs or refreshing content.
- External `http` and `https` links opened in the default browser.
- Drag-and-drop opening for `.md`, `.markdown`, and `.mdown` files.
- Local preferences for theme, sidebar visibility, auto-refresh, window size, recent files, and restoring the last open files.

Preferences are stored locally in `~/Library/Application Support/markview/preferences.conf` on macOS.

## Design

- Pluggable frontends: `FrontendRenderer` separates document input from terminal and HTML output.
- Proper GUI rendering: the GUI path renders Markdown to HTML/CSS and displays it in the system WebKit WebView on macOS, with toolbar actions and tabs handled through a small Rust/WebView bridge.
- Portable core: parsing and rendering live in the library; GUI dependencies are optional behind the `gui` feature.
- Fast enough for local viewing: the terminal renderer is single-pass, and the GUI uses the OS web engine instead of embedding a browser runtime.
- Tested: terminal rendering, HTML rendering, app tab/refresh state, CLI behavior, and frontend substitution are covered by unit and integration tests.

## Development workflow

Every commit should be reviewed before starting the next task:

1. Commit the focused change.
2. Run the review workflow described in `code-reviewer.md` against that commit's diff.
3. Write the review output as `review-<identifier>.md`.
4. Address all valid critical and major findings immediately.
5. Amend the original commit with the fixes before proceeding.

Minor findings may be fixed immediately, explicitly deferred in the review file, or turned into follow-up work.
