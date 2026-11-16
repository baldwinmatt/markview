# Annotations Spec

Markview should let a reader review a Markdown document by attaching comments to passages of rendered text, in both the `--serve` browser view and the macOS GUI. Comments are written to a plain Markdown file in a hidden store so a coding agent can read them, act on them, and mark them resolved. The model follows plannotator's (selected text plus source line labels handed to the agent), but the comments persist on disk instead of being a one-shot message.

Status: implemented in the CLI, `--serve` and the macOS GUI. A checked box below has a passing automated test; an unchecked one does not, and says why where part of it is covered. The [build tickets](ANNOTATIONS_TICKETS.md) record the manual browser and WebView runs.

## Decisions

| Question | Decision |
| --- | --- |
| Where annotations live | A hidden central store, `.markview/annotations/`, mirroring document paths. |
| Which directory holds the store | One resolver for every frontend: an existing store, else the repository root, else the document's directory. |
| File format | One Markdown file per annotated document. No JSON store, no new dependencies. |
| Annotation kinds | Comments anchored to selected text, and whole-document comments. |
| Lifecycle | Persistent, with `open` / `resolved` status. No reply threads. |
| Browser writes | Off by default. `--serve --annotate` enables them; without it, existing annotations display read-only. |
| Who owns anchoring | The Rust library. Frontends send a selection and paint what the library resolves. |

## Goals

- [x] Select text in a rendered document and attach a comment to it, in `--serve` (with `--annotate`) and in the GUI.
- [x] Add a comment that applies to the whole document.
- [x] Show annotations as highlights in the text and as cards in a panel; edit, resolve, reopen, and delete them.
- [x] Persist annotations to a Markdown file an agent can read without markview installed.
- [x] Give agents a command, `markview --annotations`, that prints open annotations with current line numbers.
- [x] Let an agent mark an annotation resolved by editing the store file, and have open views update live.
- [x] Keep annotations attached when the document is edited, and never silently drop one that can no longer be placed.
- [x] Serve and GUI read and write the same store, so a comment made in one appears in the other.

## Non-Goals

- [ ] No suggestion marks (structured delete / replace). Write the suggestion as prose in the comment. _A non-goal: no test asserts an absence._
- [ ] No reply threads, authors, or multi-user review. One local reviewer.
- [x] No annotating in the GUI editor (Edit mode) or in the terminal renderer.
- [x] No annotations in `--html` export or print output.
- [ ] No following a document across renames or moves; a moved document's annotations are left behind in the store.
- [ ] No "send feedback" round or agent hook integration. The store file is the handoff.
- [ ] No CLI commands that mutate annotations in v1; agents edit the store file.
- [ ] No image attachments.

## Store Location

A single resolver in the library, `annotation_store_root(document_path)`, decides where a document's annotations live. Both frontends and the CLI use it, so they always agree for a given document.

- [x] Canonicalize the document path. Walk its ancestors starting at its parent directory, then pick the store root by this precedence:
  1. The nearest ancestor directory of the document that already contains `.markview/annotations/`.
  2. Otherwise the nearest ancestor that contains a `.git` entry (the repository root).
  3. Otherwise the document's own directory in every frontend.
- [x] The annotation file is `<store root>/.markview/annotations/<document path relative to store root>`, keeping the document's file name and extension. `docs/SPEC.md` in a repository maps to `.markview/annotations/docs/SPEC.md`.
- [x] Probe candidate stores without following symlinks. Report a symlinked store encountered during resolution as an error instead of falling through to another root.
- [x] Create the store directories on the first write, not before. Reads never create anything.
- [x] Delete the annotation file when its last annotation is deleted if it contains only the generated preamble. Keep a file with a user-edited preamble. Leave directories in place.
- [x] The store is under a hidden directory, so serve discovery (which skips hidden directories) never lists annotation files as documents.
- [x] Markview does not touch `.gitignore`. Committing `.markview/` or ignoring it is the user's choice; document both in the README.

