# Annotations Spec

Markview should let a reader review a Markdown document by attaching comments to passages of rendered text, in both the `--serve` browser view and the macOS GUI. Comments are written to a plain Markdown file in a hidden store so a coding agent can read them, act on them, and mark them resolved. The model follows plannotator's (selected text plus source line labels handed to the agent), but the comments persist on disk instead of being a one-shot message.

Status: implemented. See the [build tickets](ANNOTATIONS_TICKETS.md) for verification coverage.

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

- [ ] Select text in a rendered document and attach a comment to it, in `--serve` (with `--annotate`) and in the GUI.
- [ ] Add a comment that applies to the whole document.
- [ ] Show annotations as highlights in the text and as cards in a panel; edit, resolve, reopen, and delete them.
- [ ] Persist annotations to a Markdown file an agent can read without markview installed.
- [ ] Give agents a command, `markview --annotations`, that prints open annotations with current line numbers.
- [ ] Let an agent mark an annotation resolved by editing the store file, and have open views update live.
- [ ] Keep annotations attached when the document is edited, and never silently drop one that can no longer be placed.
- [ ] Serve and GUI read and write the same store, so a comment made in one appears in the other.

## Non-Goals

- [ ] No suggestion marks (structured delete / replace). Write the suggestion as prose in the comment.
- [ ] No reply threads, authors, or multi-user review. One local reviewer.
- [ ] No annotating in the GUI editor (Edit mode) or in the terminal renderer.
- [ ] No annotations in `--html` export or print output.
- [ ] No following a document across renames or moves; a moved document's annotations are left behind in the store.
- [ ] No "send feedback" round or agent hook integration. The store file is the handoff.
- [ ] No CLI commands that mutate annotations in v1; agents edit the store file.
- [ ] No image attachments.

## Store Location

A single resolver in the library, `annotation_store_root(document_path)`, decides where a document's annotations live. Both frontends and the CLI use it, so they always agree for a given document.

- [ ] Canonicalize the document path. Walk its ancestors starting at its parent directory, then pick the store root by this precedence:
  1. The nearest ancestor directory of the document that already contains `.markview/annotations/`.
  2. Otherwise the nearest ancestor that contains a `.git` entry (the repository root).
  3. Otherwise the document's own directory in every frontend.
- [ ] The annotation file is `<store root>/.markview/annotations/<document path relative to store root>`, keeping the document's file name and extension. `docs/SPEC.md` in a repository maps to `.markview/annotations/docs/SPEC.md`.
- [ ] Probe candidate stores without following symlinks. Report a symlinked store encountered during resolution as an error instead of falling through to another root.
- [ ] Create the store directories on the first write, not before. Reads never create anything.
- [ ] Delete the annotation file when its last annotation is deleted if it contains only the generated preamble. Keep a file with a user-edited preamble. Leave directories in place.
- [ ] The store is under a hidden directory, so serve discovery (which skips hidden directories) never lists annotation files as documents.
- [ ] Markview does not touch `.gitignore`. Committing `.markview/` or ignoring it is the user's choice; document both in the README.

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

- [ ] A section starts at an unescaped line matching exactly `## a<N> [open|resolved] <anchor>`, where `<anchor>` is `line <n>`, `lines <n>-<m>`, or `document`. This applies inside fenced code as well. Any other line, including other `##` headings, is content.
- [ ] IDs are `a` plus a decimal number, unique within the file. A new annotation takes the highest existing number plus one.
- [ ] An optional `<!-- markview: ... -->` line directly after the header carries machine fields as `key="value"` pairs. Fields are `section` (heading path joined with ` > `), `before` and `after` (up to 32 Unicode scalar values on either side of the quote after whitespace collapse), `source_revision` and `start` (the last confirmed exact placement), and `created` (UTC, RFC 3339). Values escape `&`, `"`, `<`, `>` as HTML entities and any `-` that follows another `-` as `&#45;`. Unknown keys are preserved.
- [ ] For a text-anchored section, the first nonblank lines after the optional metadata are a contiguous blockquote. A blank line separates it from the body, whose first line may itself begin with `>`. The quote is the selected text as displayed, with whitespace runs collapsed to single spaces. A `document` section has no quote. The writer leaves a blank line after the header or metadata before writing the quote or body.
- [ ] Preserve body text after trimming leading and trailing blank lines. When writing a body line that would match a section header after removing zero or more leading backslashes, add one leading backslash. When reading body lines in this form, remove exactly one leading backslash. For example, write `\## a9 [open] document` for a literal `## a9 [open] document`, and `\\## a9 [open] document` if the body already contains a leading backslash. Agents use the same escaping when pasting into the file.
- [ ] The `lines` in a header are the source lines the quote occupied when the file was last written by markview. They are a hint; the quote is authoritative.
- [ ] Preserve the preamble verbatim and keep unrecognised content on rewrite. A line whose header does not parse belongs to the preceding section's body or the preamble. A section with no metadata line still loads. If the file cannot be rewritten without losing content or the operation's target ID is ambiguous, report an error and leave it untouched.
- [ ] Quotes are capped at 1,000 Unicode scalar values and bodies at 10,000. Reject longer selections or bodies with a visible error and preserve the draft.

