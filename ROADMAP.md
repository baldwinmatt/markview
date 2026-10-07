# Markview Roadmap

Markview is a small, fast, local-first Markdown viewer written in Rust. The project should stay useful from the terminal while growing into a polished macOS reading app with a clean boundary between Markdown parsing, document state, and frontend rendering.

## Direction

- [ ] Keep the core lightweight, portable, and dependency-conscious.
- [ ] Preserve the terminal viewer as a first-class interface.
- [ ] Make the macOS GUI pleasant for daily reading: stable chrome, tabs, refresh, find, navigation, print, links, drag/drop, and remembered state.
- [ ] Keep frontend rendering pluggable so future GUI or webview engines can reuse the same document model.
- [ ] Prefer incremental, well-tested improvements over broad rewrites.

## v0.2: Daily-Use Polish

- [x] Keep README current with actual CLI and GUI behavior.
- [x] Add GUI preferences for theme, sidebar visibility, auto-refresh, window size, recent files, and restored open files.
- [x] Add recent files and stale/modified indicators when auto-refresh is disabled.
- [x] Improve the empty state for first launch and no-open-document workflows.
- [x] Improve tab overflow behavior for many open documents.
- [x] Continue shrinking the GUI entrypoint into smaller modules around app state, webview shell, events, file watching, persistence, and generated HTML.
- [x] Add focused tests for preferences, tab state, stale state, persistence, restore behavior, and GUI command parsing.
- [x] Add focused tests for scroll preservation and watcher-adjacent behavior.

## v0.3: Reading Quality

- [x] Add Markdown extensions that fit the lightweight goal: tables, task lists, strikethrough, footnotes, and heading anchors.
- [x] Add code block syntax highlighting behind an optional feature or a small dependency.
- [x] Add built-in light, dark, and system themes.
- [x] Tune print-specific theme behavior.
- [x] Add export-to-HTML through the shared renderer layer.
- [x] Keep raw Markdown HTML sanitized unless a future trusted-content mode is explicitly designed.

## v0.4: Local App Maturity

- [x] Add macOS app bundle support while keeping cargo-based local builds as the default path.
- [x] Add app icon, Info.plist metadata, document type registration for Markdown files, and open-with behavior.
- [x] Add a CLI path for launching the GUI against one or more files.
- [x] Store settings in a portable local config location.
- [x] Add release notes and a repeatable local packaging command.

## v1.0: Stable Little Tool

- [x] Stabilize renderer and frontend boundaries for alternate frontend engines.
- [x] Document architecture, renderer traits, GUI event flow, persistence, and release process.
- [x] Add CI for the test/build matrix: CLI tests, GUI feature build on macOS, formatting, and clippy.
- [x] Publish GitHub releases once packaging is stable.

## v1.1: Editing

- [x] Add a per-tab Edit/Preview toggle backed by a plain-text Markdown textarea.
- [x] Track per-tab dirty state and save the active tab back to its file (Save As for untitled tabs).
- [x] Keep auto-refresh and manual refresh from clobbering unsaved or in-progress edits.
- [x] Add toolbar buttons, menu items, and keyboard shortcuts (Cmd+E, Cmd+S) for editing and saving.

## Issue Tracker

### BUG-001: GUI controls stop responding after backgrounding or minimizing; Reload produces a blank screen

- **Status:** Fix verified 2026-10-06; committed as `6e6ead5`. Heading/IPC, Reload, and tab/draft checks across the four-cycle Preview/Edit × background/minimize matrix passed. Detailed per-return control and position acceptance verification remains partial.
- **Reported behavior:** Consistently after roughly one minute backgrounded or minimized, returning to the GUI leaves the tab bar and toolbar unresponsive. Keyboard actions and the app's context menus also stop working.
- **Controls that still work:** Scrolling and the native macOS menu bar remain functional. The native menu can open a new document, but doing so does not restore the rest of the UI.
- **Related failure:** Selecting Reload from the right-click menu produces a blank screen.
- **Reproduction steps:** Open multiple documents, background or minimize the app for longer than one minute, return to it, and try tabs, toolbar actions, keyboard actions, and context menus. Repeat both background and minimize cycles. Check the Reload failure separately, using disposable content until unsaved-edit preservation is verified.
- **Expected behavior:** All GUI controls continue working after returning to the foreground, with the active document/tab, scroll position, editor state, and unsaved edits preserved.

