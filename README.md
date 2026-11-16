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
| `--port <PORT>` | Listen on `PORT` (default 7878). `--port 0` picks any free port and prints it, which suits scripts and agents running several servers. Port values use ASCII decimal digits without a sign or leading zeros. The old `--serve 8080 notes.md` positional form is rejected. |
| `--recurse` | Discover Markdown files at any depth. Takes a single directory; hidden and symlinked directories are skipped. |
| `--annotate` | Enable adding and changing review comments in the browser. |
| `--open` | Open the default browser once the server is ready. |
| `--daemon` | Run in the background and return after the child is ready, printing the PID and URL. Logs go to `.markview-serve-<PORT>.log` in the current directory. |

`--port` and `--recurse` also work with `--review`. `--open`, `--daemon`, and `--annotate` require `--serve`.

Served pages use a top bar, a document navigation column on the left and, with review enabled, a Comments / Outline column on the right. The top bar's Light / Auto / Dark toggle overrides the system theme and is remembered in the browser; Auto follows the system.

The server renders Markdown as HTML and live-reloads open browser tabs whenever a served file changes (a file watcher feeds a `/events` server-sent-events stream). It serves the listed documents and assets allowlisted by relative Markdown image/link destinations (`![]()` and `[]()`, including reference-style links). Referenced assets inside hidden directories are allowed, except anything under a `.markview` directory: the whole tree is blocked, so annotation stores and their lock files are never served as assets. Raw HTML `<img src>` and absolute destinations do not add assets; those files return 404 unless independently allowlisted. `/favicon.ico` has a separate local-file/built-in fallback. Other files return 404. It binds to `127.0.0.1`; to view it from another machine, forward the port over SSH first, e.g. `ssh -L 8080:localhost:8080 user@host`.

For directory collections, `/` chooses the top-level `README.md`, then `index.md` (case-insensitive), then the first relative Markdown path sorted by byte order. Explicit file sets use their first FILE argument. A one-document directory has an empty navigation area. Hidden files/directories and symlinked directories are excluded from document discovery, while aliases of already discovered files are silently dropped. Explicit file sets reject duplicate/aliased inputs.

On daemon startup failure, read the log path printed by the launcher with `cat`, report that underlying error, and remove the failed log. Daemon children retain their concrete `--port` in process arguments; foreground processes may omit it or show `0`, so read their startup banner for the actual port. Unix daemons inherit the bound listener, keeping the selected port reserved throughout startup.

### Request a collection verdict

```sh
markview --review notes.md api.md
markview --review --recurse --gate docs/
markview --review --no-open --port 8080 notes.md
```

`--review` opens an annotatable browser session and waits in the foreground.
The URL goes to stderr. The reviewer can **Approve** or **Request changes**
for the entire served collection. Request changes requires at least one
saved open comment. Approval can include open comments.

Once the reviewer confirms, stdout contains the verdict and saved open
comments from that collection, including comments saved before the session.
Both verdicts exit 0. With `--gate`, Request changes exits 2.
Ctrl+C cancels a pending review. Use `--annotations` to recover saved
comments if a session ends without a verdict.

The default port is ephemeral. For SSH, use `--no-open` with a fixed
`--port` and forward the same port on both sides, for example
`ssh -L 8080:localhost:8080 user@host`.

### Review comments

```sh
markview --serve --annotate --open notes.md
markview --annotations notes.md docs/
markview --annotations --include-resolved notes.md
```

| Option | Effect |
| --- | --- |
| `--annotate` | With `--serve`, enable adding and changing review comments in the browser. Without it, existing comments are shown read-only. |
| `--annotations` | List open comments for the given Markdown files or directories as Markdown, with current line numbers. It is a mode of its own and cannot be combined with `--serve`, `--html` or `--width`. |
| `--include-resolved` | With `--annotations`, also list resolved comments, after the open ones. |