## Store Updates

- [ ] All Markview writers take an exclusive, per-document filesystem lock before resolving a mutation against the store. Hold the lock across reading, checking revisions, applying the operation, and committing by same-directory temporary file and atomic rename, or unlinking the last record. Keep the lock file in place across annotation file replacement and deletion so both processes use the same lock. Readers and watchers ignore lock and temporary files.
- [ ] Derive the opaque store revision from the resolved store path and exact file bytes, distinguishing absence from an empty file. Edits, status changes, and deletes include the revision exposed with the annotation list. Check it under the lock before acting on an ID. On a mismatch, report a conflict, refresh the list, and keep any draft. Adds merge into the current locked snapshot.
- [ ] Direct Markdown edits are supported. Compare the current store bytes and existence with the locked snapshot immediately before replacing or deleting the file. On a difference, abort and report a conflict. External editors do not take the lock, so a direct write between that final check and the commit can still race. Document this limit and advise agents to re-read the file immediately before editing and verify the status after saving.

## Anchoring

Anchoring is implemented once, in the library, against a text projection of the Markdown source. Frontends do no fuzzy matching.

- [ ] Build a `TextProjection` from the same source preparation and parser events as the HTML renderer, including mojibake repair and raw HTML rendered as literal text. Map text back to 1-based lines in the original source, including individual lines within multiline code and HTML blocks. Serve link rewriting must change link and image destinations without changing the projected text or source line mapping.
- [ ] Derive the opaque source revision from the exact source bytes used to render the view before any preparation or link rewriting. Publish it with the preview body and with annotation responses so the frontend can detect mismatched snapshots.
- [ ] The frontend walks decoded text nodes inside the rendered document root in DOM order. Exclude image alt text, footnote references and definition numerals, task-list controls, and shell UI. Include footnote definition prose and text inside syntax and diff spans. Rust and JavaScript use the same Unicode `White_Space` code point set and count Unicode scalar values. Collapse whitespace and structural breaks to single spaces for quote display. Remove all whitespace for offsets and matching. Matching is case-sensitive.
- [ ] To create a text annotation, send the displayed source revision, the selected quote, and a zero-based, half-open range into the whitespace-stripped text. Under the store lock, Rust checks the revision against the current file and verifies that the normalized quote matches that exact nonempty range. It derives `before`, `after`, `lines`, and `section` from that occurrence. On a revision mismatch or invalid range, leave the store untouched and show a visible error. Preserve the draft and require a new selection before submitting it against a changed document.
- [ ] To resolve a stored annotation, use the stored `start` if its `source_revision` matches and the normalized quote still matches at that offset. Otherwise rank matches by the sum of the common suffix length of `before` and common prefix length of `after` after removing whitespace. Break ties by matching `section`, then by distance from the stored first source line. Ignore missing hints. A tie after all applicable hints is ambiguous.
- [ ] A missing or ambiguous match is **detached**. It keeps its card, quote, and last-known lines, with no highlight. It reattaches automatically when resolution yields a unique best match again.
- [ ] Resolution output includes the source and store revisions. Per annotation, return id, status, body, quote, current lines, section, attached flag, and the resolved half-open range for attached text annotations. Frontends map ranges to DOM text nodes only for a matching source revision. Whole-document and detached comments have no range.
- [ ] Whenever Markview rewrites a store file, refresh `lines`, `section`, `source_revision`, and `start` for attached text annotations using the same on-disk source snapshot. Preserve the last-known metadata for detached annotations. A document change alone does not rewrite the store.
- [ ] Test projection parity against a parsed DOM from rendered fixtures. Use the same DOM traversal and normalization as the review UI. Include entity decoding, generated footnote labels, Unicode, and serve link rewriting. Compare normalized text and verify ranges map back to the intended selection and source lines.

## Agent Interface

- [ ] `markview --annotations <FILE>...` prints open annotations for each file as Markdown on stdout, resolved against the current source so line numbers are fresh. A directory argument covers every document with an annotation file beneath it.
- [ ] Output per document: the document path, the store file path, an instruction line, then one section per open annotation in document order, with detached ones last and marked as such. Resolved annotations are omitted with a one-line count.

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

