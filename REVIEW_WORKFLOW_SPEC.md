# Review Workflow Spec

A follow-up to the [Annotations Spec](ANNOTATIONS_SPEC.md). Annotations give Markview comments that persist on disk. This spec adds a review loop an agent can drive and improvements for reading and commenting on documents. The review command, quick labels, alerts, code copying, and keyboard navigation can ship independently.

Status: implemented on top of the annotations work. A ticked box has a passing automated test: a Rust test, a `tests/review.test.cjs` case, or `tests/review.browser.js` run in headless Chrome by `make test-browser` and CI. An unticked box is implemented and was checked by hand, but nothing automated covers it yet.

## Decisions

| Question | Decision |
| --- | --- |
| How an agent asks for a review | `markview --review`, a foreground server that waits for a verdict and prints the open comments from the accepted review snapshot. |
| How approval is expressed | The reviewer chooses Approve or Request changes for the entire served collection. |
| Exit status | `0` for either verdict by default. `--gate` makes Request changes exit `2`. |
| Quick labels | A fixed built-in list that fills in comment text. No store format change, no configuration. |
| GitHub alerts | Rendered in every frontend, using the parser's existing support. |
| Where the new UI lives | Labels, copying, and keyboard controls are shared by serve and the GUI. The verdict bar is browser-only. |

## Goals

- [x] One command an agent can run to get a human review of a document and read the result from stdout.
- [x] A clear approve or request-changes verdict, readable by an agent and by a script.
- [x] One-click comments for the things reviewers say most often.
- [x] GitHub alert blocks (`> [!NOTE]`) rendered as alerts.
- [x] A copy button on code blocks.
- [x] Reviewing without leaving the keyboard.

## Non-Goals

- No blocking review in the macOS GUI. `--review` is a serve feature; the GUI gets the labels, alerts, copy button, and keys.
- No agent hook integration and no plan-mode interception. The agent runs a command.
- No review rounds, history, or per-round state in the store. A verdict is printed, not persisted.
- No custom or configurable labels in this version.
- No change to the annotation store format or to `--annotations` output.
- No "what changed since last review", collection search, or link checking. Those stay candidates for later.
- No Mermaid, math, or general syntax highlighting.

## Review Command

`markview --review <FILE>...` or `markview --review <DIR>` serves the inputs with annotation writes enabled, opens the browser, and waits.

- [x] `--review` runs in the foreground, binds an ephemeral loopback port by default, and inherits serve's input validation, discovery, and live rescans. `--port` and `--recurse` retain their serve meanings.
- [x] Reject `--review` combined with `--serve`, `--daemon`, `--annotations`, `--include-resolved`, `--html`, `--open`, or `--annotate` in the existing CLI error style. Review enables annotation writes and browser launch by default.
- [x] `--no-open` requires `--review` and skips launching the browser. For SSH forwarding, choose a fixed `--port` and use the same local and remote port so the browser's `Host` passes the loopback check.
- [ ] If launching the browser fails, warn on stderr and keep the printed URL available for manual use. No automated test: a test cannot make the system launcher fail without also letting it open a real browser on machines where it works.
- [x] Print the URL and a one-line "waiting for review" notice to stderr. Stdout carries only the result, so an agent can capture it directly.
- [x] Every review page labels the verdict as applying to the entire served collection. The bar shows the collection-wide counts of open comments and documents with open comments. Request changes requires at least one open comment. Both actions confirm the verdict, scope, and counts. An unsaved draft on the current page is called out in the confirmation. Only saved comments count.
- [x] `GET /_markview/review` returns the collection counts and an opaque review revision derived from the current served routes and each document's source and store revisions. The bar refreshes on source changes, collection rescans, and annotation changes on any served route. Disable verdict actions while the summary is unavailable or being refreshed. A read error for any served document or store makes the summary unavailable, with an error the reviewer can retry.
- [x] Either action posts `op=finish`, `verdict=approved` or `verdict=changes`, the confirmed `review_revision`, and the current page's served route (`doc`) and rendered revision (`source_revision`) to `POST /_markview/review`. The route is checked against the current served allowlist. The finish handler checks that the page and collection still correspond to the confirmed revision and enforces the minimum open count for Request changes. A mismatch returns `409` and leaves the session open. Refresh the page and counts as needed and ask the reviewer to confirm again.
- [x] Coordinate finish with the publication of serve rescans and with annotation mutations in this review process. A mutation already admitted completes before finish validates the snapshot. Once finish succeeds, reject new mutations and finish requests with `409` while the listener is still available. Capture source and store bytes for each document and use those bytes for its revisions, counts, and resolved annotation output. Recheck the captured revisions before accepting the verdict and report a conflict on an observed difference. External editors and other Markview processes can still write independently. Capture has no cross-file transactional guarantee, and changes after acceptance stay on disk without altering the result.
- [x] After accepting a verdict, send the finish response and a `review-finished` event to connected pages. Disable verdict and annotation actions and show "Review finished. You can close this tab." Use a bounded shutdown so a disconnected requester or stalled event client cannot delay stdout or process exit. A request that arrives after the listener closes gets a connection failure.
- [x] The result on stdout is one verdict line and a blank line, followed by annotation output using the existing `--annotations` per-document formatter. Use only the captured served allowlist, once per canonical document, in served order. Include sections for documents with saved annotations, including any omitted resolved count. If none have saved annotations, print `No open annotations in the reviewed collection.` Use the same captured records for counts and output. Keep `--annotations` behavior unchanged.

