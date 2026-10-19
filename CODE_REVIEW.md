# Code Review — changes since `da944d6`

Scope: `git diff da944d6..HEAD` — adds per-tab Markdown editing (edit mode,
dirty tracking, save), a tab right-click context menu (close others/left/
right, reload, export tab HTML), Export-as-HTML, unsaved-changes confirmation
dialogs on close/quit, and suppression of broken-pipe log noise in `--serve`
mode.

Method: 10 independent finder passes (line-by-line, removed-behavior audit,
cross-file call tracing, language-pitfall scan, wrapper/proxy correctness,
reuse, simplification, efficiency, altitude, and a fresh gap sweep), each
followed by direct verification against the current source. No `CLAUDE.md`
exists anywhere in this repo or at the user level, so no conventions findings
apply.

**Status: all 15 findings fixed**, in commits `781937e..3fb9914` (10 commits
on top of `7a8e971`). `cargo test` (71 tests) and `cargo clippy --all-targets`
both pass clean on the fixed tree. Per-finding verification below.

---

## Correctness bugs

### 1. Reloading a tab silently discards unsaved edits with no confirmation
**✅ Fixed** in `3aae833` — `ReloadTab` now checks `confirm_if_dirty` before calling the new `force_refresh`; the underlying `refresh_tab` itself now refuses to overwrite a dirty tab unless the `force_*` variant is used.
**File:** [src/bin/markview-gui.rs:286-293](src/bin/markview-gui.rs:286)

The `ReloadTab` handler calls `model.refresh(id, ...)` unconditionally. Every
other destructive tab action added in this same diff (`CloseTab`,
`CloseActiveTab`, `CloseOtherTabs`, `CloseTabsToLeft`, `CloseTabsToRight`,
window close, quit) checks `is_dirty()` and calls `confirm_discard_changes(...)`
first. The context menu's "Reload" item ([src/bin/markview-gui.rs:1979](src/bin/markview-gui.rs:1979))
is disabled only when the tab has no path (`!tab.path`) — never when the tab
is dirty.

**Failure scenario:** user enables edit mode on a file-backed tab, types
substantial changes, then right-clicks the tab and picks "Reload" (or
misclicks it). `refresh_tab` (src/lib.rs:866) overwrites `tab.document` from
disk and resets `editing`/`dirty` to `false` with zero warning — all
unsaved work vanishes instantly.

**Acceptance criteria:**
- [ ] Reloading a dirty tab (via context menu "Reload") shows the same
      "Discard them and …?" confirmation dialog used by the close actions.
- [ ] Reloading a clean (non-dirty) tab behaves as before, with no prompt.
- [ ] A test exercises `ReloadTab` on a dirty tab and asserts the model is
      left unchanged unless the user confirms.

---

### 2. Refresh (Cmd+R / toolbar / menu) also silently discards unsaved edits
**✅ Fixed** in `3aae833` — `RefreshRequested` now checks the active tab's dirty state via `confirm_if_dirty` before calling `force_refresh_active`.
**File:** [src/bin/markview-gui.rs:100-107](src/bin/markview-gui.rs:100)

Same defect as #1, but on the pre-existing Refresh action, which this diff's
editing feature made destructive for the first time. `RefreshRequested` calls
`model.refresh_active(...)` → `refresh_tab` (src/lib.rs:866) directly, with no
dirty check. Notably, the *file-watcher* trigger (`refresh_changed_paths`,
src/lib.rs:706-738) *was* patched in this diff to skip refreshing
editing/dirty tabs — but the manual Refresh path (and `refresh_path`,
`refresh_file_backed`) were not given the equivalent guard.

**Failure scenario:** user edits the active tab, then presses Cmd+R out of
habit. Their in-progress edit is silently replaced by on-disk content with no
dialog.

**Acceptance criteria:**
- [ ] `RefreshRequested` prompts to discard changes when the active tab is
      dirty, exactly like the close actions.
- [ ] Ideally, the guard is added once at the `refresh_tab`/`refresh`/
      `refresh_active` layer (see Finding 4) rather than patched per call site,
      so future callers can't reintroduce this gap.

---

