# Serve Collections Spec

Markview's `--serve` mode should grow from a single-file preview into a local documentation viewer for one file, an explicit set of files, or a top-level Markdown directory. The feature should remain local-first and conservative about filesystem exposure. Where behavior is ambiguous, default to the most restrictive option; looser behavior can be added later behind explicit flags.

## Goals

- [x] Keep single-file `--serve` behavior simple, defaulting to port `7878`.
- [x] Serve multiple Markdown files with app-like navigation.
- [x] Serve a directory as a top-level-only Markdown collection.
- [x] Rewrite same-root Markdown links so docs hotlink to each other.
- [x] Serve only referenced assets under the chosen root, without generating directory listings or exposing unreferenced files.
- [x] Preserve scroll position across live reloads.
- [x] Keep outside-root links behaving as they do today.
- [x] Block path traversal — including symlink-based and percent-encoded escapes, not just literal `..` — and avoid recursive Markdown discovery by default.

## Non-Goals

- [x] Do not recursively discover Markdown files by default; recursive discovery is available only via the explicit `--recurse` flag (see CLI Behavior / Directory Mode).
- [x] Do not expose a browsable directory listing.
- [x] Do not serve any file under root that isn't an explicit document or a referenced-and-validated asset (no open static file serving of the root).
- [x] Do not bind to any interface other than loopback (`127.0.0.1`).
- [x] Do not change terminal rendering behavior.
- [x] Do not require a heavyweight web framework or browser runtime.

## CLI Behavior

- [x] Support `markview --serve notes.md` on default port `7878`.
- [x] Support `markview --serve notes.md api.md` for an explicit multi-file set.
- [x] Support `markview --serve docs/` for a top-level directory collection.
- [x] Add `--recurse` to opt a single served directory into recursive Markdown discovery (`markview --serve --recurse docs/`); reject it when combined with explicit files, multiple inputs, a non-directory input, or without `--serve`.
- [x] Add `--port <PORT>` as the only way to select a non-default port.
- [x] Remove the legacy `markview --serve <PORT> <FILE>` positional-port form. Reject it with a clear, specific error (e.g. "the `--serve PORT FILE` form has been replaced by `--serve FILE --port PORT`") rather than silently trying to interpret the number as a filename.
- [x] Reject invalid `--port` values (non-numeric, `0`, or `> 65535`) with existing-style CLI errors, distinct from a runtime "address already in use" bind failure.
- [x] Reject `--serve` with no file/directory input.
- [x] Reject a `--serve` invocation that mixes a directory argument with explicit file arguments (e.g. `--serve docs/ extra.md`).
- [x] Reject explicit file sets containing duplicate paths, including paths that canonicalize to the same file (e.g. a symlink alias, or `./a.md` vs `a.md`).
- [x] Define useful errors for directories with no top-level Markdown files.

## Directory Mode

- [x] Discover Markdown files only in the served directory itself (no subdirectories) by default; with `--recurse`, discover Markdown files at any depth under the served directory, skipping hidden (dotfile-prefixed) directories and not descending into symlinked directories (avoids escape/cycle risk).
- [x] Treat `.md`, `.markdown`, and `.mdown` as Markdown documents (matching the rest of the app's accepted file set), comparing the extension case-insensitively.
- [x] Exclude dotfiles (names starting with `.`) from discovery by default.
- [x] Choose the default document in this order, matching exact filenames case-sensitively:
  - [x] `README.md` at the served directory's top level (even when `--recurse` also finds a nested `README.md` — the top-level one still wins)
  - [x] `index.md` at the served directory's top level
  - [x] first Markdown file, sorted alphabetically by relative path (this generalizes to the whole tree under `--recurse`)
- [x] If two discovered entries canonicalize to the same file, or their relative paths collide case-insensitively (possible on case-insensitive filesystems, e.g. `README.md` and `readme.md` both present in the same directory), reject at startup with a clear error instead of silently picking one.
- [x] Do not include assets in navigation.
- [x] Do not scan subdirectories for navigation documents.
- [x] Allow nested assets (images, etc.) to load if they are directly referenced by a served document and resolve under the root.
- [x] Do not extend that same-reference allowance to Markdown files: a link to a nested `.md` file is left as a plain, unrewritten link rather than served on demand — honoring it would reintroduce recursive discovery through link-following.
- [x] Reject at startup any discovered document whose canonical path resolves outside the served root (e.g. a symlink escaping root).

## Explicit Multi-File Mode

- [x] Accept two or more explicit Markdown files.
- [x] Compute the common root as the deepest shared canonical ancestor directory of the explicit file set.
- [x] Reject the set if the computed common root is a filesystem root (e.g. `/` or a drive root) — the files must share a real ancestor directory, not just "the filesystem."
- [x] Reject if any explicit file's canonical path escapes the computed common root (shouldn't be reachable given the computation above, but guard it explicitly).
- [x] Include exactly the explicit Markdown files in navigation.
- [x] Use the first input file, in command-line order, as the default document.
- [x] Support explicit files from different directories when they share a canonical common root that passes the checks above.
- [x] Hotlink same-root Markdown links only when the resolved target is under the common root **and** is itself one of the explicit documents. Links to other same-root Markdown files that weren't explicitly listed are left unrewritten — explicit means explicit.