```markdown
Review: changes requested (1 open comment in 1 document)

# Annotations: docs/SPEC.md
Store: .markview/annotations/docs/SPEC.md (change `[open]` to `[resolved]` there once addressed)

## a1 · lines 42-44 · Routing > Assets
> only referenced assets are served

This contradicts the favicon rule two sections down. Pick one.
```

- [x] The verdict line is `Review: approved` or `Review: changes requested`, followed by `(<N> open comment[s] in <M> document[s])`. Use singular nouns only for counts of one. `M` counts distinct documents with open comments. Approval may carry open comments.
- [x] Comments that were already open before the command started count and are printed. The command reports the state of the store, not only this session's additions.
- [x] Exit `0` for either verdict. With `--gate`, exit `2` for changes requested. `--gate` requires `--review`.
- [x] SIGINT or SIGTERM before finish cancels the session with the platform's signal exit status and no verdict. Errors before accepting a verdict use the existing nonzero error exit. Build the complete result before writing it to stdout. Saved comments remain readable with `markview --annotations`.
- [x] Do not exit because the last browser tab closed or disconnected; live reloads drop the connection routinely. The command ends on finish or on a signal.
- [x] Update `skills/markview/SKILL.md` to use `--review` for a human verdict, keep the process alive while waiting, and capture stderr for the URL and stdout for the finished result. If interrupted, use `--annotations` to recover saved comments and report that the session has no verdict.

## Quick Labels

- [x] The new-comment composer shows four keyboard-accessible label buttons above the text field: **Question**, **Needs a test**, **Out of scope**, and **Nit**.
- [x] Activating a label with a blank body saves the comment at once with the label as its body. Use the normal submit path, including revision validation, pending-request guards, and draft preservation on error. Activating a label with existing text prefixes it with the label, a colon, and a space. Keep the composer open and return focus to the text field. Activating the same label again does not duplicate the prefix.
- [x] A labelled comment is an ordinary annotation: same store section, same status, same output. Agents read the label as the first words of the body.
- [x] Labels apply to text-anchored and whole-document comments, in serve and in the GUI.

## GitHub Alerts

- [x] Render blockquotes that start with `[!NOTE]`, `[!TIP]`, `[!IMPORTANT]`, `[!WARNING]`, or `[!CAUTION]` as alerts in HTML, serve, and GUI output, with a title and an accent colour per kind in the light and dark themes.
- [x] Enable `Options::ENABLE_GFM` in `markdown_options()`. The scanner already enables it. Keep the renderer and annotation projection on the same parser options. Run the roadmap's before/after corpus comparison for HTML and terminal output and pin the intended alert changes.
- [x] Render a visible, accessible title for the kind. Explicitly mark the generated title as excluded from the annotation DOM walk. A selection starting in the title alone cannot create an annotation, and a range spanning the title and alert body quotes only document text. The projection skips the consumed marker and maps the remaining alert content to its original source lines.
- [x] In the terminal renderer, print the kind as a bold first line of the quoted block.
- [x] An unknown marker such as `[!FOO]` stays an ordinary blockquote with its text intact.
- [x] Alerts keep a visible border and title in print, without relying on background colour.

## Copy Button

- [x] Each fenced or indented code block in serve gets a Copy button. Show it on hover, keyboard focus, and on devices without hover. Reattach buttons after document refreshes. `--html` export has no copy controls.
- [ ] GUI Preview attaches the same buttons and reattaches them after tab switches. No automated test: the wiring lives in the GUI shell script, which only runs in a WKWebView.
- [x] Copy the rendered code payload, using decoded text from the block's `<code>` element. Preserve spaces, tabs, diff markers, and the exact trailing newline in that payload. Fences, Markdown container prefixes, and indentation consumed by the parser are outside the payload. Line endings and any renderer normalization follow the displayed code.
- [x] Keep the control outside `<code>` and exclude it explicitly from the annotation DOM walk and quote extraction. The control never changes a code block's text or annotation offsets.
- [x] Show "Copied" after clipboard success and "Copy failed" on error. Announce the result to assistive technology and keep focus on the button.
- [x] In serve, use the browser clipboard API from button activation. In the GUI, use the native pasteboard helper shared with Copy Path. Scope the copy request to the current Preview tab and source revision. Return a correlated success or error to the initiating button. Reject a stale or oversized payload without writing to the clipboard.
- [x] The button is hidden in print and is reachable with Tab.

## Keyboard Navigation

