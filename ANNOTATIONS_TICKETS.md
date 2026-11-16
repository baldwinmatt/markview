# FEAT-004 — Persistent document annotations build tickets

[Annotation specification](ANNOTATIONS_SPEC.md)

Each ticket lists its direct prerequisites. Acceptance checks belong with the ticket that introduces the behavior.

| Ticket | Deliverable | Depends on |
| --- | --- | --- |
| FEAT-004.1 | Markdown store codec | — |
| FEAT-004.2 | Canonical store resolution and secure path access | — |
| FEAT-004.3 | Rendered text projection and source revisions | — |
| FEAT-004.4 | Anchor validation and resolution | .1, .3 |
| FEAT-004.5 | Agent CLI | .2, .4 |
| FEAT-004.6 | Shared store watching | .2 |
| FEAT-004.7 | Read-only browser review | .4, .6 |
| FEAT-004.8 | Locked mutations and shared form parser | .2, .4 |
| FEAT-004.9 | Secure browser writes and shared review controls | .7, .8 |
| FEAT-004.10 | macOS Preview review | .9 |
| FEAT-004.11 | Documentation and end-to-end verification | .5, .10 |

Delivery checkpoints are an agent CLI that reads hand-written stores (.5), read-only browser review (.7), browser writes with the security checks in place (.9), and macOS Preview review (.10).

## FEAT-004.1 — Parse and write the Markdown store

Implement a library codec for the v1 file in **Store File Format**. Keep the original preamble and unrecognized content available for round-tripping.

- [x] Recognize exact annotation headers even inside fences. Preserve malformed header lines as preamble or body text and load sections with no metadata.
- [x] Round-trip selection quotes, document comments, bodies beginning with `>`, and literal header-shaped body lines with any number of leading backslashes. Trim only surrounding blank body lines.
- [x] Round-trip metadata, including unknown keys, escaped entities and consecutive hyphens. Support the optional last-confirmed source revision and offset, context, heading path, and UTC RFC 3339 creation time.
- [x] Recognize agent-edited status markers, deleted sections, and appended body text. Refuse a rewrite that would lose content or target an ambiguous ID. Allocate new IDs from the highest existing number plus one.
- [x] Distinguish the generated preamble from any user-edited preamble so the mutation layer can decide whether to remove the file.

## FEAT-004.2 — Resolve the store and confine filesystem access

Implement the common library resolver and secure filesystem operations from **Store Location** and **Security Boundaries**. The result should give readers, writers, and watchers the same canonical document identity and mirrored store path.

- [x] Canonicalize the document and choose the nearest existing ancestor store, then the nearest ancestor containing a `.git` entry, then the document's directory. Verify nested documents outside Git map to the same path for CLI, serve, and GUI.
- [x] Reads create nothing. First writes create only the missing mirrored directories. Keep the source filename and extension.
- [x] Fail on symlinked store components, including the final file, and on a non-regular annotation file. A symlink found while probing for an existing store produces an error even if a fallback root exists.
- [x] Open or create directories relative to verified directory handles without following symlinks. Provide handle-relative operations for lock and temporary files, rename, and unlink. Exercise component swaps so a path change after validation cannot redirect a write.
- [x] Verify repository markers that are files, ancestor store precedence, symlink rejection on reads and first writes, and hidden-store exclusion from serve discovery.

## FEAT-004.3 — Project rendered text back to source

Use the library renderer in `src/lib.rs` and serve link handling in `src/main.rs` to produce a `TextProjection` from the same prepared Markdown and parser events that generate the preview.

- [x] Publish an opaque revision of the exact original source bytes before mojibake repair or link rewriting. Serve destination rewriting preserves projected text and its source mapping.
- [x] Map projected Unicode scalar positions to original 1-based lines and heading paths. Verify individual lines in multiline code and escaped HTML, including after source preparation.
- [x] Define the shared traversal and normalization contract with a parsed DOM fixture. Include decoded visible text in DOM order, code and diff spans, and footnote definition prose. Exclude image alt text, generated footnote numerals and references, task controls, and shell UI.
- [x] Collapse Unicode `White_Space` and structural breaks for displayed quotes. Strip whitespace for case-sensitive matching and zero-based, half-open scalar offsets. Verify parity for emphasis, entities, links and rewritten destinations, code, diff blocks, tables, lists, tasks, footnotes, literal HTML, mojibake, and non-ASCII text.