## Routing

- [x] Serve `/` as the default document.
- [x] Serve known Markdown documents at normalized root-relative routes, such as `/README.md` and `/notes.md`.
- [x] Serve only referenced assets under the root, at normalized root-relative routes such as `/assets/diagram.png`. This is an allowlist populated by scanning served documents for asset references — not a general static file server over the root.
- [x] Return `404 Not Found` for unknown routes, and for any file under root that exists on disk but is neither a served document nor a discovered-and-validated asset reference.
- [x] Return `404 Not Found` — uniformly, never `403` — for traversal attempts that resolve outside the root, so the response code can't be used as an oracle for "exists outside root" vs. "doesn't exist."
- [x] Percent-decode the request path (including doubly-encoded sequences) before resolving it, then canonicalize the resolved path and confirm it is prefixed by the canonicalized root. Reject requests containing null bytes or control characters outright.
- [x] Only handle `GET` and `HEAD`; reject other methods with `405 Method Not Allowed`.
- [x] Keep `/events` for live reload.
- [x] Do not provide any route that lists directory contents.
- [x] Never bind to an interface other than `127.0.0.1` (loopback-only).

## Link Rewriting

- [x] Rewrite same-root relative Markdown links to served routes, restricted to links that target one of the served documents (per the Directory/Explicit Mode rules above).
- [x] Preserve fragments, such as `other.md#install`.
- [x] Preserve pure fragment links, such as `#install`.
- [x] Preserve query strings on links as opaque — do not attempt to resolve or rewrite based on them.
- [x] Preserve external links, including `http:`, `https:`, and `mailto:`.
- [x] Preserve absolute filesystem-style links (e.g. `/etc/...`, `C:\...`, `file://...`) unrewritten, the same as outside-root links.
- [x] Preserve links that resolve outside the served root.
- [x] Rewrite same-root asset links to served routes only when the asset resolves under root and has been added to the allowlist by reference scanning (see Routing).
- [x] Apply rewriting only to served HTML, not exported `--html` output or GUI rendering.

## Navigation UI

- [x] Render no navigation, or minimal chrome only, for a single served file.
- [x] Render horizontal tabs for explicit multi-file sets of up to 6 documents.
- [x] Render a quiet sidebar or compact file list for directory mode and for any set larger than 6 documents.
- [x] Use the first heading or document title as the visible nav label.
- [x] Fall back to the filename when no heading/title is available.
- [x] Use the route or filename as tooltip text.
- [x] Mark the active document in navigation.
- [x] Keep the existing footer visible in served pages: `Served by markview`.

## Live Reload

- [x] Watch all served Markdown documents (not assets).
- [x] When the active document's content changes, reload its rendered content and preserve scroll position.
- [x] When any watched document's title changes (active or not), refresh navigation labels and shell chrome without forcing a full content reload, unless the active document also changed.
- [x] Update `document.title` after an active-document reload.
- [x] Keep the footer and shell chrome visible after code or shell changes that require a full browser refresh — the full-refresh path must render the same footer and nav as the live-reload path, with no separate no-footer code path.
- [x] Keep routine `/events` disconnects quiet.

## Serve Model

- [x] Add a served collection model, roughly:

```rust
struct ServeConfig {
    root: PathBuf,
    documents: Vec<ServedDocument>,
    default_document: PathBuf,
    port: u16,
}

struct ServedDocument {
    source_path: PathBuf,
    route_path: String,
    title: String,
}
```

