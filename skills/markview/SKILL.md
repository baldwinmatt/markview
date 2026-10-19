---
name: markview
description: Serve a directory of Markdown as a browsable local website with live reload, using the markview CLI. Use when the user wants to preview, browse, or read docs, notes, or a Markdown tree in a browser, or asks to serve, host, or view Markdown files.
---

Serve the target with `markview --serve`, then hand the user a URL that returns 200.

## 1. Find the binary

Run `command -v markview`. If present, run this intentional-failure probe:

```sh
markview --serve --port 0 /dev/null
```

`markview: served input is not a file: /dev/null` confirms that port zero was accepted without starting a server. An old binary instead reports `invalid port: 0`. If missing or old, install a version with port-zero support:

```sh
cargo install --git https://github.com/baldwinmatt/markview
```

For an unpushed local checkout, install that checkout with `cargo install --path /abs/path/to/markview` instead. Repeat the probe after installing; report other errors before proceeding.

## 2. Check running instances

```sh
ps -eo pid,args | grep '[m]arkview.* --serve'
```

Each line shows a server's PID and target. Daemon children include a concrete `--port` in their arguments; a foreground server may omit it or show `0`, so use its startup banner for the actual port. An instance already serving the same target with the same `--recurse` setting is the one to reuse: skip to step 4 with its port. Leave unrelated instances running.

## 3. Serve

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

- Add `--open` when the user is at this machine and wants the browser launched.
- On daemon failure, the launcher points to `.markview-serve-<PORT>.log`; the real startup error is in that log. Run `cat <LOG_PATH>`, report its contents, then delete that failed log with `rm <LOG_PATH>`. Do not delete logs for running servers.
- Report errors such as a discovered symlink escaping the root, case-colliding document routes, or a bad path verbatim. Explicit file sets reject duplicate/aliased files; directory discovery silently drops aliases of files already discovered.

## 4. Verify

```sh
curl -s -o /dev/null -w "%{http_code}\n" http://localhost:<PORT>/
```

Done when `/` returns `200`. Then give the user the URL, the PID, and `kill <PID>` as the way to stop it.

## What the site contains

- For a directory, `/` selects the top-level `README.md`, else top-level `index.md` (both matched case-insensitively), else the first Markdown relative path in alphabetical byte order. For explicit files, `/` serves the first FILE argument.
- A directory containing one document has an empty navigation area; larger collections list their discovered documents.
- Discovery covers `.md`, `.markdown`, and `.mdown`. Hidden files/directories and symlinked directories are skipped for document discovery.
- Asset discovery scans only relative Markdown `![]()` and `[]()` destinations, including reference-style Markdown links. Assets must resolve under the root; referenced assets inside hidden directories are allowed. Raw HTML such as `<img src>` and absolute destinations do not add assets to the allowlist, so those files return 404 unless independently allowlisted. The favicon route has its own local-file/built-in fallback.
- Only served documents and allowlisted assets are accessible; other files return 404.
- Edits, additions, and removals on disk reach open browser tabs automatically, so a running server needs no restart.
- The server binds to `127.0.0.1`. For a remote host, the user forwards the port first: `ssh -L <PORT>:localhost:<PORT> user@host`.