**Diagnosis and fix**

- A table-of-contents click changed the inline shell URL from `about:blank` to `about:blank#heading`. Wry's macOS IPC request builder rejected that URL and silently dropped GUI commands. This failure reproduced immediately without backgrounding; removing the fragment restored commands without reloading.
- Native WebKit Reload cleared the inline `about:blank` shell. The repaired shell uses the private, reloadable `markview://app/` protocol, and table-of-contents buttons scroll without rewriting its URL.
- Shell requests are handled in event order against the current model, preserving drafts and the active tab. WebView session storage preserves reading positions, editor selection, and find text across Reload.

**Verification recorded 2026-10-06**

- Current-source debug app, launched as a separate local bundle on macOS 27.0 (26A428), with two disposable Markdown files and isolated preferences.
- The IPC URL regression test failed against `about:blank#first` and passed with the repaired shell URL. All 160 GUI-feature tests, the GUI build, GUI clippy checks, and formatting checks for the modified Rust file passed.
- Runtime checks passed for heading navigation followed by tab switching, native WebKit Reload, reading position at Section 10 of a long document, unsaved source/dirty state/edit mode/selection preservation, and an input event immediately followed by Reload.
- One background cycle in Edit mode longer than one minute passed, including tabs, typing, toolbar toggles, and a context-menu Reload cancelled at the unsaved-changes prompt.
- The user confirmed physical Dock restoration and responsive tab switching after an Edit/minimize interval longer than one minute. Subsequent inspection confirmed the unsaved draft and edit mode remained intact.
- A Preview/background interval longer than one minute passed after native dialog activation, including tab switching and preservation of the draft in the other tab. Automation's window-raise action alone did not activate the window.
- The final Preview/minimize interval exceeded one minute. The user confirmed physical Dock restoration, responsive switching between both tabs, and preservation of the unsaved draft. All four Preview/Edit × background/minimize intervals exceeded one minute and passed tab/draft checks; minimized-window returns used physical Dock restoration because automation could not restore them.
- Broader controls were explicitly exercised after Edit/background, and reading position/editor selection were checked across Reload. These observations do not establish the full control and position criteria after every lifecycle return; those two criteria remain unchecked.

**Investigation scope**

- Diagnose the cause before choosing a repair; an automatic UI reload that loses document state or unsaved edits is not an acceptable workaround.
- Investigate the unresponsive controls and Reload's blank screen together unless evidence establishes separate causes.
- Start with the confirmed background and minimize sequences. Expand to related transitions such as switching Spaces or waking from sleep only if evidence points to a shared cause.
- Establish the affected build and launch route, macOS version, and the failing interaction path through runtime reproduction; keep observations separate from hypotheses.

**Acceptance criteria**

- [x] Repeated background and minimize cycles longer than one minute succeed in the running macOS app, in both Preview and Edit modes.
- [ ] After each return, tabs, toolbar actions, typing in Edit mode, keyboard shortcuts, and app context menus work; scrolling and native macOS menus remain functional.
- [ ] The active document/tab, scroll position, editor state, and unsaved edits survive every cycle and any recovery.
- [x] Reload displays a usable document instead of a blank screen and does not silently discard unsaved edits.
- [x] Verification includes a document with unsaved edits and records the build, launch route, macOS version, cycle duration/count, and results.
- [x] Relevant automated checks pass alongside repeated runtime verification; existing generated-HTML tests alone do not establish that the lifecycle failure is fixed.

### FEAT-001: Edit from a tab's right-click context menu