### 3. Saving a tab while still in Edit Mode causes a false "stale" badge
**✅ Fixed** in `43d95aa` — `refresh_changed_paths` now keys off `tab.dirty` alone (not `editing || dirty`); a new test covers save-while-editing followed by the resulting file-watch event and asserts the tab is not marked stale.
**Files:** [src/lib.rs:706-738](src/lib.rs:706) (`refresh_changed_paths`),
[src/bin/markview-gui.rs:310-318](src/bin/markview-gui.rs:310) (`SaveRequested`)

`refresh_changed_paths` treats `tab.editing || tab.dirty` as "has unsaved
edits" (src/lib.rs:724) — being in edit mode alone, with *no* pending changes,
is enough to skip the refresh and call `mark_stale` instead. `mark_saved`
(src/lib.rs:822-828) only clears `dirty`; it never touches `editing` or
`stale_tabs`.

**Failure scenario:** user edits `notes.md` in-app and presses Cmd+S.
`save_active` writes the file and clears `dirty`, but the tab is still
`editing = true` (save doesn't exit edit mode). The app's own write is then
picked up by the file watcher, which dispatches `FilesChanged` →
`refresh_changed_paths` sees `(editing=true, dirty=false)` and calls
`mark_stale(id)` — the tab immediately shows an "externally modified"
indicator on a file that is, in fact, fully in sync with what was just saved.

**Acceptance criteria:**
- [ ] Saving a tab that remains in edit mode does not mark it stale when the
      resulting file-watch event fires for that tab's own path.
- [ ] A regression test covers: open tab → edit → save (stay in edit mode) →
      simulate the resulting `FilesChanged` event → assert `is_stale(id)` is
      `false`.

---

### 4. No single chokepoint for "confirm before discarding unsaved work"
**✅ Fixed** in `3aae833, 60b2797` — Added `confirm_if_dirty`, `sync_after_tab_mutation`, and `sync_persisted_view` shared helpers used by every destructive handler; more importantly, the dirty-safety invariant was pushed down into `refresh_tab` itself (private, dirty-checking) with explicit `force_refresh*` variants for the confirmed-discard path — so future callers can no longer bypass it by accident.
**File:** [src/bin/markview-gui.rs](src/bin/markview-gui.rs) (match arms at
lines 75, 100, 189, 210, 238, 254, 270, 286)

This is the systemic root cause of Findings 1 and 2. The
confirm-then-mutate pattern is hand-rolled independently in six-plus match
arms, each pairing a bespoke dirty-predicate (`any_tab_dirty`,
`other_tabs_dirty`, `tabs_to_left_dirty`, `tabs_to_right_dirty`,
`tab.is_dirty()`) with its own warning string directly on the `if` guard of
the match arm. Adding a new destructive action is one `if` clause away from
being forgotten — which is exactly what happened with `ReloadTab` and
`RefreshRequested`.

**Acceptance criteria:**
- [ ] Introduce one guarded-mutation helper (e.g.
      `fn guarded(window, is_dirty: bool, message: &str, action: impl FnOnce())`)
      that every destructive tab action routes through.
- [ ] `refresh_tab` (or a thin wrapper all public `refresh*` methods share)
      enforces the "don't discard unsaved edits" invariant once, at the model
      layer, rather than relying on each GUI call site to remember it.

---

## Correctness — lower severity / edge cases

### 5. `other_tabs_dirty` doesn't guard against a missing `keep_id`
**✅ Fixed** in `c8f292e, 60b2797` — `other_tabs_dirty` now delegates to `model.other_tab_ids(keep_id)`, which returns an empty `Vec` for an unknown id (test: `other_tabs_dirty(&model, 99)` is `false`).
**File:** [src/bin/markview-gui.rs:691-696](src/bin/markview-gui.rs:691)

Unlike its siblings `tabs_to_left_dirty`/`tabs_to_right_dirty` (lines
698-714), which explicitly `return false` when the id can't be found,
`other_tabs_dirty` has no such guard: if `keep_id` no longer exists in
`model.tabs()`, every tab satisfies `tab.id() != keep_id`, so the function
reports "dirty" if *any* open tab is dirty.

**Failure scenario:** two `close-others:` / tab-removal IPC messages race
(e.g. the target tab was already closed by a prior message before this one is
processed) — the user sees a spurious "Other open tabs have unsaved changes"
prompt for what is actually a no-op action.

**Acceptance criteria:**
- [ ] `other_tabs_dirty` returns `false` when `keep_id` is not present in
      `model.tabs()`, matching the sibling helpers.

---

### 6. Cmd+S / Cmd+E are registered both as native macOS menu shortcuts and JS keydown handlers
**✅ Fixed** in `5f2ba14` — Removed the redundant JS-side `keydown` handlers for `toggle-edit` and `save`; the native macOS menu key equivalents are now the sole dispatch path for Cmd+E/Cmd+S.
**File:** [src/bin/markview-gui.rs:940](src/bin/markview-gui.rs:940),
[974](src/bin/markview-gui.rs:974),
[2102-2110](src/bin/markview-gui.rs:2102)

"Save" (`sel!(markviewSave)`, key `"s"`) and "Toggle Edit Mode"
(`sel!(markviewToggleEdit)`, key `"e"`) are registered as `NSMenuItem` key
equivalents *and* the webview's JS `keydown` listener separately intercepts
`Cmd+S`/`Cmd+E` and calls `event.preventDefault()` before posting the same
IPC message. `preventDefault()` only cancels the DOM's default action; it does
not stop AppKit's `performKeyEquivalent:` menu dispatch, which — depending on
exactly how WKWebView's key-equivalent handling interacts with the app's main
menu — could result in the same shortcut firing twice (e.g. two stacked
Save-As dialogs) or, in the other direction, dead JS-side code that never
runs on macOS because the menu wins first.

**Failure scenario:** platform/timing-dependent double invocation of
`save_active`/`toggle_editing` on a single Cmd+S / Cmd+E press.

**Acceptance criteria:**
- [ ] Manually verify on macOS whether Cmd+S while the webview has focus
      triggers `SaveRequested` once or twice (e.g. by logging each dispatch).
- [ ] If it double-fires, remove the redundant JS-side handling for the keys
      that already have a native menu equivalent (keep JS-only handling for
      platforms/builds without the native menu).

---

### 7. `is_client_disconnect` suppresses errors on every route, not just `/events`
**✅ Fixed** in `b031cb2` — Disconnect suppression is now scoped to the `/events` route only; the generic connection handler logs all errors again.
**File:** [src/main.rs:106](src/main.rs:106)

`is_client_disconnect` is applied to the result of `handle_connection` for
*every* incoming connection, not only the long-lived `/events` stream it was
designed for. `UnexpectedEof`/`ConnectionReset` can also occur while parsing a
genuinely malformed or truncated request against `/` or other routes.

**Failure scenario:** a proxy, health-checker, or buggy client sends a
partial/garbled HTTP request and disconnects; the resulting error is now
silently dropped for any route, potentially masking a real protocol-handling
bug during development.

**Acceptance criteria:**
- [ ] Scope the suppression to the `/events` handler specifically, or confirm
      (and document) that swallowing these error kinds is acceptable for all
      routes.

---

## Test quality

### 8. New broken-pipe regression test is timing-based and can pass vacuously
**✅ Fixed** in `3fb9914` — Test now asserts a positive signal (`markview: /events client disconnected`, gated behind an env var) that the suppression path was actually exercised, polling with a deadline instead of fixed sleeps.
**File:** [tests/cli.rs:138-177](tests/cli.rs:138)

`serve_mode_does_not_log_noise_when_an_events_client_disconnects` only
asserts the *absence* of `"serve error"` in stderr, using three hard-coded
sleeps (200ms/300ms/300ms) to hope the OS surfaces a broken pipe on a second
write. There's no positive assertion that a broken-pipe write — and thus the
`is_client_disconnect` suppression path — was actually exercised.

**Failure scenario:** on a slow/loaded CI runner, the peer's FIN may never be
observed before the assertion runs, so the test passes without ever
exercising the code path it's meant to cover; it would pass just as well if
`is_client_disconnect`'s matching were wrong or removed entirely. It's also a
flakiness risk in the other direction under CI load.

**Acceptance criteria:**
- [ ] Add a positive signal that the disconnect path was hit (e.g. an
      internal counter/log line gated behind `cfg(test)`, or restructure the
      test to deterministically force the write failure) instead of relying
      solely on the absence of a log line plus fixed sleeps.

---

## Efficiency

### 9. Every keystroke in edit mode triggers a full Markdown→HTML re-render that isn't displayed
**✅ Fixed** in `4bbf737` — `app_view_with_preferences` now filters out the active document before rendering when the tab `is_editing()`, so Markdown parsing/rendering is skipped entirely while editing.
**Files:** [src/lib.rs:916-949](src/lib.rs:916) (`app_view_with_preferences`),
[src/bin/markview-gui.rs:305-309](src/bin/markview-gui.rs:305) (`EditChanged`
handler)

`app_view_with_preferences` unconditionally computes
`RenderedDocument::from_markdown(active_document)` — full parse, HTML
sanitize/render, heading extraction — regardless of `tab.is_editing()`. The
`EditChanged` handler calls `sync_view` (which calls this function) on every
keystroke, even though edit mode shows a raw `<textarea>` and never touches
`activeHtml`/`headings` for that tab.

**Failure scenario:** on a large document, every keystroke pays full render
cost for output that's computed, JS-escaped, shipped over
`webview.evaluate_script`, and immediately discarded by the renderer.

**Acceptance criteria:**
- [ ] Skip `RenderedDocument::from_markdown` (reuse empty/last-known
      html+headings) when the active tab is in edit mode; only re-render on
      exiting edit mode or when not editing.

---

### 10. Saving unconditionally re-syncs the file watcher, even for a same-path re-save
**✅ Fixed** in `3aae833` — `SaveRequested` now only calls `sync_watcher` when `save_active` reports `path_changed` (Save-As case); an ordinary re-save skips the watcher resync.
**File:** [src/bin/markview-gui.rs:310-318](src/bin/markview-gui.rs:310)

`SaveRequested` always calls `watcher.sync(model.watched_directories())`.
`Watcher::sync` canonicalizes (a real syscall) every currently-watched
directory to diff old vs. new. For the common case — re-saving an
already-tracked file to the same path — the watched-directory set is
unchanged, so this is a full canonicalize pass to arrive at the same answer.

**Acceptance criteria:**
- [ ] Only call `watcher.sync(...)` when `save_active` actually assigned a
      new path (Save-As on an untitled tab); skip it for ordinary re-saves.

---

## Reuse / simplification

### 11. Five near-identical "confirm → close → sync → persist → render → title" blocks
**✅ Fixed** in `3aae833` — Extracted `sync_after_tab_mutation`/`sync_persisted_view` helpers; every close/refresh handler's tail is now a single shared call.
**File:** [src/bin/markview-gui.rs:189-285](src/bin/markview-gui.rs:189)

`CloseTab`, `CloseActiveTab`, `CloseOtherTabs`, `CloseTabsToLeft`, and
`CloseTabsToRight` each repeat the same ~15-line shape: compute a dirty
predicate, confirm, then on confirm run `model.close_X(id); watcher.sync(...);
persist_open_state(...); sync_view(...); window.set_title(...)`. A future
change to the post-close sequence has to be applied in five places (this is
also the mechanism behind Finding 4).

**Acceptance criteria:**
- [ ] Extract a shared helper for the post-mutation tail
      (sync/persist/render/title) and a shared `confirm_if_dirty(window,
      dirty, message) -> bool`, reducing each arm to ~3 lines.

---

### 12. `close_others`/`close_to_left`/`close_to_right` share an identical body shape
**✅ Fixed** in `c8f292e` — `close_others`/`close_to_left`/`close_to_right` now delegate to a shared `close_ids` plus `other_tab_ids`/`tab_ids_to_left`/`tab_ids_to_right`.
**File:** [src/lib.rs:603-646](src/lib.rs:603)

Each function locates/validates the id, collects target ids into a `Vec`,
then loops calling `self.close(other_id)` — differing only in the
id-selection filter/slice.

**Acceptance criteria:**
- [ ] Factor out a shared `fn close_ids(&mut self, ids: impl IntoIterator<Item = u64>)`
      (or a single `close_where(predicate)`) used by all three.

---

### 13. GUI-side dirty-check helpers duplicate index math already computed in `lib.rs`
**✅ Fixed** in `60b2797` — GUI-side `tabs_to_left_dirty`/`tabs_to_right_dirty`/`other_tabs_dirty` now consume the same `model.tab_ids_to_left`/`tab_ids_to_right`/`other_tab_ids` lists `close_to_left`/`close_to_right`/`close_others` use, via a shared `ids_dirty` helper.
**Files:** [src/bin/markview-gui.rs:691-714](src/bin/markview-gui.rs:691) vs.
[src/lib.rs:603-646](src/lib.rs:603)

`tabs_to_left_dirty`/`tabs_to_right_dirty` re-derive the same
`position(id)` + slice-before/after partitioning that `close_to_left`/
`close_to_right` already compute to decide what to actually close. The
"which tabs does this action affect" logic now exists in two independent
places that must be kept in lockstep by hand.

**Acceptance criteria:**
- [ ] Expose the affected-id list from `AppModel` (it's already built
      internally as `to_close`/`others`) and have the GUI's dirty checks
      consume that same list instead of re-deriving it.

---

### 14. `assign_path` duplicates title-from-path derivation already in `MarkdownDocument::from_path`
**✅ Fixed** in `781937e` — Extracted `title_from_path`, used by both `MarkdownDocument::from_path` and `AppModel::assign_path`.
**File:** [src/lib.rs:831-843](src/lib.rs:831) vs.
[src/lib.rs:272-280](src/lib.rs:272)

`assign_path` re-implements
`path.file_name().and_then(|n| n.to_str()).unwrap_or("Markdown").to_owned()`,
duplicating the exact chain already encapsulated in
`MarkdownDocument::from_path`.

**Acceptance criteria:**
- [ ] Extract a shared `fn title_from_path(&Path) -> String` used by both.

---

### 15. `existingEditor` reuse check is an empty-if/real-work-in-else inversion
**✅ Fixed** in `63afc6f` — Inverted to `if (!existingEditor || existingEditor.dataset.tabId !== ...)`, removing the empty-if/real-work-in-else shape.
**File:** [src/bin/markview-gui.rs:1841-1861](src/bin/markview-gui.rs:1841)

The "leave the mounted textarea alone" branch is written as a positive `if`
containing only a comment, with all the real work (~20 lines building the
textarea) in the `else`. This is harder to scan than a guard clause.

**Acceptance criteria:**
- [ ] Invert to a guard: `if (!existingEditor || existingEditor.dataset.tabId
      !== String(next.activeTabId)) { …build textarea… }`, removing the
      empty branch.

---

## Summary

| # | Severity | Area | One-liner |
|---|----------|------|-----------|
| 1 | High | Correctness | Reload discards unsaved edits, no confirm |
| 2 | High | Correctness | Refresh (Cmd+R) discards unsaved edits, no confirm |
| 3 | High | Correctness | Save-while-editing → false "stale" badge |
| 4 | Medium | Altitude | No shared "confirm before discard" chokepoint |
| 5 | Low | Correctness | `other_tabs_dirty` missing id guard |
| 6 | Low | Correctness | Cmd+S/Cmd+E dual native+JS registration |
| 7 | Low | Correctness | Broad-route broken-pipe suppression |
| 8 | Low | Test quality | Flaky/vacuous broken-pipe test |
| 9 | Medium | Efficiency | Full re-render on every edit keystroke |
| 10 | Low | Efficiency | Unconditional watcher re-sync on save |
| 11 | Medium | Reuse | 5x duplicated close-tail sequence |
| 12 | Low | Reuse | `close_others/left/right` duplicated shape |
| 13 | Low | Reuse | Duplicated index-partition math |
| 14 | Low | Reuse | Duplicated title-from-path logic |
| 15 | Low | Simplification | Empty-if/else inversion in editor mount |
