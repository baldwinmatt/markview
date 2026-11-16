---
name: markview
description: Use the Markview CLI to preview or serve Markdown with live reload, collect browser annotations, list and resolve saved review comments, or request and wait for a human verdict on one document or a collection.
---

## Choose the workflow

| User wants | Mode |
| --- | --- |
| Browse or preview Markdown, including existing comments | `--serve` |
| Leave or manage comments in a running browser session | `--serve --annotate` |
| Give a human approval or request-changes verdict for the collection | `--review` |
| Read or address saved comments | `--annotations` |

## Find the binary

Run `command -v markview` and check `markview --help` for the flags the selected
workflow needs. For serving on a free port, run this intentional-failure probe:

```sh
markview --serve --port 0 /dev/null
```

`markview: served input is not a file: /dev/null` confirms that port zero was accepted without starting a server. An old binary instead reports `invalid port: 0`. If the binary is missing or lacks the needed flags or port-zero support, install the current version:

```sh
cargo install --git https://github.com/baldwinmatt/markview
```

When working from a local Markview checkout, use its built binary or install it
with `cargo install --path /abs/path/to/markview`. Repeat the applicable checks
after installing. Report unexpected errors before proceeding.

## Serve a preview or collect comments

### Check running instances

```sh
ps -eo pid,args | grep '[m]arkview.* --serve'
```

Each line shows a server's PID and target. Daemon children include a concrete `--port` in their arguments. A foreground server may omit it or show `0`, so use its startup banner for the actual port. Reuse an instance serving the same target with the same `--recurse` and `--annotate` settings and verify its URL. Leave unrelated instances running.

### Start the server

Pick the arguments from the target:

| User wants | Arguments |
| --- | --- |
| Every Markdown file under a directory, at any depth | `--recurse <DIR>` |
| One directory, top level only | `<DIR>` |
| A hand-picked set | `<FILE> <FILE>...` |
| One document | `<FILE>` |

`--recurse` takes exactly one directory. A website, docs tree, or "everything in here" means the first row.

Launch from `/tmp/.markview`, which keeps the `.markview-serve-<PORT>.log` file out of the served tree. Pass the target as an absolute path and `--port 0`, so the OS picks a free port.

```sh
mkdir -p /tmp/.markview && cd /tmp/.markview && markview --serve --recurse --daemon --port 0 /abs/path/to/docs
```

The command prints the PID, the URL with the port it chose, and the log path. Record all three.

- Add `--annotate` when the user wants to leave review comments. Share the loopback URL using `localhost` or `127.0.0.1` with the server port to enable writes. Tell the reviewer to select rendered text and click **Comment**, or use **Comment on document** for overall feedback, then save. Comments persist on disk and can be read while the server runs or after it stops.
- Add `--open` when the user is at this machine and wants the browser launched.
- On daemon failure, the launcher points to `.markview-serve-<PORT>.log`; the real startup error is in that log. Run `cat <LOG_PATH>`, report its contents, then delete that failed log with `rm <LOG_PATH>`. Do not delete logs for running servers.
- Report errors such as a discovered symlink escaping the root, case-colliding document routes, or a bad path verbatim. Explicit file sets reject duplicate/aliased files; directory discovery silently drops aliases of files already discovered.

### Verify the URL

```sh
curl -s -o /dev/null -w "%{http_code}\n" http://localhost:<PORT>/
```

Done when `/` returns `200`. Then give the user the URL, the PID, and `kill <PID>` as the way to stop it.

### What the site contains

- For a directory, `/` selects the top-level `README.md`, else top-level `index.md` (both matched case-insensitively), else the first Markdown relative path in alphabetical byte order. For explicit files, `/` serves the first FILE argument.
- A directory containing one document has an empty navigation area; larger collections list their discovered documents.
- Discovery covers `.md`, `.markdown`, and `.mdown`. Hidden files/directories and symlinked directories are skipped for document discovery.
- Asset discovery scans only relative Markdown `![]()` and `[]()` destinations, including reference-style Markdown links. Assets must resolve under the root; referenced assets inside hidden directories are allowed, except that nothing under a `.markview` directory is ever served. Raw HTML such as `<img src>` and absolute destinations do not add assets to the allowlist, so those files return 404 unless independently allowlisted. The favicon route has its own local-file/built-in fallback.
- Only served documents and allowlisted assets are accessible; other files return 404.
- Edits, additions, and removals on disk reach open browser tabs automatically, so a running server needs no restart.
- The server binds to `127.0.0.1`. For a remote host, the user forwards the port first: `ssh -L <PORT>:localhost:<PORT> user@host`.


## Read and address saved comments

Run `markview --annotations <FILE or DIR>...` for open comments, current
line labels and the store paths. Pass the source Markdown paths. Directory
inputs are always recursive. Pass explicit files to match a nonrecursive
serve or review session. Use `--include-resolved` to include completed comments.

For a request to address feedback, use the annotation ID together with its store
path. Read the comment and source before editing. Use the quoted text and heading
as context. A detached comment still needs attention and its line label is only
the last known location, so confirm the target from context before making changes.
Keep comments open when their request is ambiguous or the work is incomplete.

After addressing a comment, re-read its store immediately before editing and
confirm the ID, body and status still match the comment you addressed. Change
its section header from `[open]` to `[resolved]`. Preserve the anchor, metadata,
body, unrelated sections and any user-written preamble. If the store changed,
re-list the comments and use the current version.

Header lines must stay exactly `## a<N> [open|resolved] <anchor>`, where the
anchor is `line <n>`, `lines <n>-<m>` or `document`. Change only the word in
the brackets. Do not change its case (`[Resolved]`, `[OPEN]`), add a second
space anywhere, leave a trailing space, or renumber the ID: any of those turns
the header into comment text and the annotation disappears from the list.
Re-run `markview --annotations --include-resolved` against the affected source
files and confirm the intended IDs are listed as resolved. Check stderr and
the exit status. A malformed header can produce a warning, and a read error
can produce partial output with a nonzero exit status.

## Request a human verdict

For an explicit collection verdict, start `markview --review [--recurse] <FILE>... | <DIR>`
as a foreground process and keep it alive while the reviewer works. Read stderr
for the browser URL and share it with the reviewer as soon as it appears. Use a
process session you can poll while waiting. The browser opens by default and the
port is chosen automatically. For example:

```sh
markview --review /abs/path/to/notes.md /abs/path/to/api.md
markview --review --recurse /abs/path/to/docs
markview --review --no-open --port 8080 /abs/path/to/notes.md
```

Use `--no-open --port <PORT>` over SSH and forward the same port locally and
remotely. Review mode enables annotation writes automatically.

Ask the reviewer to save their comments and confirm **Approve** or **Request
changes** for the entire served collection. Request changes needs at least one
saved open comment. Approval can include open comments. If the collection changes
during confirmation, have the reviewer check the refreshed counts and confirm again.

Capture stdout through completion. Read `Review: approved` or
`Review: changes requested` to determine the verdict. Both verdicts exit 0 by
default. Use `--gate` when Request changes should exit 2. Report the verdict and
the saved open comments in the accepted snapshot, including any that predate the
session. If asked to address them, re-list the current comments before editing and
follow the saved-comments workflow above.

If the process is interrupted, recover saved comments with `markview --annotations`
for the same document scope and report that the session ended without a verdict.
For a top-level-only directory review, pass just the reviewed files explicitly.