## FEAT-004.4 — Validate selections and resolve stored anchors

Build the library read model and exact-selection validator from **Anchoring**. Keep matching in Rust and return ranges that both frontends can paint.

- [x] Validate the source revision, nonempty bounds, and normalized quote against the selected range. Derive source lines, heading path, and up to 32 scalars of collapsed context on each side. Select either of two identical same-line passages and recover that exact occurrence after reopening.
- [x] Prefer the recorded offset when its source revision and quote still match. Otherwise rank matches by summed before/after context agreement, then heading match, then distance from the saved first line. Ignore absent hints and treat a remaining tie as detached.
- [x] Return source and store revisions with IDs, statuses, body text, quotes, line labels, heading paths, attachment state, and ranges where applicable. Hash the resolved store path and exact file bytes for the store revision, distinguishing an absent file from an empty one.
- [x] Keep detached comments visible with their quote and last-known lines. Verify detachment on missing or ambiguous text, automatic reattachment, moved lines, and selections crossing formatting or block boundaries. Document comments and detached comments have no text range.
- [x] Expose the metadata refresh operation for use during Markview writes. Refresh `lines`, `section`, `source_revision`, and `start` for attached annotations against the on-disk snapshot and preserve detached metadata. Resolution by itself never rewrites the store.

## FEAT-004.5 — List review comments from the CLI

Add `markview --annotations <FILE>...` and `--include-resolved` to argument parsing, help, and dispatch. Use the canonical resolver and anchor engine so the command works against hand-written files before the UI ships.

- [x] Accept multiple Markdown files and directories. A directory recursively finds documents with annotation files, including documents whose stores live above that directory. Use the existing CLI error style for missing or non-Markdown inputs.
- [x] Print the document path, store path, and status-edit instructions. Print open comments in document order with fresh source lines, quote, section, and body. Label detached comments and place them last in the open group. Include whole-document comments.
- [x] Omit resolved comments with a count by default. With `--include-resolved`, print them after the open group with a status label. Verify zero-open output and successful exit, including a resolved-only file with both flag settings.
- [x] Reject combinations with `--serve` or `--html`, and require `--annotations` for `--include-resolved`. Exercise output and flag handling in `tests/cli.rs`.

## FEAT-004.6 — Watch store creation, replacement, and deletion

Build reusable watch planning and event classification around the existing `notify` integration. Frontends use the same rules for the documents they track.

- [x] Watch directories so first creation, direct writes, atomic replacement, and deletion are detected. Track existing directories needed to discover missing store paths, including ancestors above a serve root.
- [x] Re-resolve affected documents and update watches when an ancestor store appears or disappears. Ignore temporary and lock file events. Verify that a resolver change reaches the frontend even if the previously selected path did not change.
- [x] Report which documents need annotation refresh while preserving the shell's existing source-change path. Test creation of a missing store hierarchy, replacement, deletion, and a switch between ancestor and fallback stores.
- [x] When multiple documents share watched directories, an unrelated file change does not trigger an annotation repaint. Removing one document from the tracked set preserves watches still needed by the others.

## FEAT-004.7 — Serve annotations read-only

Add the shared review script and stylesheet to the library. Give the serve shell a transport adapter for resolved annotation reads and store-change events.

- [x] Reserve the full `/_markview/` prefix and implement `GET /_markview/annotations?doc=<route>` with HEAD semantics. Accept only the two specified loopback Host values with the bound port. Resolve `doc` through verified served routes, including revalidation under the serve root. Escape JSON strings and send no CORS headers.
- [x] Show existing annotations read-only with a panel, open count, document-order cards, and a default-off resolved toggle. Render bodies as plain text with line breaks and preserve the appearance of documents without annotations.
- [ ] Paint overlapping open highlights without changing the document's text. Navigate between a highlight and its card. Detached cards retain their quote and last-known lines. Hide annotation UI in print and distinguish highlights from search and diff colours in both themes. _Covered except telling highlights from search and diff colours in both themes, a visual property with no automated check._
- [x] Paint ranges only when the response revision matches the preview. Test shared DOM traversal and scalar-to-DOM range conversion with parsed fixtures, including supplementary characters, generated footnotes, and overlapping ranges.
- [ ] Emit an `annotations` SSE event for store-only changes and repaint without replacing the document body. Source changes refresh the preview and annotations together, including when source and store events arrive close together. _Covered except source and store events arriving close together._