- [ ] `--include-resolved` also prints resolved annotations, after the open ones, with `resolved` in each header in place of the omitted-count line. It requires `--annotations`.
- [ ] With nothing open, print `No open annotations for <path>.` and exit 0. A missing or non-Markdown input is an error in the existing CLI error style.
- [ ] `--annotations` cannot be combined with `--serve` or `--html`.
- [ ] Update `skills/markview/SKILL.md`: pass `--annotate` when the user wants to review, read comments with `markview --annotations`, and resolve by editing the store file.

## Reviewing UI

Shared by both frontends: one script and one stylesheet owned by the library (as the diff palette is today), with a small transport adapter per shell.

- [ ] Selecting text inside the document body shows a small Comment button near the selection. Selections that touch navigation, the table of contents, or the panel do not.
- [ ] The button opens a composer with the quote and a text field. Cmd/Ctrl+Enter saves; Escape cancels.
- [ ] Open annotations are highlighted in the text. Highlights are added client-side over text ranges and must not change the document's text content. Overlapping annotations are allowed.
- [ ] An annotations panel lists cards in document order: quote, comment, line label, and actions for Edit, Resolve or Reopen, and Delete (with confirmation). A header shows the open count, a "Comment on document" action, and a "Show resolved" toggle that is off by default.
- [ ] Clicking a highlight focuses its card; clicking a card scrolls to its highlight. Detached cards have no highlight to scroll to.
- [ ] Comment bodies render as escaped plain text with line breaks preserved. No Markdown or HTML rendering of bodies in v1.
- [ ] An in-progress draft survives a live reload (session storage, keyed by document). Retain its source or store revision so a reload cannot silently retarget a selection or an edit.
- [ ] Highlights and the panel are hidden in print.
- [ ] Highlight colours work in the light and dark themes and stay distinguishable from find-in-document matches and diff row colours.

## Serve Behaviour

- [ ] Add `--annotate`. It requires `--serve`, like `--port` and `--open`.
- [ ] Without `--annotate`, pages show existing annotations read-only: highlights and the panel, no composer and no actions. A document with no annotations looks exactly as it does today.
- [ ] `GET /_markview/annotations?doc=<route>` returns the resolved annotations for a served document as JSON, with or without `--annotate`. The library only needs to write JSON, never parse it.
- [ ] `POST /_markview/annotations` performs one operation and returns the updated list. The body is `application/x-www-form-urlencoded` with `op` (`add`, `edit`, `status`, `delete`), `doc`, and the applicable fields (`quote`, `start`, `end`, `source_revision`, `store_revision`, `body`, `id`, `status`). An add with no quote or range is a whole-document comment. Without `--annotate` return `405`. A revision or concurrent-write conflict returns `409`.
- [ ] `/_markview/` is a reserved route prefix, like `/events`; a served directory of that name is not reachable.
- [ ] Watch changes to the stores for served documents, including stores above the serve root and paths that do not exist yet. Detect first creation, deletion, and atomic replacement through directory watches and re-resolve the store location when an ancestor store appears or disappears. An annotation-only change sends an `annotations` server-sent event. The page refetches and repaints without replacing the document body. A source change refreshes the document and annotations together.

## GUI Behaviour

- [ ] Annotating works in Preview mode for file-backed tabs. It is unavailable for untitled tabs, as Copy Path is.
- [ ] While a tab has unsaved edits, existing annotations still display but new ones are disabled with a "Save to comment" hint, because line numbers must refer to the file the agent will read.
- [ ] The panel shares the sidebar with the table of contents. Add a View menu item and a shortcut for "Add Comment" on the current selection.
- [ ] Annotation state for the active tab is part of `AppView`, resolved in Rust and pushed with the existing state update. The WebView shell stays a view adapter.
- [ ] New IPC messages follow the existing `kind:<tab id>:<payload>` pattern, with the same form-encoded payload the serve endpoint accepts, so the library has one request parser for both frontends.
- [ ] Apply the same store watch rules to file-backed GUI tabs. An external store change updates annotations without replacing the preview DOM or touching scroll position or unsaved source, even with document auto-refresh disabled.
- [ ] Resolve highlights against the source displayed in the preview, including unsaved edits. Use the file on disk when refreshing persisted line and section hints. Manual refresh and auto-refresh re-resolve highlights against the source now displayed.

## Security Boundaries

`--annotate` is the first serve feature that accepts writes, so its limits are explicit.

