# Markview Release Notes

## 0.1.0

Initial local-first Markdown viewer release.

Highlights:

- Terminal Markdown viewing with wrapping, color, bold text attributes, and HTML export.
- Native macOS GUI backed by the system WebKit view.
- Tabs, close buttons, open/refresh/print toolbar actions, recent files, find-in-document, and table-of-contents navigation.
- Auto-refresh on file changes, stale indicators when auto-refresh is disabled, and scroll preservation.
- External browser handling for `http` and `https` links.
- Drag-and-drop opening for Markdown files.
- Local preferences for theme, sidebar visibility, auto-refresh, window size, recent files, and restored open files.
- Markdown tables, task lists, strikethrough, footnotes, heading anchors, and optional Rust code highlighting.
- Local macOS `.app` bundle and repeatable zip packaging command.

Known limitations:

- macOS packaging is local and unsigned.
- The GUI remains an optional Cargo feature.