Provide an accessible "Single-key shortcuts" toggle in the review panel, on by default. Remember it per browser origin and as a GUI preference. A review on a new ephemeral port uses a new browser origin and starts with the default. When off, the standard Tab, Enter, and Space controls remain available. Ignore the letter shortcuts in editable fields, during IME composition, on key repeat, when a modifier is held, or when a dialog or another handler has already claimed the event. Preserve the composer's existing Escape handling independently of the toggle.

| Key | Action |
| --- | --- |
| `j` / `k` | Focus the next / previous visible annotation, scrolling its highlight into view. Wraps at the ends. |
| `c` | Open the composer for a valid document selection, or for a whole-document comment when the selection is collapsed or absent. Leave invalid nonempty selections alone. |
| `e` | Edit the focused annotation. |
| `r` | Resolve or reopen the focused annotation. |
| `Escape` | Use the composer's existing cancel behavior when it has focus. Otherwise clear annotation focus if the review panel owns focus. |

- [x] Order follows the visible panel, with detached cards last and resolved cards included only while "Show resolved" is on. With no card focused, `j` starts at the first and `k` at the last. Scroll to the card when it has no highlight. Navigation can reveal the GUI sidebar and must move actual keyboard focus to the card.
- [x] Route shortcuts through the same actions and permission checks as the buttons. On read-only serve pages, `c`, `e`, and `r` are inert. In dirty GUI previews, `c` is inert while actions on existing annotations remain available. Untitled tabs and the GUI editor leave all annotation shortcuts inactive.
- [x] Delete stays a button with confirmation. Consume an event only when its action runs. Preserve existing Find, editor, menu, and dialog behavior. Restore focus to the invoking control or card when dismissing a composer where possible.
- [x] List shortcuts in the README and in the applicable tooltips. No in-app shortcut overlay.

## Security Boundaries

- [x] `/_markview/review` exists only under `--review`. Outside that mode, `GET`/`HEAD` return `404` and `POST` returns `405`. Apply the existing local `Host` check to reads and writes and the token check to every finish request. Reuse the annotations endpoint's form framing, size cap, and read deadline. Review responses use `Cache-Control: no-store`. The page route carried by finish is an allowlist key and never a filesystem path.
- [x] `--review` keeps every annotations boundary: loopback only, `Host` check, per-process token, no CORS headers, nonce-only script policy.
- [x] Serve the new UI logic through Markview's existing nonce-approved scripts. Use DOM listeners and text nodes for controls, counts, errors, and other data.
- [x] Cap copied GUI text at 1 MiB of UTF-8. Bound the IPC frame before decoding it, and verify the decoded byte length before touching the pasteboard. Report an error without truncation if the cap is exceeded.

## Tests

- CLI tests cover the review flag conflicts, `--gate` and `--no-open` requiring `--review`, ephemeral and explicit ports, and separation of stderr and stdout.
- Review command tests cover both verdicts, zero-comment approval, existing open comments, resolved counts, exit codes with and without `--gate`, and exact output for one file, multiple files, and recursive and nonrecursive directories. Include an annotated descendant excluded by nonrecursive serve.
- Review state tests cover stale source, store, and membership revisions; a race between finish and a save; simultaneous finish requests; request-changes validation when there are no open comments; snapshot read failures; tab disconnects; stalled SSE clients; and signal cancellation with saved comments intact. Verify an accepted snapshot's counts and lines match the emitted result even if a file changes before stdout is written.
- Endpoint and browser tests cover disabled mode, bad `Host`, missing or invalid token, malformed or oversized forms, request deadlines, live collection counts across pages, draft warnings, reconfirmation after `409`, and disabled review and annotation controls after finish.
- Quick-label tests cover one-click save with a blank field, prefixing text, repeated activation, keyboard activation, and persistence of a usable draft after errors.
- Alert tests cover all kinds in HTML and terminal output, unknown markers, nested content, print titles and borders, the parser corpus comparison, and DOM projection parity for selections inside an alert and spanning its generated title.
- Copy tests cover fenced and indented code, nested containers, plain and highlighted Rust and diff blocks, CRLF input, trailing newlines, and unchanged annotation offsets. Exercise clipboard errors, GUI IPC size and revision validation, and button restoration after refresh.
- Keyboard tests cover actual focus, wrap and detached cards, the shortcut toggle, text fields, IME composition, modified and repeated keys, dialog and Find behavior, and the distinct permissions for read-only serve and dirty GUI previews.

## Implementation Order

1. [x] Review command: `--review`, review bar, finish endpoint, result output, `--gate`, `--no-open`, skill update.
2. [x] Quick labels.
3. [x] Keyboard navigation.
4. [x] GitHub alerts.
5. [x] Copy button.
6. [x] Docs: README, ARCHITECTURE, ROADMAP entry.

## Resolved Questions

- The built-in list omits **Looks good**. The review bar provides Approve for a positive verdict.
- `--gate` is opt-in.
- `j` / `k` / `c` / `e` / `r` are single-key shortcuts with an in-app toggle in both frontends.
