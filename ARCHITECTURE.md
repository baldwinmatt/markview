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

Stored values include theme, sidebar visibility, auto-refresh, single-key annotation shortcuts, window size, recent files, last open files, and the active file. Paths are escaped with the library preference serializer so the file stays simple and portable.

## Document annotations

`src/annotations/` owns the Markdown codec, canonical store resolver,
rendered-text projection, anchors, and mutations. The projection counts
non-whitespace Unicode scalars from the Markdown renderer's prepared input
and tracks source lines and headings. `prepare_source` is that shared
preparation: mojibake repair, and U+0000 replaced with U+FFFD because a
browser drops NUL from body text. The HTML escaper keeps C1 control codes as
raw UTF-8, since a numeric reference to one is remapped through Windows-1252.
Serve rewrites destinations on parser events, so link routing cannot change
the selection offsets. `src/annotations/parity.rs` holds the test that keeps
all of this true: it renders the corpus in `tests/fixtures/parity` and
compares the text an HTML parser would build with the projection.

Source and store revisions let each frontend reject stale ranges or
mutations. They are FNV-1a/64 digests, a fixed algorithm because
`source_revision` is written into store files. `locate` confirms a quote at
its stored offset when the revision matches, and otherwise searches only where
the quote's rarest character occurs. `ReviewCache` remembers the projection
and resolved review of recently used documents by path, source revision and
store revision. `ServeConfig` and the GUI's `AppModel` each hold one, and a
write stores its own result there, so the read that follows is not resolved
again.

The store resolver selects the closest existing store, then the closest
Git root, then the document directory. Reads open existing files without
creating directories. Writes traverse directory handles without following
symlinks, using the `libc` crate's `openat` family (`src/annotations/paths.rs`;
other platforms get a stub that reports annotations as unsupported). They hold
a persistent per-document lock through validation and atomic replacement or
removal, and a first write that fails removes the directories it created.
A store is rewritten only if reading the result back gives the same
annotations; otherwise the write is refused and the error names the
annotation. Direct Markdown edits use the final snapshot comparison described
in the README.

The review script (`src/annotations/review.js`) converts Rust's ranges to DOM
ranges, handles draft storage and UI actions, and never resolves anchors
itself. It uses no native dialogs: the GUI's WebView has no dialog delegate,
so confirmations are drawn in the page. The browser adapter in the same file
uses the loopback-only annotation endpoint, and the WebView adapter sends
form-encoded IPC addressed to a tab. The GUI renders the current annotation
state into `AppView` and sends annotation-only changes as a smaller update,
skipping one that repeats the last. `tests/review.test.cjs` runs the script
against the stub DOM in `tests/support/review-dom.cjs`. What only a real
browser decides (focus, layout, selections, the nonce policy, highlights) is
checked by `tests/review.browser.js`: the Rust test
`review_browser_harness_passes_in_headless_chrome` loads the rendered review
fixture with that harness in headless Chrome or Chromium and reads its
verdict. `cargo test` skips it, saying so, when neither browser is installed;
`make test-browser` and CI set `MARKVIEW_REQUIRE_BROWSER` so that a missing
browser fails instead.

A served page carries the review script only when it can use it: always with
`--annotate`, otherwise only for a document that has a store. Any other page
carries no review machinery; its extras are the copy controls, alert styles
and live-reload shell, pinned by `tests/fixtures/serve-baseline.expected.html`.
The scripts and stylesheets are static, cacheable routes under the reserved
`/_markview/` prefix, each linked with a digest of its content. The copy
assets are versioned independently of the review assets: their digest covers
`copy.js` and `copy.css` alone, so editing `review.js`, `review.css` or
`verdict.js` leaves the bytes of a plain page unchanged. Both
frontends use the same directory watch planner to observe creation,
replacement, deletion, and changes in store precedence. Serve starts watching
a document's store the first time the document is served and blocks on watch
events with no timer; a rescan drops watches for documents that are gone.
Source changes still follow each shell's normal refresh path.

Browser writes require `--annotate` or `--review`, a loopback Host and the server's random
token, which is compared in constant time and embedded only in pages requested
through a loopback Host. Token-bearing pages use a per-response script nonce
and the policy from `content_security_policy`; assets are served sandboxed.
Only requests to the reserved prefix go through the strict HTTP reader; pages,
assets and `/events` read just the request line. Every response is followed by
a bounded drain of the unread request, so the client receives it instead of a
connection reset. The shared form parser checks encoded size, decoded scalar
counts and allowed fields.

## Collection review

`src/serve_review.rs` owns the foreground review session. The
`/_markview/review` endpoint reuses the annotation HTTP parser, loopback
Host check, form limits, and server token. It exposes collection counts and
a revision computed from the served routes and source and store revisions.

Finish holds the published allowlist stable and uses the same session lock
as annotation mutations. It captures source and store data, validates the
page and collection revisions, builds the result with the CLI annotation
formatter, and rechecks disk revisions before accepting the verdict.
Independent processes can still edit files during capture. The accepted
result is built from the captured data and stays fixed after acceptance.
The server sends an SSE finish event, then gives connections a bounded
window before exiting and printing the result.

The annotation UI shares quick labels and keyboard handlers between serve
and the WebView. The HTML renderer and annotation projection share parser
options. Generated alert titles and code controls are excluded from the
annotation text walk. Browser code copy uses the clipboard API. GUI code copy
uses a bounded IPC frame addressed to the current Preview tab and source
revision, with the same native pasteboard helper as Copy Path.

## Agent HTTP API

`src/serve_api.rs` answers everything under `/_markview/api/v1/`. The request
flow is: the strict HTTP reader parses the request, `serve_annotations::handle`
applies the loopback `Host` check and hands the prefix to `serve_api::handle`,
which then owns the route table, the credential, the lock policy and the
errors. Both handlers share only helpers (`json_response`, `post_body`,
`query_document`, `respond_error`, `token_matches`), the review endpoint and the
annotations endpoint, which take their differences as data (`Writes`).

Writes need a second credential beside the page token: `MARKVIEW_API_TOKEN`,
64 lowercase hex digits read once at startup into `ServeSession`. An empty
value counts as unset and a malformed one stops `--annotate` and `--review`.
`ServeSession::api_access` is the one place that says what the credential
allows (comment, and give a verdict only under `--review`); the catalog's
`can_annotate` and `can_finish` and the write gate both read it. Reads need no
credential. `GET /document` goes through the same `ReviewCache` projection as
`GET /annotations`, so a revision is projected once, refuses a document over
16 MiB, and answers `If-None-Match` with `304`. An API write without a
review session reads the live config but never holds the config lock across the
store's blocking `flock`. Every add names the revision it was made against; the
check runs before the lock (by digest for a whole-document add) and again
under it.

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
6. Run `node --test tests/review.test.cjs`.
7. Run `make test-browser`.
8. Run `cargo clippy --all-targets -- -D warnings`.
9. Run `cargo clippy --features gui --all-targets -- -D warnings`.
10. Run `cargo build --features gui --bin markview-gui`.
11. Run `make package-macos`.
12. Tag the release as `v<version>` after the reviewed release commit lands.
13. Push the tag. The release workflow builds the macOS zip and attaches it to the GitHub release.