## FEAT-004.8 — Apply locked mutations through one request parser

Implement the library form parser and annotation operations used by HTTP and GUI IPC. Cover `add`, `edit`, `status`, and `delete` with typed results for validation errors and conflicts.

- [x] Enforce the 160 KiB encoded request cap, 16 KiB encoded overhead cap, and decoded limits of 1,000 quote and 10,000 body scalars. Reject malformed encoding, invalid fields and incomplete text selections. An add with no quote or range is a document comment. Accept maximum-length values containing four-byte scalars.
- [x] Hold a per-document exclusive filesystem lock across the snapshot read, revision checks, mutation, and commit. Keep the same lock file across annotation file replacement and removal. Adds merge into the current locked snapshot. Changes to existing IDs require the current store revision.
- [x] Use the on-disk source under the lock to validate text adds and refresh attached metadata during every rewrite. Assign creation time and ID for new records.
- [x] Compare locked snapshot bytes and existence just before handle-relative atomic replacement or deletion. Remove an empty generated store file after its final annotation is deleted, preserve a user-edited preamble, and leave directories and the lock in place.
- [x] Test separate-process concurrent adds, stale edit/status/delete revisions, a changed source revision, a direct external status flip before commit, and lock reuse after final deletion. Verify conflict and validation failures leave the store untouched.

## FEAT-004.9 — Enable secure browser writes and review controls

Wire the shared mutation service to `--serve --annotate` and add the interactive controls to the shared review script.

- [x] Require `--serve` for `--annotate` and propagate the flag to the daemon child. Keep the server loopback-only. Accept POST only at the annotation endpoint when enabled. Return 405 for disallowed methods, 409 for revision or concurrent-write conflicts, and 400 for malformed or oversized bodies.
- [x] Require the valid Host and per-process random `X-Markview-Token` before mutation. Read HTTP headers and framed bodies with a deadline and enforce the byte cap before body allocation or decoding. Cover missing or invalid tokens, bad Hosts, unsupported methods, malformed and slow requests, and route escape attempts. Send no CORS headers.
- [x] Block executable Markdown URLs after entity decoding and URL normalization. Preserve ordinary links and fragments. Serve a fresh response nonce on Markview scripts and a CSP that blocks untrusted scripts, inline handlers, executable URLs, and embedded objects. Verify in a browser that Markview's scripts and annotation controls work under the policy.
- [x] Offer a Comment button for selections wholly inside the preview. Show the quote in the composer, save with Cmd/Ctrl+Enter, and cancel with Escape. Support document comments, edit, resolve, reopen, and confirmed delete.
- [x] Retain drafts per document in session storage with the original source or store revision. On conflict, refresh cards and retain the draft. Require a new selection for a text add against changed source. Test mutation effects and draft retention through annotation and source refreshes.

## FEAT-004.10 — Integrate annotations into macOS Preview

Adapt the shared review assets to the WebView and carry resolved state in Rust `AppView`. Hook into the existing tab, sidebar, menu, IPC, and watch paths in `src/bin/markview-gui.rs`.

- [x] Show comments in Preview for file-backed tabs. Keep annotations unavailable in Edit mode and on untitled tabs. In dirty Preview, display existing comments, disable new comments, and show “Save to comment.”
- [ ] Share the sidebar with the table of contents. Add a View menu item and shortcut for Add Comment on the current selection. Use the same composer, cards, and draft behavior as serve. _Covered except the native View menu item, which needs a running app._
- [x] Send mutations through `kind:<tab id>:<payload>` with the shared form parser and limits. Validate against the addressed tab, escape the GUI state payload, and discard stale responses for the displayed document or revision.
- [x] Resolve highlights against the current preview, including unsaved source. Refresh persisted hints against disk. Manual refresh and source auto-refresh re-resolve against whichever snapshot the preview displays.
- [ ] Apply shared store watches to file-backed tabs even with source auto-refresh disabled. Verify a store-only change updates highlights without replacing the preview DOM or losing scroll, unsaved source, or the review draft. Cover stale selections, dirty and untitled add guards, and a tab switch while a response is in flight. _Covered except watch delivery with auto-refresh disabled, which needs a running app._

