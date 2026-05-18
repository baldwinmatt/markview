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
- [ ] Add release notes and a repeatable local packaging command.

## v1.0: Stable Little Tool

- [ ] Stabilize renderer and frontend boundaries for alternate frontend engines.
- [ ] Document architecture, renderer traits, GUI event flow, persistence, and release process.
- [ ] Add CI for the test/build matrix: CLI tests, GUI feature build on macOS, formatting, and clippy.
- [ ] Publish GitHub releases once packaging is stable.

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
