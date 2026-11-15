# Markview Architecture

Markview is split into a portable Markdown core, a terminal frontend, and an optional macOS GUI frontend. The main rule is that parsing, document state, and rendered-document data live in the library crate; platform shells are thin adapters around that model.

## Core Document Model

`MarkdownDocument` is the source input boundary. It owns the Markdown source and a display title, usually derived from the file name.

`RenderedDocument` is the shared rendered-output boundary. It contains:

- `title`: the document title used by full-document renderers.
- `html`: sanitized rendered Markdown body HTML.
- `headings`: table-of-contents data with stable generated heading IDs.

Alternate frontend engines should prefer consuming `RenderedDocument` instead of reparsing Markdown or scraping generated HTML.

## Renderers

`FrontendRenderer` is the trait used by frontend adapters:

```rust
pub trait FrontendRenderer {
    type Output;

    fn render_document(&self, document: &MarkdownDocument) -> Self::Output;
}
```

Current implementations:

- `TerminalRenderer`: renders Markdown to terminal text with wrapping, ANSI color, and bold attributes.
- `HtmlRenderer`: renders a complete standalone HTML document from `RenderedDocument`.

`render_html` and `render` are convenience functions for CLI-style callers. New frontends should use the document and renderer types directly when they need titles, headings, or alternate output formats.

HTML rendering applies heading IDs and a fenced-code iterator adapter to parser
events before serializing them. The adapter buffers raw text only for recognized
diff fences and, with `highlight`, Rust fences; other events pass through.
Buffered text is escaped before emitting highlighted HTML. Diff rows use a
preamble/body classifier without relying on hunk line counts. Rendering no longer
scans the generated HTML for code blocks. The library owns the shared diff palette,
layout rules, and print colour override used by the GUI shell.

Task-marker parsing follows GFM since pulldown-cmark 0.13: empty markers depend on a final newline, and text after a marker stays inline.

## GUI Event Flow

The macOS GUI binary is behind the `gui` Cargo feature. It uses `tao` for the window/event loop and `wry` for the system WebKit view.

The flow is:

1. Parse GUI CLI inputs.
2. Load `GuiPreferences`.
3. Build or restore `AppModel`.
4. Convert the model to `AppView` with `app_view_with_preferences`.
5. Load the app shell from the private `markview://app/` WebView protocol.
6. Receive toolbar, tab, recent-file, print, edit, save, link, drag/drop, and file-watch events through the event loop.
7. Mutate `AppModel`/`GuiPreferences`.
8. Rebuild `AppView` and send a small JavaScript state update to the WebView.

The WebView shell should remain a view adapter. File loading, tab state, stale state, preferences, and rendered document data should stay in Rust model types.

Shell requests, including WebKit Reload, are queued through the GUI event loop and rendered from the current model after earlier edit events. Reload preserves unsaved source, dirty state, and the active tab instead of returning to the launch snapshot or an empty `about:blank` page. Scroll positions, editor selection, and find text are kept in WebView session storage across shell reloads. Table-of-contents buttons scroll within the document without modifying the shell URL; the private URL also remains valid for WebKit IPC when a Markdown anchor adds a fragment.

Document link clicks pass the original `href` and source tab ID through IPC rather than navigating the shell. GUI support resolves local Markdown paths against the source tab's file URL and opens a new tab; the event loop then updates watchers, preferences, and the view. HTTP(S) links require an external-link warning before the default browser opens, and Cancel leaves the destination unopened. Same-document fragments scroll within the document without changing the shell URL. Missing or unsupported local destinations report an error without replacing the current document; untitled documents must be saved before relative links can be resolved.

## Editing

Each `DocumentTab` tracks an `editing` flag and a `dirty` flag alongside its `MarkdownDocument`. Toggling edit mode (toolbar button, View menu, or Cmd+E) flips `editing`; every keystroke in the editor textarea sends the full source back through `AppModel::update_source`, which updates the tab's document and sets `dirty`. Because the model recomputes `AppView` on every keystroke like any other mutation, the WebView shell has one exception to the "always re-render from state" rule: while a tab is being edited, `setState` leaves an already-mounted `<textarea>` for that tab alone instead of replacing it, so the state push doesn't reset the user's cursor position mid-keystroke.