- **Status:** Open; requested 2026-10-06.
- **Requested behavior:** Add an **Edit** option to each tab's right-click context menu that opens a Markdown editor for that tab's document.
- **Editor:** A plain-text editing panel with Markdown syntax highlighting and squiggles only.
- **Optional reference pane:** Show Markdown syntax guidance beside the editor.
- **Existing foundation:** The v1.1 editing work above records a per-tab plain-text editor and Edit/Preview toggle; this request adds the context-menu entry, highlighting, squiggles, and optional syntax reference.
- **Detail to establish before implementation:** What the squiggles should flag.

### FEAT-002: Copy Path from a tab's right-click context menu

- **Status:** Open; requested 2026-10-07.
- **Requested behavior:** Add a **Copy Path** option to each file-backed tab's right-click context menu that copies that document's full file path to the clipboard.
- **Target:** Copy the path of the right-clicked tab, even when another tab is active.
- **Untitled tabs:** Disable the option when the document has no file path.

### BUG-002 / FEAT-003: GUI document links and destination handling

- **Status:** Fixed and verified locally 2026-10-07.
- **Reported behavior:** Links in documents displayed in the GUI do not open.
- **Relative Markdown links:** Resolve the linked Markdown file relative to the current document's directory and open it in a new GUI tab.
- **External links:** Show a warning dialog before opening the destination, with **Cancel** and **Open in Browser** actions. Cancel leaves the link unopened; Open in Browser opens it in the user's default browser.
- **Diagnosis:** A relative link such as `guide.md` resolved against the private shell URL as `markview://app/guide.md`, which the navigation filter rejected. The document's file directory was never used. HTTP(S) links already opened externally but skipped the requested warning.
- **Fix:** Document clicks send the original link destination and source tab ID through IPC. Rust resolves local Markdown destinations beside the source document and opens a new tab, updating file watching and persisted state. HTTP(S) destinations require the **Cancel** / **Open in Browser** warning, including navigation-handler fallback. Same-document heading links scroll without changing the shell URL. Missing files, unsupported destinations, and relative links from unsaved untitled documents show an error and keep the current document intact.
- **Runtime verification:** Installed `/Applications/Markview.app` reproduced the local-link failure twice with neighboring disposable `source.md` / `guide.md` files. A separate current-source debug bundle at `target/bug002/Markview Link Test.app` on macOS 27.0 (26A428) opened the correct file in a new tab, repeated successfully after Markdown heading navigation, and kept the shell URL intact. **Cancel** dismissed the warning and added no Chrome tab; **Open in Browser** opened `https://example.com/` in the default browser, Google Chrome.
- **Automated verification:** Relative-link opening and external cancellation tests were observed failing before their implementation and passing afterward. Regression coverage also checks source-tab routing when another tab is active, encoded paths, parent directories, unsaved-source preservation, external approval, and failures without document replacement. All 165 GUI-feature tests pass. GUI build, GUI clippy across all targets with warnings denied, and modified-file formatting pass. Repository-wide formatting still reports pre-existing differences in `src/main.rs` and `tests/cli.rs`.

## Review Workflow

Every commit should be reviewed before starting the next task:

1. [ ] Commit the focused change.
2. [ ] Run the review workflow described in `code-reviewer.md` against that commit's diff.
3. [ ] Write the review output as `review-<identifier>.md`.
4. [ ] Address all valid critical and major findings immediately.
5. [ ] Amend the original commit with the fixes before proceeding.

Review files are local artifacts and should not be committed. Minor findings may be fixed immediately, explicitly deferred in the review file, or turned into follow-up work.

## Verification Expectations

- [ ] Run `cargo test` for core and CLI changes.
- [ ] Run `cargo test --features gui` for GUI-facing changes.
- [ ] Run `cargo build --features gui --bin markview-gui` before handing off GUI work.
- [ ] Add unit tests near the model or renderer code for pure behavior.
- [ ] Add integration or feature-gated GUI tests for persistence, command parsing, file restore, and watcher-adjacent logic.

## Assumptions

- Markview remains local-first and lightweight.
- Cargo-based usage remains the primary distribution path until packaging is mature.
- macOS GUI polish is the near-term priority, but CLI behavior should remain fast and reliable.
- Heavy rendering dependencies should be optional and justified by visible reading-quality improvements.