All frontends use the same canonical document path and precedence. For example, outside Git and with no existing ancestor store, both serving `/notes` and opening `/notes/api/spec.md` use `/notes/api/.markview/annotations/spec.md` for that document.

## Store File Format

The file is Markdown that renders sensibly, reads naturally to an agent, and parses with a line scanner.

```markdown
# Annotations: docs/SPEC.md

<!-- markview-annotations v1. To resolve one, change [open] to [resolved]. Deleting a section deletes the annotation. Prefix a header-shaped body line with a backslash. -->

## a1 [open] lines 42-44
<!-- markview: section="Routing &gt; Assets" before="served under the root, " after=" unless the file" created="2026-11-15T10:32:00Z" -->

> only referenced assets are served

This contradicts the favicon rule two sections down. Pick one.

## a2 [resolved] line 80
<!-- markview: section="CLI Behavior" before="defaulting to " after=". Support" created="2026-11-15T10:35:10Z" -->

> port `7878`

Mention `--port 0` here.

## a3 [open] document
<!-- markview: created="2026-11-15T10:40:02Z" -->

Overall this needs a migration section.
```

- [x] A section starts at an unescaped line matching exactly `## a<N> [open|resolved] <anchor>`, where `<anchor>` is `line <n>`, `lines <n>-<m>`, or `document`. This applies inside fenced code as well. Any other line, including other `##` headings, is content.
- [x] IDs are `a` plus a decimal number, unique within the file. A new annotation takes the highest existing number plus one.
- [x] An optional `<!-- markview: ... -->` line directly after the header carries machine fields as `key="value"` pairs. Fields are `section` (heading path joined with ` > `), `before` and `after` (up to 32 Unicode scalar values on either side of the quote after whitespace collapse), `source_revision` and `start` (the last confirmed exact placement), and `created` (UTC, RFC 3339). Values escape `&`, `"`, `<`, `>` as HTML entities and any `-` that follows another `-` as `&#45;`. Unknown keys are preserved.
- [x] For a text-anchored section, the first nonblank lines after the optional metadata are a contiguous blockquote. A blank line separates it from the body, whose first line may itself begin with `>`. The quote is the selected text as displayed, with whitespace runs collapsed to single spaces. A `document` section has no quote. The writer leaves a blank line after the header or metadata before writing the quote or body.
- [x] Preserve body text after trimming leading and trailing blank lines. When writing a body line that would match a section header after removing zero or more leading backslashes, add one leading backslash. When reading body lines in this form, remove exactly one leading backslash. For example, write `\## a9 [open] document` for a literal `## a9 [open] document`, and `\\## a9 [open] document` if the body already contains a leading backslash. Agents use the same escaping when pasting into the file.
- [x] The `lines` in a header are the source lines the quote occupied when the file was last written by markview. They are a hint; the quote is authoritative.
- [x] Preserve the preamble verbatim and keep unrecognised content on rewrite. A line whose header does not parse belongs to the preceding section's body or the preamble. A section with no metadata line still loads. A hand-edited section that reads back unchanged does not block writes: a line-anchored section may have no quote, and a metadata line that does not parse is written back verbatim. If writing would lose content, or an ID names more than one section, report an error that names the annotation and leave the file untouched. A UTF-8 byte order mark before the first header is ignored, and a store whose lines all end in CRLF keeps CRLF.
- [x] Quotes are capped at 1,000 Unicode scalar values and bodies at 10,000. Reject longer selections or bodies with a visible error and preserve the draft.

## Store Updates

