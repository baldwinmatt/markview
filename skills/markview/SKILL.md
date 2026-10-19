---
name: markview
description: Serve a directory of Markdown as a browsable local website with live reload, using the markview CLI. Use when the user wants to preview, browse, or read docs, notes, or a Markdown tree in a browser, or asks to serve, host, or view Markdown files.
---

Serve the target with `markview --serve`, then hand the user a URL that returns 200.

## 1. Find the binary

Run `command -v markview`. When it is missing, or `markview --help` does not mention `0 picks any free port`, install the current version:

```sh
cargo install --git https://github.com/baldwinmatt/markview
```

## 2. Check running instances

```sh
ps -eo pid,args | grep '[m]arkview.* --serve'
```

Each line shows a server's PID, its target, and its `--port`. An instance already serving the same target with the same `--recurse` setting is the one to reuse: skip to step 4 with its port. Leave unrelated instances running.

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
- Report startup errors verbatim: a symlink escaping the root, two files whose names differ only by case, a bad path.

## 4. Verify

```sh
curl -s -o /dev/null -w "%{http_code}\n" http://localhost:<PORT>/
```

Done when `/` returns `200`. Then give the user the URL, the PID, and `kill <PID>` as the way to stop it.

## What the site contains

- `/` is the top-level `README.md`, else the top-level `index.md`, else the first file alphabetically. Every discovered document gets a navigation entry.
- Discovery covers `.md`, `.markdown`, and `.mdown`. Hidden directories and symlinked directories are left out, and their routes return 404.
- Only discovered documents and the images or files they reference are served; any other file returns 404.
- Edits, additions, and removals on disk reach open browser tabs automatically, so a running server needs no restart.
- The server binds to `127.0.0.1`. For a remote host, the user forwards the port first: `ssh -L <PORT>:localhost:<PORT> user@host`.