Select rendered text and click **Comment** or press Ctrl/Cmd+Shift+K, or
choose **Comment on document**. A selection can hold at most 1,000 characters.
Use Cmd/Ctrl+Enter to save the comment and Escape to cancel it; Escape asks
first if the draft has text. Choose **Edit**, **Resolve**, **Reopen**, or **Delete**
on a card; **Delete** asks for confirmation in the card. Existing comments are
available for read-only review with `--serve` alone. In macOS Preview, comments
share the table-of-contents sidebar, which opens when you start a comment. Use
**View → Add Comment** (Cmd+Shift+K) on a selection. Save a dirty or untitled
tab before adding a comment.
New comments have four quick labels. **Question**, **Needs a test**,
**Out of scope**, and **Nit** save a blank draft immediately or prefix
a draft that already has text.

Single-key shortcuts are on by default in the browser and macOS Preview.
Use the **Single-key shortcuts** toggle in the annotations panel to turn
them off. The browser remembers the choice for its current origin. The
macOS app saves it in its preferences.

| Key | Action |
| --- | --- |
| `j` / `k` | Focus the next or previous visible annotation, wrapping at the ends. |
| `c` | Comment on the selection or, with no selection, on the document. |
| `e` | Edit the focused comment. |
| `r` | Resolve or reopen the focused comment. |
| `Escape` | Cancel the composer or clear the focused annotation. |

The browser and GUI update when the shared Markdown store changes. A comment
whose text is missing or ambiguous stays visible as **Detached** with its
last-known lines. It reattaches when Markview finds a unique match again.

Markview canonicalizes the document path and picks the nearest ancestor
with a `.markview/annotations/` directory. Otherwise it uses the nearest
ancestor with a `.git` entry, or the document's own directory outside Git.
Paths mirror the source tree below that root. For example, repository file
`docs/notes.md` maps to `.markview/annotations/docs/notes.md`. Commit
`.markview/` to share the Markdown comments, or add `.markview/` to
`.gitignore` for local review. Exclude the generated `*.markview-lock` files
if you commit comments. Store files are created with ordinary permissions
(your umask applies) and keep their mode when Markview rewrites them.

`/_markview/` is a reserved route prefix, like `/events`. A directory named
`_markview` at the top of a served directory is not served; Markview prints a
warning naming it at startup. The annotation endpoint under that prefix
answers only when the page is opened as `localhost:<port>` or
`127.0.0.1:<port>`. Opened through any other host name, the page is read-only
and says where to open it instead.

Agents can run `markview --annotations <FILE or DIR>...` to see open comments
and current lines: text comments in document order, then whole-document
comments, then detached ones. Each document's output names its store file,
relative to the working directory when the store is below it. A document that
cannot be listed is reported on stderr as `error: <document>: <problem>`; the
others are still listed and the command exits non-zero. After addressing a
comment, change its header from `[open]` to `[resolved]` in the store. A
section header must stay exactly `## a<N> [open|resolved] <anchor>`: a
different case, an extra space or a trailing space turns it into comment text,
which the listing warns about. Markview locks its own writes and
compares the on-disk file immediately before committing. A direct edit can
still race between that comparison and the final rename or removal. Re-read
the store immediately before editing it and verify the intended change after
saving.

### Alerts and code copy

Start a blockquote with `[!NOTE]`, `[!TIP]`, `[!IMPORTANT]`, `[!WARNING]`, or
`[!CAUTION]` to render a GitHub alert. The terminal shows a bold title, and
HTML uses a titled border that stays visible in print.

In the browser and macOS Preview, hover over a code block or Tab to its
**Copy** button to copy the displayed code, including indentation and the
trailing newline. On touch devices, the button is always visible.

### Diff blocks

Use a fenced code block with a case-insensitive label: `diff`, `patch`, or
`udiff`. Diff row colours are always on in the GUI,
`--html` output, and `--serve` pages, and follow the light or dark theme.
The terminal renderer shows diff code without row colouring. Only Rust keyword
highlighting requires the `highlight` Cargo feature.

Added lines (`+`) are green, removed lines (`-`) are red, and hunk headers
(`@@`) are blue. File and git preamble headers use neutral, bold text, including
mode changes and rename/copy metadata. After a hunk header, comment lines such
as `--- removed comment` are deletions, while ordinary context stays uncoloured.
Format-patch separators and signatures also stay uncoloured.

````markdown
```diff
--- a/example.txt
+++ b/example.txt
@@ -1 +1 @@
-old text
+new text
```
````

### macOS GUI

```sh
cargo run --features gui --bin markview-gui -- README.md docs/notes.md
```