- [x] All Markview writers take an exclusive, per-document filesystem lock before resolving a mutation against the store. Hold the lock across reading, checking revisions, applying the operation, and committing by same-directory temporary file and atomic rename, or unlinking the last record. Keep the lock file in place across annotation file replacement and deletion so both processes use the same lock. The lock is `<document name>.markview-lock`; a name too long to take that suffix locks on a 16-digit digest of the name instead. Store, lock and temporary files are created with mode 0666 less the umask, and a replaced store keeps its mode. A first write that fails removes the directories it created. Readers and watchers ignore lock and temporary files.
- [x] Derive the opaque store revision from the resolved store path and exact file bytes, distinguishing absence from an empty file. Revisions are FNV-1a/64 digests in 16 hex digits, a fixed algorithm because `source_revision` is persisted in store files: the source revision hashes the source bytes, and the store revision hashes the path bytes, a `0x00` separator, a presence byte (`0x00` absent, `0x01` present) and the file bytes. Edits, status changes, and deletes include the revision exposed with the annotation list. Check it under the lock before acting on an ID. On a mismatch, report a conflict, refresh the list, and keep any draft. Adds merge into the current locked snapshot.
- [x] Direct Markdown edits are supported. Compare the current store bytes and existence with the locked snapshot immediately before replacing or deleting the file. On a difference, abort and report a conflict. External editors do not take the lock, so a direct write between that final check and the commit can still race. Document this limit and advise agents to re-read the file immediately before editing and verify the status after saving.

## Anchoring

Anchoring is implemented once, in the library, against a text projection of the Markdown source. Frontends do no fuzzy matching.

- [x] Build a `TextProjection` from the same source preparation and parser events as the HTML renderer, including mojibake repair and raw HTML rendered as literal text. Map text back to 1-based lines in the original source, including individual lines within multiline code and HTML blocks. Serve link rewriting must change link and image destinations without changing the projected text or source line mapping.
- [x] Derive the opaque source revision from the exact source bytes used to render the view before any preparation or link rewriting. Publish it with the preview body and with annotation responses so the frontend can detect mismatched snapshots.
- [x] The frontend walks decoded text nodes inside the rendered document root in DOM order. Exclude image alt text, footnote references and definition numerals, task-list controls, and shell UI. Include footnote definition prose and text inside syntax and diff spans. Rust and JavaScript use the same Unicode `White_Space` code point set and count Unicode scalar values. Collapse whitespace and structural breaks to single spaces for quote display. Remove all whitespace for offsets and matching. Matching is case-sensitive.
- [x] To create a text annotation, send the displayed source revision, the selected quote, and a zero-based, half-open range into the whitespace-stripped text. Under the store lock, Rust checks the revision against the current file and verifies that the normalized quote matches that exact nonempty range. It derives `before`, `after`, `lines`, and `section` from that occurrence. On a revision mismatch or invalid range, leave the store untouched and show a visible error. Preserve the draft and require a new selection before submitting it against a changed document.
- [x] To resolve a stored annotation, use the stored `start` if its `source_revision` matches and the normalized quote still matches at that offset. Otherwise rank matches by the sum of the common suffix length of `before` and common prefix length of `after` after removing whitespace. Break ties by matching `section`, then by distance from the stored first source line. Ignore missing hints. A tie after all applicable hints is ambiguous.
- [x] A missing or ambiguous match is **detached**. It keeps its card, quote, and last-known lines, with no highlight. It reattaches automatically when resolution yields a unique best match again.
- [x] Resolution output includes the source and store revisions. Per annotation, return id, status, body, quote, current lines, section, attached flag, and the resolved half-open range for attached text annotations. Frontends map ranges to DOM text nodes only for a matching source revision. Whole-document and detached comments have no range.
- [x] Whenever Markview rewrites a store file, refresh `lines`, `section`, `source_revision`, and `start` for attached text annotations using the same on-disk source snapshot. Preserve the last-known metadata for detached annotations. A document change alone does not rewrite the store.
- [x] Test projection parity against a parsed DOM from rendered fixtures. Use the same DOM traversal and normalization as the review UI. Include entity decoding, generated footnote labels, Unicode, and serve link rewriting. Compare normalized text and verify ranges map back to the intended selection and source lines.

## Agent Interface

