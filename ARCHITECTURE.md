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

## GUI Event Flow

The macOS GUI binary is behind the `gui` Cargo feature. It uses `tao` for the window/event loop and `wry` for the system WebKit view.

The flow is:

1. Parse GUI CLI inputs.
2. Load `GuiPreferences`.
3. Build or restore `AppModel`.
4. Convert the model to `AppView` with `app_view_with_preferences`.
5. Render the app shell into the WebView.
6. Receive toolbar, tab, recent-file, print, link, drag/drop, and file-watch events through the event loop.
7. Mutate `AppModel`/`GuiPreferences`.
8. Rebuild `AppView` and send a small JavaScript state update to the WebView.

The WebView shell should remain a view adapter. File loading, tab state, stale state, preferences, and rendered document data should stay in Rust model types.

## Persistence

GUI settings are stored in a local text config:

- macOS: `~/Library/Application Support/markview/preferences.conf`
- Other platforms: `~/.config/markview/preferences.conf`

Stored values include theme, sidebar visibility, auto-refresh, window size, recent files, last open files, and the active file. Paths are escaped with the library preference serializer so the file stays simple and portable.

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
5. Run `cargo clippy --all-targets -- -D warnings`.
6. Run `cargo clippy --features gui --all-targets -- -D warnings`.
7. Run `cargo build --features gui --bin markview-gui`.
8. Run `make package-macos`.
9. Tag the release as `v<version>` after the reviewed release commit lands.
10. Upload the zip from `target/dist/` to the GitHub release.
