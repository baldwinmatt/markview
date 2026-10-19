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

To create a local macOS app bundle around the Cargo-built GUI binary:

```sh
sh packaging/macos/bundle.sh
open target/macos/Markview.app
```

To create a local release zip:

```sh
make package-macos
```

## Usage

### Terminal and HTML

```sh
markview README.md
cat README.md | markview --no-color
markview --width 72 notes.md
markview --html README.md > README.html
```

| Option | Effect |
| --- | --- |
| `--html` | Print a complete HTML document instead of terminal output. |
| `-w`, `--width <COLUMNS>` | Wrap terminal text at `COLUMNS` (minimum 20, default 88). |
| `--no-color` | Drop ANSI colors, keep bold text attributes. |

### Local server

```sh
markview --serve notes.md                    # http://localhost:7878
markview --serve notes.md api.md             # explicit doc set with navigation
markview --serve docs/                       # top-level Markdown files in docs/
markview --serve --recurse docs/             # docs/ and every Markdown file below it
markview --serve notes.md --port 8080        # specific port
markview --serve docs/ --port 0              # any free port, printed on start
markview --serve --open --daemon docs/       # background server, browser opened
```

| Option | Effect |
| --- | --- |
| `--serve` | Serve the given files, or a directory's Markdown files, over HTTP with navigation between documents. |
| `--port <PORT>` | Listen on `PORT` (default 7878). `--port 0` picks any free port and prints it, which suits scripts and agents that run several servers. The old `--serve 8080 notes.md` positional form is rejected. |
| `--recurse` | Discover Markdown files at any depth. Takes a single directory; hidden and symlinked directories are skipped. |
| `--open` | Open the default browser once the server is ready. |
| `--daemon` | Run in the background and return immediately, printing the PID and URL. Logs go to `.markview-serve-<PORT>.log` in the current directory. |

`--port`, `--recurse`, `--open`, and `--daemon` require `--serve`.

The server renders Markdown as HTML and live-reloads open browser tabs whenever a served file changes (a file watcher feeds a `/events` server-sent-events stream). It serves only the listed documents and the assets they reference; everything else returns 404. It binds to `127.0.0.1`; to view it from another machine, forward the port over SSH first, e.g. `ssh -L 8080:localhost:8080 user@host`.

### macOS GUI

```sh
cargo run --features gui --bin markview-gui -- README.md docs/notes.md
```

With no files, the GUI opens piped stdin as an untitled tab, or restores the previous session's tabs.

The GUI renders Markdown through the system WebKit view and includes:

- Toolbar actions for Open, Edit/Preview, Save, Refresh, Print, sidebar visibility, auto-refresh, theme selection, recent files, and find-in-document.
- A per-tab Markdown editor: toggle a tab into a plain-text editor (Cmd+E), edit the raw source, and toggle back to a live preview. Unsaved tabs show a dirty indicator; Cmd+S (or File > Save) writes the active tab back to its file, prompting for a destination if the tab has none yet.
- Right-click a tab and choose Edit to open that document's editor, or Copy Path to copy its full file path. Copy Path is disabled for untitled documents.
- The editor highlights Markdown syntax, shows spelling squiggles on macOS (excluding code and links), and offers a collapsible Markdown syntax reference beside the source. Standard Cut, Copy, Paste, Undo, and Redo shortcuts are available.
- Tabs for multiple open documents, including per-tab close buttons and overflow scrolling.
- Relative links to Markdown files open in a new tab, resolved beside the linking document. Links from unsaved untitled documents show an error instead.
- External `http` and `https` links ask for confirmation (Cancel or Open in Browser) before opening in the default browser.
- Markdown document registration for bundled `.app` builds.
- A table-of-contents sidebar generated from document headings.
- Auto-refresh for file-backed tabs when files change on disk, with a modified indicator when auto-refresh is disabled or a tab has unsaved edits.
- Scroll preservation when switching tabs or refreshing content.
- Drag-and-drop opening for `.md`, `.markdown`, and `.mdown` files.
- Local preferences for theme, sidebar visibility, auto-refresh, window size, recent files, and restoring the last open files.

Preferences are stored locally in `~/Library/Application Support/markview/preferences.conf` on macOS.

Run `markview --help` or `markview-gui --help` for the same option summary in the terminal.

## Agent skill

The repo ships a `markview` agent skill ([skills/markview/SKILL.md](skills/markview/SKILL.md)) that teaches a coding agent to serve a Markdown directory as a browsable local site: it installs the CLI if missing, checks for servers already running, starts `markview --serve --recurse --daemon --port 0` from `/tmp/.markview`, checks the URL, and reports how to stop the server.

Install it with the [skills CLI](https://github.com/vercel-labs/skills):

```sh
npx skills add baldwinmatt/markview
```

Or copy the folder by hand, for example for Claude Code:

```sh
cp -R skills/markview ~/.claude/skills/markview
```

## Design

- Pluggable frontends: `FrontendRenderer` separates document input from terminal and HTML output.
- Shared rendered model: `RenderedDocument` exposes HTML body content and heading navigation data for alternate frontend engines.
- Proper GUI rendering: the GUI path renders Markdown to HTML/CSS and displays it in the system WebKit WebView on macOS, with toolbar actions and tabs handled through a small Rust/WebView bridge.
- Portable core: parsing and rendering live in the library; GUI dependencies are optional behind the `gui` feature.
- Fast enough for local viewing: the terminal renderer is single-pass, and the GUI uses the OS web engine instead of embedding a browser runtime.
- Local app bundle: `packaging/macos/bundle.sh` wraps the Cargo-built GUI binary in `target/macos/Markview.app` with app metadata, a small icon, and Markdown document registration.
- Local packaging: `make package-macos` creates `target/dist/markview-<version>-macos.zip` from a release app bundle.
- CI-ready: GitHub Actions runs formatting, clippy, CLI/core tests, and macOS GUI feature checks.
- Release-ready: pushing a `v*` tag packages the macOS app and publishes a GitHub release with the zip attached.
- Tested: terminal rendering, HTML rendering, app tab/refresh state, CLI behavior, and frontend substitution are covered by unit and integration tests.

For a deeper map of the core types, GUI flow, persistence, packaging, and release process, see [ARCHITECTURE.md](ARCHITECTURE.md).

## Development workflow

Editor highlighting and spelling-response checks can be run with `node --test tests/editor.test.cjs`, alongside the Rust GUI tests.

Every commit should be reviewed before starting the next task:

1. Commit the focused change.
2. Run the review workflow described in `code-reviewer.md` against that commit's diff.
3. Write the review output as `review-<identifier>.md`.
4. Address all valid critical and major findings immediately.
5. Amend the original commit with the fixes before proceeding.

Minor findings may be fixed immediately, explicitly deferred in the review file, or turned into follow-up work.