## FEAT-004.11 — Document and verify the review workflow

- [x] Update `README.md` with CLI, browser, GUI, and agent workflows. Explain store location, how to commit or ignore `.markview/`, and the direct-editor race between the final comparison and commit. Tell agents to re-read immediately before editing and verify after saving.
- [x] Update `ARCHITECTURE.md`, the `FEAT-004` roadmap status, and `skills/markview/SKILL.md` with the delivered behavior. The skill uses `--annotate` for review, reads comments with `--annotations`, and resolves by editing the store.
- [ ] In a browser and a running macOS GUI, review the same file. Create and edit comments in both directions, list them in the CLI, flip a status directly in Markdown, and verify live updates to both views. Edit the source to detach and then reattach a comment. Verify scroll, drafts, and dirty source survive store updates. _Not repeated for this revision: see the verification results for what was run._
- [x] Verify annotation UI stays out of print and standalone HTML export and the terminal renderer is unchanged. `libc`, already in the lockfile for the GUI, is now a dependency of every build: the store uses it for confined file operations. Run the repo's core, GUI, JavaScript, formatting, clippy, and GUI build checks. Record browser and macOS runtime results alongside automated coverage.


### Verification results

A checked box above has a passing automated test. The table says where those tests live and what was additionally run by hand on 2026-11-15.

| Check | Result |
| --- | --- |
| Shared store and anchoring | `src/annotations/tests.rs` with the default and `highlight` features: codec round-trips, hand-edited sections, concurrent processes, symlinked components on reads and first writes, a parent swap, an external edit between read and commit, file modes, long names, stable revisions, anchor ranking and the review cache. |
| Projection parity | `src/annotations/parity.rs` renders every file in `tests/fixtures/parity` and compares the text an HTML parser would build with the Rust projection, with LF and CRLF endings and with rewritten link destinations. In Chromium 156 a served document with C1 controls, NUL, NEL and U+2028 gave 321 DOM scalars and the server accepted a comment on the last eight. |
| CLI and render isolation | `tests/cli.rs`: the exact listing layout and order, broken stores beside good ones, near-miss header warnings, duplicate IDs, a store file as input and flag conflicts. Terminal rendering and standalone HTML omit comments. |
| Browser | `tests/cli.rs` serve tests cover Host and token checks, lenient ordinary requests, large refused POSTs, page byte-identity without annotations, cached review assets, the exact policy header, and store creation, replacement and deletion events including a store above the serve root. `tests/review.test.cjs` runs the review script against a stub DOM. By hand in Chromium 156: the policy blocked an injected inline script, an inline handler and an un-nonced script while Markview's ran; a page opened as `foo.localhost` carried no token and said where to open it; locating a selection in 242,000 characters took 15.7 ms (690 ms before). |
| macOS Preview | GUI unit tests cover IPC parsing, dirty and untitled guards, stale selections, the shared review cache and a store change that leaves unsaved source alone. By hand in a WKWebView 605.1.15 with no UI delegate, loading the real application shell: Delete works through the in-page prompt (`window.confirm` returns false there); starting a comment reveals the sidebar and it stays open across a state push; a store-only update keeps the preview nodes, scroll position and draft; focus returns to the Edit button on Cancel. The packaged application was not driven end to end for this revision. |
| Build and static checks | `cargo fmt --check`, Clippy for the default, `highlight` and `gui` feature sets, the three `cargo test` configurations, and both Node test files pass. Two intermittent tests were fixed afterwards: `daemon_tests::inherited_listener_keeps_port_reserved`, which predates annotations and raced other tests' process spawns, and the serve test for store events above the serve root, which raced the file watcher. The full suite then passed 12 consecutive runs. |