`AppModel::mark_saved` clears the dirty flag once the GUI writes the source to disk; `AppModel::assign_path` gives an untitled tab a path after a Save As. Manual refresh (toolbar, menu, or per-tab reload) always discards in-progress edits and exits edit mode, since it's an explicit "show me what's on disk" action. Auto-refresh from the file watcher instead skips dirty tabs and marks them stale; clean tabs are refreshed even if their editing flag is set, so unsaved edits are never silently overwritten by an external change.

`AppModel::edit(id)` selects a tab and enters edit mode idempotently. The context menu sends `edit-tab:<id>` and `copy-tab-path:<id>` through the pure `ipc_event` parser. Copy Path writes the stored canonical path to `NSPasteboard` in-process, including Unicode; symlinks therefore copy their resolved targets.

`src/bin/markview_gui_support/editor.js` is injected into the app shell. A native textarea owns selection, undo and input methods, with a transparent background over a highlighted `<pre>`. Both layers soft-wrap and synchronize both scroll axes. The editor fills the available space; a separate resize handle is deliberately omitted. Overlay rebuilds are coalesced into one animation frame, use bare text nodes for plain text, and preserve/shift existing spelling marks outside an edit. They still rebuild the full overlay rather than patching individual lines.

Spelling requests use `spell:<id>:<revision>` after a debounce; the input revision prevents stale responses from updating an editor. Rust reads the tab's model source and queues work on a worker thread. A per-tab latest-request map skips obsolete queued work and removes closed tabs. The worker uses pulldown-cmark offset events to mask everything except prose, preserving newlines and UTF-16 offsets; link labels are checked, while code, destinations, reference definitions, autolinks, bare URLs, and HTML markup/blocks are excluded. A single NSSpellChecker batch call produces ranges, then `proxy.send_event` returns them to the GUI, which invokes `receiveEditorSpelling(id, revision, ranges)`.

## Persistence

GUI settings are stored in a local text config:

- macOS: `~/Library/Application Support/markview/preferences.conf`
- Other platforms: `~/.config/markview/preferences.conf`

Stored values include theme, sidebar visibility, auto-refresh, window size, recent files, last open files, and the active file. Paths are escaped with the library preference serializer so the file stays simple and portable.

## Document annotations

`src/annotations/` owns the Markdown codec, canonical store resolver,
rendered-text projection, anchors, and mutations. The projection counts
non-whitespace Unicode scalars from the Markdown renderer's prepared input
and tracks source lines and headings. Serve rewrites destinations on parser
events so link routing cannot change the selection offsets. Source and store
revisions let each frontend reject stale ranges or mutations.

The store resolver selects the closest existing store, then the closest
Git root, then the document directory. Reads open existing files without
creating directories. Writes traverse directory handles without following
symlinks and hold a persistent per-document lock through validation and
atomic replacement or removal. Direct Markdown edits use the final
snapshot comparison described in the README.

The review script converts Rust's ranges to DOM ranges, handles draft
storage and UI actions, and never resolves anchors itself. The browser
adapter uses the loopback-only annotation endpoint, and the WebView
adapter sends form-encoded IPC addressed to a tab. The GUI renders the
current annotation state into `AppView` and sends annotation-only changes as
a smaller update. Both frontends use the same directory watch planner to
observe creation, replacement, deletion, and changes in store precedence.
Source changes still follow each shell's normal refresh path.

Browser writes require `--annotate`, a matching loopback Host and the
server's random token. Token-bearing pages use a per-response script
nonce and Content Security Policy. The shared form parser checks encoded
size, decoded scalar counts and allowed fields.

## Packaging

Local macOS packaging keeps Cargo as the build path:

- `sh packaging/macos/bundle.sh` builds `markview-gui` and creates `target/macos/Markview.app`.
- `make package-macos` builds a release app bundle and creates `target/dist/markview-<version>-macos.zip`.

The bundle includes `Info.plist`, `Markview.icns`, document registration for Markdown files, and the GUI executable.

## Release Process

1. Update `RELEASE_NOTES.md`.
2. Run `cargo fmt`.
3. Run `cargo test`.
4. Run `cargo test --features gui`.
5. Run `node --test tests/editor.test.cjs`.
6. Run `cargo clippy --all-targets -- -D warnings`.
7. Run `cargo clippy --features gui --all-targets -- -D warnings`.
8. Run `cargo build --features gui --bin markview-gui`.
9. Run `make package-macos`.
10. Tag the release as `v<version>` after the reviewed release commit lands.
11. Push the tag. The release workflow builds the macOS zip and attaches it to the GitHub release.