- [x] `markview --annotations <FILE>...` prints open annotations for each file as Markdown on stdout, resolved against the current source so line numbers are fresh. A directory argument covers every document with an annotation file beneath it.
- [x] Output per document: the document path, the store file path (relative to the working directory when the store is below it), an instruction line, then one section per open annotation: text comments in document order, then whole-document comments, with detached ones last and marked as such. Resolved annotations are omitted with a one-line count. The browser panel and the JSON endpoint use the same order.
- [x] A document that cannot be listed is reported on stderr as `error: <document>: <problem>`, naming the store when the store is at fault. The remaining documents are still listed and the command exits non-zero. A header that almost parses, or a store with content but no sections, produces a `warning:` line on stderr. An ID used by two sections, or an input that is itself a store file, is an error.

```markdown
# Annotations: docs/SPEC.md
Store: .markview/annotations/docs/SPEC.md (change `[open]` to `[resolved]` there once addressed)

## a1 · lines 42-44 · Routing > Assets
> only referenced assets are served

This contradicts the favicon rule two sections down. Pick one.

## a3 · document

Overall this needs a migration section.

1 resolved annotation not shown.
```

- [x] `--include-resolved` also prints resolved annotations, after the open ones, with `resolved` in each header in place of the omitted-count line. It requires `--annotations`.
- [x] With nothing open, print `No open annotations for <path>.` and exit 0. A missing or non-Markdown input is an error in the existing CLI error style.
- [x] `--annotations` cannot be combined with `--serve` or `--html`, nor with `--daemon`, `--recurse`, `--open`, `--port`, `--annotate` or `--width`. The error names the conflicting flag and comes before any complaint about a missing input.
- [x] Update `skills/markview/SKILL.md`: pass `--annotate` when the user wants to review, read comments with `markview --annotations`, and resolve by editing the store file.

## Reviewing UI

Shared by both frontends: one script and one stylesheet owned by the library (as the diff palette is today), with a small transport adapter per shell.