- [x] Track allowlisted asset routes (discovered by reference scanning) separately from `documents`, so the routing layer has a single source of truth for "is this request servable."
- [x] Canonicalize the root and served paths before routing decisions, and re-validate on every request rather than trusting the startup-time computation alone.
- [x] Restrict served content-types to a fixed safe list (images, plain text, common doc formats). Force any other extension — including `.html`, `.htm`, `.svg`, `.xhtml` — to `application/octet-stream` rather than trusting the extension to render inline, to avoid script execution from untrusted served content (e.g. an SVG containing `<script>` in a directory someone points `--serve` at).
- [x] Keep rendered-document concerns reusable and out of socket handling.
- [x] Keep the serving adapter thin around file IO, routing, and response writing.

## Security Boundaries

- [x] Percent-decode (including doubly-encoded sequences), then canonicalize requested paths before serving.
- [x] Confirm every served file — documents discovered at startup and assets discovered by reference scanning — resolves under the configured root. Reject at startup for documents; reject the request for assets.
- [x] Reject requests whose canonicalized target resolves outside the root, or that fail to canonicalize (e.g. a dangling symlink component), with `404`.
- [x] Block traversal via canonicalization, not literal `..` string matching — this also catches symlink-based escapes and encoded traversal sequences.
- [x] Do not recursively discover Markdown documents by default.
- [x] Do not expose directory listings.
- [x] Do not serve any file under root that is not an explicit/discovered document or a scanned-and-validated asset reference — no open static file serving of the root.
- [x] Restrict served asset content-types to a safe list; never render untrusted served content as `text/html` or `image/svg+xml`.
- [x] Only accept `GET`/`HEAD` requests; reject others with `405`.
- [x] Never bind to a non-loopback interface.
- [x] Preserve current behavior for outside-root links instead of fetching or proxying them.

## Tests

- [x] Single-file `--serve` still returns rendered HTML.
- [x] Single-file `--serve` includes the footer.
- [x] `--port` selects a custom port.
- [x] Legacy `--serve 8080 file.md` now errors with a message pointing to `--port`.
- [x] `--port` with a non-numeric or out-of-range value errors distinctly from a bind failure.
- [x] Mixing a directory and explicit files in one `--serve` invocation errors.
- [x] Duplicate/aliased explicit files (including symlink aliases) error.
- [x] An explicit multi-file set with no real common ancestor (e.g. files from unrelated top-level directories) errors.
- [x] Explicit multi-file serve renders navigation.
- [x] Explicit multi-file serve opens the first input at `/`.
- [x] Directory serve chooses `README.md` first.
- [x] Directory serve chooses `index.md` when no `README.md` exists.
- [x] Directory serve falls back to first alphabetical Markdown file.
- [x] Directory serve ignores nested Markdown files.
- [x] Directory serve loads referenced assets under root.
- [x] Directory/explicit serve returns `404` for a real, unreferenced file under root (proves the allowlist, not open static serving).
- [x] Unknown routes return `404`.
- [x] Traversal attempts — literal `..`, percent-encoded, and symlink-based — all return `404`, never `403`.
- [x] Non-`GET`/`HEAD` requests return `405`.
- [x] An `.html`/`.svg` asset under root is served with a neutralized content-type, not inline-renderable.
- [x] Same-root Markdown links are rewritten to served routes.
- [x] Markdown link fragments are preserved.
- [x] Outside-root links are preserved.
- [x] A nested (non-top-level) Markdown link in directory mode is left unrewritten.
- [x] Live reload preserves scroll position.
- [x] Live reload updates title/nav/footer shell regions.

## Implementation Order

- [x] Fix current served footer visibility by making reload update shell chrome (commit to the auto-refresh path — no "restart required" fallback).
- [x] Remove the legacy `--serve <PORT> <FILE>` positional form and add `--port <PORT>` parsing, with a clear migration error for the old syntax, and tests.
- [x] Introduce `ServeConfig` and route resolution for single-file mode, including the loopback-only bind and the `GET`/`HEAD`-only method check.
- [x] Add multi-file explicit serve support, including common-root validation and duplicate/alias rejection.
- [x] Add top-level directory discovery and default document selection, including dotfile exclusion and case-collision rejection.
- [x] Add asset serving under root as an allowlist populated by reference scanning, with the safe content-type policy.
- [x] Add served-only Markdown and asset link rewriting (documents and allowlisted assets only).
- [x] Add adaptive navigation rendering.
- [x] Upgrade live reload to preserve scroll and update shell regions.
- [x] Expand integration tests for routing, security boundaries (traversal, symlinks, method/content-type restrictions), and reload behavior.
- [x] Update `README.md` usage examples after behavior lands, calling out `--port` and the removal of the legacy positional port.