- [ ] Only `GET` and `HEAD` are accepted unless `--annotate` is set; then `POST` is accepted on `/_markview/annotations` only.
- [ ] Reject any request to `/_markview/` whose `Host` header is not `localhost:<port>` or `127.0.0.1:<port>`. This blocks DNS rebinding.
- [ ] Require a per-process random token on every `POST`, sent in an `X-Markview-Token` header and embedded in served pages. The custom header forces a CORS preflight, which the server never approves, so another site's page cannot post.
- [ ] Block executable Markdown link destinations such as `javascript:` after decoding entities and normalizing URL syntax. Keep ordinary links and fragments working. On token-bearing pages, send a Content Security Policy that allows scripts only with a fresh response nonce applied to Markview's own scripts. Disallow inline handlers, executable URLs, and embedded objects.
- [ ] Send no CORS headers.
- [ ] The `doc` field selects among served documents by route. It is never used as a filesystem path. Revalidate the document against the serve root before accessing its annotations. A request can write only that document's resolved annotation file and its lock and temporary files.
- [ ] Start from the canonical store root. Reject symlinks at `.markview`, `annotations`, every mirrored directory, and the annotation file itself. Reject a non-regular annotation file. Walk or create missing directories relative to verified directory handles without following symlinks. Create lock and temporary files and commit replacements relative to the verified parent, so a path swapped after a check cannot redirect the write. A symlinked store is an error even if its target would otherwise pass a canonical containment check.
- [ ] Cap form-encoded request bodies at 160 KiB. Allow up to 16 KiB of encoded bytes for all fields and delimiters other than the quote and body values, leaving room for those values even when each four-byte scalar expands to 12 bytes. Enforce the byte cap before allocating or decoding the body, and apply an HTTP read deadline. Reject oversize or malformed bodies with `400` and leave the store untouched. Apply the same field limits to GUI IPC.
- [ ] Annotation text is always escaped on output: in page HTML, in JSON, and in the GUI state update.
- [ ] Still bind to loopback only.

## Tests

- [ ] Store format covers field round-trips, metadata escaping, preserved preambles and unknown keys, malformed headers, missing metadata, bodies beginning with `>`, and header-shaped body lines with and without leading backslashes, including inside fences. Exercise agent status flips, deleted sections, and appended body text.
- [ ] Store location covers existing stores, repository roots, agreement across frontends for nested documents outside Git, and symlink rejection at each store path component on reads and first writes.
- [ ] Store updates cover concurrent adds from separate processes, stale card conflicts on edit or delete, external status flips between snapshot and precommit check, and lock reuse after deleting the last record.
- [ ] Projection covers emphasis, decoded entities, links and serve link rewrites, inline and fenced code, diff blocks, tables, lists, task lists, both kinds of generated footnote numerals, escaped raw HTML, mojibake repair, and non-ASCII text. Compare against a parsed DOM.
- [ ] Anchoring covers exact selection of either of two identical passages on the same line and persistence of that placement after reopening, stale revision rejection, ranking existing matches by context, section, and line, ambiguous ties, quotes spanning formatting or blocks, changed source lines, and detach and reattach behavior.
- [ ] CLI: `--annotations` output for open, resolved, detached, and document-level annotations; `--include-resolved`; directory argument; empty result; flag conflicts.
- [ ] Serve tests cover read-only pages, method and token enforcement, bad hosts, body size and deadline checks, a maximum-length Unicode comment, stale store revisions, mutation effects, and store creation, replacement, and deletion events including stores above the serve root. Verify executable link destinations are blocked and the policy allows Markview's script while blocking untrusted scripts.
- [ ] GUI tests cover IPC parsing, dirty and untitled tabs refusing new annotations, stale preview selections, and store changes refreshing existing highlights without losing unsaved source, scroll position, or the current draft.
- [ ] Shared script tests cover normalization, conversion of scalar offsets to DOM ranges including supplementary code points, and overlapping highlights. Use parsed DOM fixtures for parity and a browser test for the annotation-enabled page's script policy.

## Implementation Order

1. [ ] Library: store format, store resolver, text projection, anchoring. `markview --annotations`. Usable by an agent against hand-written store files before any UI exists.
2. [ ] Serve, read-only: shared script and styles, `GET` endpoint, highlights and panel, `annotations` event.
3. [ ] Serve, writes: `--annotate`, `POST` endpoint, security checks, composer and card actions.
4. [ ] GUI: IPC, `AppView` state, panel in the sidebar, menu item and shortcut, store watching.
5. [ ] Docs: README, ARCHITECTURE, ROADMAP entry (FEAT-004), and the agent skill.

## Resolved Questions

- Serve pages show existing annotations read-only when `--annotate` is absent.
- `.git` is the only repository marker the resolver recognises for now. Other project markers can be added later.
- Outside Git, a document's own directory is the fallback for all frontends.
- Agents resolve comments by editing the Markdown file directly. Markview writers coordinate through a shared filesystem lock and check for external changes before committing.
- `--annotations` has an `--include-resolved` option.