With no files, the GUI opens piped stdin as an untitled tab, or restores the previous session's tabs.

The GUI renders Markdown through the system WebKit view and includes:

- Toolbar actions for Open, Edit/Preview, Save, Refresh, Print, sidebar visibility, auto-refresh, theme selection, recent files, and find-in-document.
- A per-tab Markdown editor: toggle a tab into a plain-text editor (Cmd+E), edit the raw source, and toggle back to a live preview. Unsaved tabs show a dirty indicator; Cmd+S (or File > Save) writes the active tab back to its file, prompting for a destination if the tab has none yet.
- Right-click a tab and choose Edit to open that document's editor, or Copy Path to copy its full canonical file path (the resolved target for a symlink). Copy Path is disabled for untitled documents.
- The editor highlights Markdown syntax, shows spelling squiggles on macOS for prose and link labels, excluding code, link destinations, bare URLs, autolinks, reference definitions, and HTML markup/blocks, and offers a collapsible Markdown syntax reference beside the source. The editor soft-wraps within the available window space and has no separate resize handle. Standard Cut, Copy, Paste, Undo, and Redo shortcuts are available.
- Tabs for multiple open documents, including per-tab close buttons and overflow scrolling.
- Relative links to Markdown files open in a new tab, resolved beside the linking document. Links from unsaved untitled documents show an error instead.
- External `http` and `https` links ask for confirmation (Cancel or Open in Browser) before opening in the default browser.
- Markdown document registration for bundled `.app` builds.
- A sidebar with the table of contents and review comments for file-backed Preview tabs.
- Auto-refresh for file-backed tabs when files change on disk, with a modified indicator when auto-refresh is disabled or a tab has unsaved edits.
- Scroll preservation when switching tabs or refreshing content.
- Drag-and-drop opening for `.md`, `.markdown`, and `.mdown` files.
- Local preferences for theme, sidebar visibility, auto-refresh, window size, recent files, and restoring the last open files.

Preferences are stored locally in `~/Library/Application Support/markview/preferences.conf` on macOS.

Run `markview --help` or `markview-gui --help` for the same option summary in the terminal.

## Agent skill

The repo ships a `markview` agent skill ([skills/markview/SKILL.md](skills/markview/SKILL.md)) that teaches a coding agent to request a human verdict or serve a Markdown directory as a browsable local site. For ordinary serving, it installs the CLI if missing, checks for servers already running, starts `markview --serve --recurse --daemon --port 0` from `/tmp/.markview`, checks the URL, and reports how to stop the server.

Install the published repository version with the [skills CLI](https://github.com/vercel-labs/skills):

```sh
npx skills add baldwinmatt/markview
```

The skills CLI installs into the current project by default; add `--global` for a user-wide installation. Unpushed changes in this checkout are available through the repository install command only after they are pushed.

To use this checkout immediately, copy its contents by hand, for example for Claude Code:

```sh
mkdir -p ~/.claude/skills/markview
cp -R skills/markview/. ~/.claude/skills/markview/
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

`make test` runs both Rust test configurations, `node --test tests/editor.test.cjs` and `node --test tests/review.test.cjs`. The review tests run the shared annotation script against a small stub DOM: text normalisation, offset-to-range conversion, selection capture, drafts, confirmation prompts, messages and focus. The editor tests cover source preservation, tokenizer edge cases, UTF-16 spelling marks, revision checks, and shifting marks through edits. Rust GUI tests cover IPC parsing, model editing, canonical Copy Path targets, parser-based spelling exclusions, and native spelling ranges. Real WKWebView checks on 2026-10-19 verified reused-editor focus, initial caret position, matching wrapped textarea/overlay geometry with tabs/CJK/emoji, available-space layout, input refresh, and Find exclusions. These checks do not establish a large-document performance budget.

Every commit should be reviewed before starting the next task:

1. Commit the focused change.
2. Run the review workflow described in `code-reviewer.md` against that commit's diff.
3. Write the review output as `review-<identifier>.md`.
4. Address all valid critical and major findings immediately.
5. Amend the original commit with the fixes before proceeding.

Minor findings may be fixed immediately, explicitly deferred in the review file, or turned into follow-up work.