- [x] Selecting text inside the document body shows a small Comment button near the selection. Selections that touch navigation, the table of contents, or the panel do not.
- [x] The button opens a composer with the quote and a text field. Cmd/Ctrl+Enter saves; Escape cancels, asking first in the composer when the draft holds text, and is ignored while an input method is composing. Ctrl/Cmd+Shift+K comments on the selection from the keyboard. Focus returns to the control that opened the draft.
- [x] Open annotations are highlighted in the text. Highlights are added client-side over text ranges and must not change the document's text content. Overlapping annotations are allowed.
- [x] An annotations panel lists cards in document order: quote, comment, line label, and actions for Edit, Resolve or Reopen, and Delete (with confirmation in the card, never a native dialog, which the GUI's WebView cannot show). Cards and their buttons are labelled with the comment they belong to. A header shows the open count, a "Comment on document" action, and a "Show resolved" toggle that is off by default.
- [x] Clicking a highlight focuses its card; clicking a card scrolls to its highlight. Detached cards have no highlight to scroll to.
- [x] Comment bodies render as escaped plain text with line breaks preserved. No Markdown or HTML rendering of bodies in v1.
- [x] An in-progress draft survives a live reload (session storage, keyed by document). Retain its source or store revision so a reload cannot silently retarget a selection or an edit.
- [x] Highlights and the panel are hidden in print.
- [ ] Highlight colours work in the light and dark themes and stay distinguishable from find-in-document matches and diff row colours. _Not covered: a visual property with no automated check._

## Serve Behaviour

- [x] Add `--annotate`. It requires `--serve`, like `--port` and `--open`.
- [x] Without `--annotate`, pages show existing annotations read-only: highlights and the panel, no composer and no actions. A page without annotations carries no review machinery: the review script and stylesheet are added only for a document that has a store, or always with `--annotate`. Its extras are the copy controls, alert styles and live-reload shell, pinned by `tests/fixtures/serve-baseline.expected.html`. The review assets are served from `/_markview/review.js` and `/_markview/review.css` with a long cache lifetime. The copy assets (`/_markview/copy.js` and `/_markview/copy.css`) are versioned independently, by a digest of those two files alone, so editing the review scripts does not change a plain page.
- [x] `GET /_markview/annotations?doc=<route>` returns the resolved annotations for a served document as JSON, with or without `--annotate`. The library only needs to write JSON, never parse it.
- [x] `POST /_markview/annotations` performs one operation and returns the updated list. The body is `application/x-www-form-urlencoded` with `op` (`add`, `edit`, `status`, `delete`), `doc`, and the applicable fields (`quote`, `start`, `end`, `source_revision`, `store_revision`, `body`, `id`, `status`). An add with no quote or range is a whole-document comment. Both kinds of add carry one `source_revision`, validated against the current source (`409` when stale, `400` when empty): a text add must send it, and a whole-document add may send it alone. Without `--annotate` return `405`. A revision or concurrent-write conflict returns `409`.
- [x] `/_markview/` is a reserved route prefix, like `/events`; a served directory of that name is not reachable.
- [x] Watch changes to the stores for served documents, including stores above the serve root and paths that do not exist yet. Detect first creation, deletion, and atomic replacement through directory watches and re-resolve the store location when an ancestor store appears or disappears. A document's store is watched from the first time the document is served, with no polling. An annotation-only change sends an `annotations` server-sent event; the first store for a page that was served without the review script reloads the page instead. The page refetches and repaints without replacing the document body. A source change refreshes the document and annotations together.

## GUI Behaviour

- [x] Annotating works in Preview mode for file-backed tabs. It is unavailable for untitled tabs, as Copy Path is.
- [x] While a tab has unsaved edits, existing annotations still display but new ones are disabled with a "Save to comment" hint, because line numbers must refer to the file the agent will read.
- [ ] The panel shares the sidebar with the table of contents. Add a View menu item and a shortcut for "Add Comment" on the current selection. _Covered except the native View menu item, which needs a running app._
- [x] Annotation state for the active tab is part of `AppView`, resolved in Rust and pushed with the existing state update. The WebView shell stays a view adapter.
- [x] New IPC messages follow the existing `kind:<tab id>:<payload>` pattern, with the same form-encoded payload the serve endpoint accepts, so the library has one request parser for both frontends.
- [ ] Apply the same store watch rules to file-backed GUI tabs. An external store change updates annotations without replacing the preview DOM or touching scroll position or unsaved source, even with document auto-refresh disabled. _Covered except watch delivery with auto-refresh disabled, which needs a running app._
- [x] Resolve highlights against the source displayed in the preview, including unsaved edits. Use the file on disk when refreshing persisted line and section hints. Manual refresh and auto-refresh re-resolve highlights against the source now displayed.

## Security Boundaries

`--annotate` is the first serve feature that accepts writes, so its limits are explicit.

- [x] Only `GET` and `HEAD` are accepted unless `--annotate` is set; then `POST` is accepted on `/_markview/annotations` only.
- [x] Reject any request to the annotation endpoint whose `Host` header is not `localhost:<port>` or `127.0.0.1:<port>`, compared without regard to case and allowing one trailing dot after `localhost`. This blocks DNS rebinding. The static review script and stylesheet under the same prefix hold nothing private and are served for any Host, so a page opened under another name can load them and say where to open it instead. Only requests to the reserved prefix use the strict HTTP reader; pages, assets and `/events` read just the request line.
- [x] Require a per-process random token on every `POST`, sent in an `X-Markview-Token` header, compared in constant time, and embedded only in pages requested with a valid `Host`. The custom header forces a CORS preflight, which the server never approves, so another site's page cannot post.
- [x] Block executable Markdown link destinations such as `javascript:` after decoding entities and normalizing URL syntax. `data:` is blocked for links, and for images unless its type is `image/*`. With `--annotate`, served assets carry `Content-Security-Policy: sandbox`. Keep ordinary links and fragments working. On token-bearing pages, send a Content Security Policy that allows scripts only with a fresh response nonce applied to Markview's own scripts. Disallow inline handlers, executable URLs, and embedded objects.
- [x] Send no CORS headers.
- [x] The `doc` field selects among served documents by route. It is never used as a filesystem path. Revalidate the document against the serve root before accessing its annotations. A request can write only that document's resolved annotation file and its lock and temporary files.
- [x] Start from the canonical store root. Reject symlinks at `.markview`, `annotations`, every mirrored directory, and the annotation file itself. Reject a non-regular annotation file. Walk or create missing directories relative to verified directory handles without following symlinks. Create lock and temporary files and commit replacements relative to the verified parent, so a path swapped after a check cannot redirect the write. A symlinked store is an error even if its target would otherwise pass a canonical containment check.
- [x] Cap form-encoded request bodies at 160 KiB. Allow up to 16 KiB of encoded bytes for all fields and delimiters other than the quote and body values, leaving room for those values even when each four-byte scalar expands to 12 bytes. Enforce the byte cap before allocating or decoding the body, and apply an HTTP read deadline. Reject oversize or malformed bodies with `400` and leave the store untouched. Apply the same field limits to GUI IPC.
- [x] Annotation text is always escaped on output: in page HTML, in JSON, and in the GUI state update.
- [x] Still bind to loopback only.

## Tests

- [x] Store format covers field round-trips, metadata escaping, preserved preambles and unknown keys, malformed headers, missing metadata, bodies beginning with `>`, and header-shaped body lines with and without leading backslashes, including inside fences. Exercise agent status flips, deleted sections, and appended body text.
- [x] Store location covers existing stores, repository roots, agreement across frontends for nested documents outside Git, and symlink rejection at each store path component on reads and first writes.
- [x] Store updates cover concurrent adds from separate processes, stale card conflicts on edit or delete, external status flips between snapshot and precommit check, and lock reuse after deleting the last record.
- [x] Projection covers emphasis, decoded entities, links and serve link rewrites, inline and fenced code, diff blocks, tables, lists, task lists, both kinds of generated footnote numerals, escaped raw HTML, mojibake repair, and non-ASCII text. Compare against a parsed DOM.
- [x] Anchoring covers exact selection of either of two identical passages on the same line and persistence of that placement after reopening, stale revision rejection, ranking existing matches by context, section, and line, ambiguous ties, quotes spanning formatting or blocks, changed source lines, and detach and reattach behavior.
- [x] CLI: `--annotations` output for open, resolved, detached, and document-level annotations; `--include-resolved`; directory argument; empty result; flag conflicts.
- [x] Serve tests cover read-only pages, method and token enforcement, bad hosts, body size and deadline checks, a maximum-length Unicode comment, stale store revisions, mutation effects, and store creation, replacement, and deletion events including stores above the serve root. Verify executable link destinations are blocked and the policy allows Markview's script while blocking untrusted scripts.
- [x] GUI tests cover IPC parsing, dirty and untitled tabs refusing new annotations, stale preview selections, and store changes refreshing existing highlights without losing unsaved source, scroll position, or the current draft.
- [x] Shared script tests cover normalization, conversion of scalar offsets to DOM ranges including supplementary code points, and overlapping highlights. Parity runs in Rust over the corpus in `tests/fixtures/parity`, against an HTML-parser stand-in that walks text nodes as the review script does. The script policy is asserted on the real response header; its enforcement was checked by hand in Chromium.

## Implementation Order

1. [x] Library: store format, store resolver, text projection, anchoring. `markview --annotations`. Usable by an agent against hand-written store files before any UI exists.
2. [x] Serve, read-only: shared script and styles, `GET` endpoint, highlights and panel, `annotations` event.
3. [x] Serve, writes: `--annotate`, `POST` endpoint, security checks, composer and card actions.
4. [x] GUI: IPC, `AppView` state, panel in the sidebar, menu item and shortcut, store watching.
5. [x] Docs: README, ARCHITECTURE, ROADMAP entry (FEAT-004), and the agent skill.

## Resolved Questions

- Serve pages show existing annotations read-only when `--annotate` is absent.
- `.git` is the only repository marker the resolver recognises for now. Other project markers can be added later.
- Outside Git, a document's own directory is the fallback for all frontends.
- Agents resolve comments by editing the Markdown file directly. Markview writers coordinate through a shared filesystem lock and check for external changes before committing.
- `--annotations` has an `--include-resolved` option.
