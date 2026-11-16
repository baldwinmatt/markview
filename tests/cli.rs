use assert_cmd::cargo::cargo_bin;
use assert_cmd::Command;
use predicates::prelude::*;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[test]
fn renders_markdown_file_without_color() {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("sample.md");
    std::fs::write(
        &file,
        "# Markview\n\nA [tiny](https://example.com) viewer.\n",
    )
    .expect("write sample");

    let mut cmd = Command::cargo_bin("markview").expect("binary");
    cmd.arg("--no-color")
        .arg(&file)
        .assert()
        .success()
        .stdout(predicate::str::contains("# Markview"))
        .stdout(predicate::str::contains(
            "tiny (https://example.com) viewer.",
        ));
}

#[test]
fn renders_stdin() {
    let mut cmd = Command::cargo_bin("markview").expect("binary");
    cmd.arg("--no-color")
        .write_stdin("- portable\n- fast\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("- portable"))
        .stdout(predicate::str::contains("- fast"));
}

#[test]
fn renders_html_file() {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("sample.md");
    std::fs::write(&file, "# Markview\n\nExported **HTML**.\n").expect("write sample");

    let mut cmd = Command::cargo_bin("markview").expect("binary");
    cmd.arg("--html")
        .arg(&file)
        .assert()
        .success()
        .stdout(predicate::str::contains("<!doctype html>"))
        .stdout(predicate::str::contains("<title>sample.md</title>"))
        .stdout(predicate::str::contains(
            r#"<h1 id="markview">Markview</h1>"#,
        ))
        .stdout(predicate::str::contains("<strong>HTML</strong>"));
}

#[test]
fn html_export_has_code_blocks_without_copy_or_review_controls() {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("sample.md");
    std::fs::write(
        &file,
        "# Code\n\n```rust\nlet a = 1;\n```\n\n    indented\n",
    )
    .unwrap();
    let output = Command::cargo_bin("markview")
        .unwrap()
        .arg("--html")
        .arg(&file)
        .output()
        .unwrap();
    let html = String::from_utf8(output.stdout).unwrap();
    assert_eq!(html.matches("<pre><code").count(), 2, "{html}");
    // An exported file has no server and no script: nothing could copy.
    for control in [
        "markview-copy",
        "markview-code-block",
        "/_markview/",
        "<script",
    ] {
        assert!(!html.contains(control), "{control}");
    }
}

#[test]
fn renders_html_stdin() {
    let mut cmd = Command::cargo_bin("markview").expect("binary");
    cmd.arg("--html")
        .write_stdin("# Piped\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("<title>Untitled Markdown</title>"))
        .stdout(predicate::str::contains(r#"<h1 id="piped">Piped</h1>"#));
}

#[cfg(unix)]
#[test]
fn skill_port_zero_probe_reaches_input_validation_without_starting_server() {
    Command::cargo_bin("markview")
        .unwrap()
        .args(["--serve", "--port", "0", "/dev/null"])
        .assert()
        .failure()
        .stdout("")
        .stderr("markview: served input is not a file: /dev/null\n");
}

#[test]
fn prints_help() {
    let mut cmd = Command::cargo_bin("markview").expect("binary");
    cmd.arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage: markview"));
}

#[test]
fn reports_invalid_width() {
    let mut cmd = Command::cargo_bin("markview").expect("binary");
    cmd.args(["--width", "nope"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid width: nope"));
}

#[test]
fn annotations_cli_lists_open_detached_and_resolved_for_files_and_directories() {
    let dir = tempfile::tempdir().unwrap();
    let docs = dir.path().join("docs");
    let store = dir.path().join(".markview/annotations/docs");
    std::fs::create_dir_all(&docs).unwrap();
    std::fs::create_dir_all(&store).unwrap();
    let file = docs.join("spec.md");
    let closed = docs.join("closed.md");
    std::fs::write(&file, "# Routing\n\n\nFresh **source**.\n").unwrap();
    std::fs::write(&closed, "# Closed").unwrap();
    std::fs::write(
        store.join("spec.md"),
        concat!(
            "## a2 [open] line 9\n\n> gone\n\nDetached body\n\n",
            "## a3 [open] document\n\nDocument body\n\n",
            "## a1 [open] line 3\n\n> Fresh source\n\nOpen body\n\n",
            "## a4 [resolved] document\n\nClosed body\n",
        ),
    )
    .unwrap();
    std::fs::write(
        store.join("closed.md"),
        "## a1 [resolved] document\n\nClosed only\n",
    )
    .unwrap();

    let mut cmd = Command::cargo_bin("markview").unwrap();
    cmd.arg("--annotations")
        .arg(&docs)
        .assert()
        .success()
        .stdout(predicate::str::contains("## a1 · line 4 · Routing"))
        .stdout(predicate::str::contains("## a2 · line 9 · detached"))
        .stdout(predicate::str::contains("## a3 · document"))
        .stdout(predicate::str::contains("No open annotations for "))
        .stdout(predicate::str::contains("1 resolved annotation not shown"))
        .stdout(predicate::str::contains("Closed body").not());

    let mut cmd = Command::cargo_bin("markview").unwrap();
    let output = cmd
        .args(["--annotations", "--include-resolved"])
        .arg(&file)
        .arg(&closed)
        .output()
        .unwrap();
    assert!(output.status.success());
    let out = String::from_utf8(output.stdout).unwrap();
    assert!(out.find("Open body").unwrap() < out.find("Detached body").unwrap());
    assert!(out.find("Detached body").unwrap() < out.find("Closed body").unwrap());
    assert!(out.contains("## a4 · document · resolved"));
    assert!(out.contains("Closed only"));
    assert!(!out.contains("not shown"));

    for flag in ["--html", "--no-color"] {
        Command::cargo_bin("markview")
            .unwrap()
            .arg(flag)
            .arg(&file)
            .assert()
            .success()
            .stdout(predicate::str::contains("Open body").not());
    }
}

/// Runs `markview --annotations` in `cwd`, returning (stdout, stderr, success).
fn list_annotations_in(cwd: &std::path::Path, args: &[&str]) -> (String, String, bool) {
    let output = std::process::Command::new(cargo_bin("markview"))
        .arg("--annotations")
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.success(),
    )
}

#[test]
fn annotations_cli_prints_the_spec_layout_in_document_order() {
    let dir = tempfile::tempdir().unwrap();
    let docs = dir.path().join("docs");
    let store = dir.path().join(".markview/annotations/docs");
    std::fs::create_dir_all(&docs).unwrap();
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(
        docs.join("spec.md"),
        "# Routing\n\nFirst line.\n\nFresh **source**.\n",
    )
    .unwrap();
    // Stored out of order: a detached comment, a document comment, a text
    // comment, a resolved one, and a second text comment earlier in the text.
    std::fs::write(
        store.join("spec.md"),
        concat!(
            "## a4 [open] line 9\n\n> gone\n\nDetached body\n\n",
            "## a3 [open] document\n\nDocument body\n\n",
            "## a1 [open] line 3\n\n> Fresh source\n\nSecond in the text\n\n",
            "## a5 [resolved] document\n\nClosed body\n\n",
            "## a2 [open] line 3\n\n> First line\n\nFirst in the text\n",
        ),
    )
    .unwrap();

    let (stdout, stderr, success) = list_annotations_in(dir.path(), &["docs/spec.md"]);
    assert!(success, "{stderr}");
    assert_eq!(stderr, "");
    // Text comments in document order, then document comments, then detached.
    assert_eq!(
        stdout,
        concat!(
            "# Annotations: docs/spec.md\n",
            "Store: .markview/annotations/docs/spec.md (change `[open]` to `[resolved]` there once addressed)\n",
            "\n",
            "## a2 · line 3 · Routing\n> First line\n\nFirst in the text\n\n",
            "## a1 · line 5 · Routing\n> Fresh source\n\nSecond in the text\n\n",
            "## a3 · document\n\nDocument body\n\n",
            "## a4 · line 9 · detached\n> gone\n\nDetached body\n\n",
            "1 resolved annotation not shown.\n\n",
        )
    );

    // The same order reaches the browser panel through the JSON endpoint.
    let server = ServeProcess::start(&docs.join("spec.md"));
    let json = http_get_local(server.port, "/_markview/annotations?doc=%2Fspec.md");
    let ids: Vec<_> = json
        .split("{\"id\":\"")
        .skip(1)
        .map(|rest| rest.split('"').next().unwrap())
        .collect();
    assert_eq!(ids, ["a2", "a1", "a3", "a4", "a5"]);
}

#[test]
fn annotations_cli_keeps_listing_past_a_broken_document() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    let docs = dir.path().join("docs");
    let store = dir.path().join(".markview/annotations/docs");
    std::fs::create_dir_all(&docs).unwrap();
    std::fs::create_dir_all(&store).unwrap();
    let section = "## a1 [open] document\n\nNote\n";
    for name in [
        "a-good.md",
        "b-not-utf8.md",
        "c-symlink.md",
        "d-directory.md",
        "f-good.md",
    ] {
        std::fs::write(docs.join(name), "# Doc\n").unwrap();
    }
    std::fs::write(docs.join("e-bad-source.md"), b"# Doc \xff\n").unwrap();
    for name in ["a-good.md", "e-bad-source.md", "f-good.md"] {
        std::fs::write(store.join(name), section).unwrap();
    }
    std::fs::write(
        store.join("b-not-utf8.md"),
        b"## a1 [open] document\n\n\xff\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("elsewhere.md"), section).unwrap();
    symlink(dir.path().join("elsewhere.md"), store.join("c-symlink.md")).unwrap();
    std::fs::create_dir(store.join("d-directory.md")).unwrap();

    let (stdout, stderr, success) = list_annotations_in(dir.path(), &["docs"]);
    assert!(!success, "a listing with errors must not exit 0");
    // The good neighbours on both sides of the broken ones are listed.
    let listed: Vec<_> = stdout
        .lines()
        .filter(|line| line.starts_with("# Annotations: "))
        .collect();
    assert_eq!(
        listed,
        [
            "# Annotations: docs/a-good.md",
            "# Annotations: docs/f-good.md"
        ]
    );
    // One line per broken document, naming it and what is wrong with it.
    let errors: Vec<_> = stderr.lines().collect();
    assert_eq!(errors.len(), 4, "{stderr}");
    for (line, document, problem) in [
        (
            errors[0],
            "docs/b-not-utf8.md",
            "annotation store is not UTF-8",
        ),
        (
            errors[1],
            "docs/c-symlink.md",
            ".markview/annotations/docs/c-symlink.md",
        ),
        (
            errors[2],
            "docs/d-directory.md",
            ".markview/annotations/docs/d-directory.md",
        ),
        (errors[3], "docs/e-bad-source.md", "UTF-8"),
    ] {
        assert!(line.starts_with(&format!("error: {document}: ")), "{line}");
        assert!(line.contains(problem), "{line}");
    }

    // Explicit files behave the same way.
    let (stdout, stderr, success) =
        list_annotations_in(dir.path(), &["docs/c-symlink.md", "docs/f-good.md"]);
    assert!(!success);
    assert!(stdout.contains("# Annotations: docs/f-good.md"));
    assert!(stderr.starts_with("error: docs/c-symlink.md: "), "{stderr}");
}

#[test]
fn annotations_cli_warns_about_headers_that_almost_parse() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join(".markview/annotations");
    std::fs::create_dir_all(&store).unwrap();
    for name in ["near.md", "none.md", "twice.md", "fine.md"] {
        std::fs::write(dir.path().join(name), "# Doc\n").unwrap();
    }
    std::fs::write(
        store.join("near.md"),
        concat!(
            "## a1 [OPEN] document\n\nUpper case status\n\n",
            "## a2 [open] document \n\nTrailing space\n\n",
            "## a3  [open] document\n\nTwo spaces\n\n",
            "## a4 [Resolved] document\n\nCapitalised status\n\n",
            "## a5 [open] document\n\nThe only real one\n\n",
            "\\## a6 [OPEN] document\n\n## a11y notes are just a heading\n",
        ),
    )
    .unwrap();
    std::fs::write(
        store.join("none.md"),
        "### a1 [open] document\n\nWrong level\n",
    )
    .unwrap();
    std::fs::write(
        store.join("twice.md"),
        "## a1 [open] document\n\nOne\n\n## a1 [open] document\n\nTwo\n",
    )
    .unwrap();
    std::fs::write(store.join("fine.md"), "## a1 [open] document\n\nFine\n").unwrap();

    let (stdout, stderr, success) = list_annotations_in(dir.path(), &["near.md"]);
    assert!(success, "{stderr}");
    assert!(stdout.contains("## a5 · document"));
    assert!(!stdout.contains("## a1 · "));
    let warnings: Vec<_> = stderr.lines().collect();
    assert_eq!(warnings.len(), 4, "{stderr}");
    for (warning, line, header) in [
        (warnings[0], 1, "## a1 [OPEN] document"),
        (warnings[1], 5, "## a2 [open] document "),
        (warnings[2], 9, "## a3  [open] document"),
        (warnings[3], 13, "## a4 [Resolved] document"),
    ] {
        assert!(
            warning.starts_with(&format!(
                "warning: .markview/annotations/near.md:{line}: \"{header}\" is not an annotation header"
            )),
            "{warning}"
        );
        assert!(
            warning.contains("## a<N> [open|resolved] <anchor>"),
            "{warning}"
        );
    }

    let (stdout, stderr, success) = list_annotations_in(dir.path(), &["none.md"]);
    assert!(success);
    assert!(stdout.contains("No open annotations for none.md."));
    assert_eq!(
        stderr,
        "warning: .markview/annotations/none.md: the store has content but no annotation sections; a section starts with a line like \"## a1 [open] document\"\n"
    );

    // Two sections with one ID cannot be told apart, so neither is printed.
    let (stdout, stderr, success) = list_annotations_in(dir.path(), &["twice.md", "fine.md"]);
    assert!(!success);
    assert!(!stdout.contains("# Annotations: twice.md"));
    assert!(stdout.contains("# Annotations: fine.md"));
    assert_eq!(
        stderr,
        "error: twice.md: .markview/annotations/twice.md: annotation a1 appears more than once; give each section a unique ID\n"
    );
}

#[test]
fn annotations_cli_refuses_a_store_file_as_input() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join(".markview/annotations/docs");
    std::fs::create_dir_all(dir.path().join("docs")).unwrap();
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(dir.path().join("docs/spec.md"), "# Doc\n").unwrap();
    std::fs::write(store.join("spec.md"), "## a1 [open] document\n\nNote\n").unwrap();

    let input = ".markview/annotations/docs/spec.md";
    let (stdout, stderr, success) = list_annotations_in(dir.path(), &[input, "docs/spec.md"]);
    assert!(!success);
    assert_eq!(
        stderr,
        format!("error: {input}: this is an annotation store, not a document; pass the Markdown file it annotates\n")
    );
    // The real document is still listed, and nothing was resolved under the store.
    assert_eq!(stdout.matches("# Annotations: ").count(), 1);
    assert!(stdout.contains("# Annotations: docs/spec.md"));
}

#[test]
fn annotations_cli_reports_flag_conflicts_before_missing_inputs() {
    for (args, flag) in [
        (&["--annotations", "--serve"][..], "--serve"),
        (&["--annotations", "--daemon"], "--daemon"),
        (&["--annotations", "--recurse", "docs"], "--recurse"),
        (&["--annotations", "--open", "one.md"], "--open"),
        (&["--annotations", "--port", "0", "one.md"], "--port"),
        (&["--annotations", "--annotate", "one.md"], "--annotate"),
        (&["--annotations", "--html", "one.md"], "--html"),
        (&["--annotations", "--width", "60", "one.md"], "--width"),
        (&["--annotations", "-w", "60", "one.md"], "--width"),
        (&["--daemon", "--annotations", "--serve"], "--serve"),
    ] {
        let output = std::process::Command::new(cargo_bin("markview"))
            .args(args)
            .output()
            .unwrap();
        assert!(!output.status.success(), "{args:?}");
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            format!("markview: --annotations cannot be combined with {flag}\n"),
            "{args:?}"
        );
    }
    let help = std::process::Command::new(cargo_bin("markview"))
        .arg("--help")
        .output()
        .unwrap();
    let help = String::from_utf8(help.stdout).unwrap();
    // Every description starts in the same column, 25 characters in.
    assert!(help.contains(concat!(
        "      --annotations      List open comments for Markdown files or directories\n",
        "      --include-resolved\n",
        "                         Include resolved comments after open comments\n",
    )));
}

#[test]
fn annotations_cli_validates_arguments() {
    for (args, error) in [
        (&["--include-resolved"][..], "requires --annotations"),
        (&["--annotations", "--html", "one.md"], "cannot be combined"),
        (
            &["--annotations", "--serve", "one.md"],
            "cannot be combined",
        ),
        (&["--annotations"], "missing value"),
        (&["--annotate", "one.md"], "requires --serve"),
    ] {
        Command::cargo_bin("markview")
            .unwrap()
            .args(args)
            .assert()
            .failure()
            .stderr(predicate::str::contains(error));
    }
    let dir = tempfile::tempdir().unwrap();
    let invalid = dir.path().join("plain.txt");
    std::fs::write(&invalid, "not Markdown").unwrap();
    Command::cargo_bin("markview")
        .unwrap()
        .arg("--annotations")
        .arg(invalid)
        .assert()
        .failure()
        .stderr(predicate::str::contains("input is not Markdown"));
    // A missing input is an error in the usual `markview: ...` style.
    let missing = dir.path().join("missing.md");
    Command::cargo_bin("markview")
        .unwrap()
        .arg("--annotations")
        .arg(&missing)
        .assert()
        .failure()
        .stdout("")
        .stderr(predicate::str::starts_with(format!(
            "markview: {}: ",
            missing.display()
        )));
}

#[test]
fn serve_mode_returns_rendered_html() {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("README.md");
    let mojibake_dash = "\u{00E2}\u{0080}\u{0094}";
    std::fs::write(
        &file,
        format!("# Served\n\nRemote **view** {mojibake_dash} clean text.\n"),
    )
    .expect("write sample");
    let mut server = ServeProcess::start(&file);

    let response = http_get(server.port, "/");

    assert!(response.contains("HTTP/1.1 200 OK"));
    assert!(response.contains("Content-Type: text/html; charset=utf-8"));
    assert!(response.contains(r#"<h1 id="served">Served</h1>"#));
    assert!(response.contains("<strong>view</strong>"));
    assert!(response.contains("&#8212; clean text"));
    assert!(!response.contains("&#226;&#128;&#148; clean text"));
    assert!(!response.contains("— clean text"));
    assert!(response.contains(r#"<div class="markview-shell" data-nav="none">"#));
    assert!(response
        .contains(r#"Served by <a href="https://github.com/baldwinmatt/markview">markview</a>"#));
    assert!(response.contains(r#"<footer class="markview-footer">"#));
    assert!(response.contains(r#"class="markview-footer-meta""#));
    assert!(response.contains(r#"class="markview-modified""#));
    assert!(response.contains("Doc modified <time data-markview-modified-at data-timestamp-ms="));
    assert!(response.contains(r#"class="markview-refreshed""#));
    assert!(response.contains("Last refreshed <time data-markview-refreshed-at>"));
    assert!(response.contains("new EventSource('/events')"));
    assert!(response.contains("updateModifiedTimes();"));
    assert!(response.contains("updateRefreshTime();"));
    server.stop();
}

#[test]
fn serve_mode_streams_reload_events_when_file_changes() {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("README.md");
    std::fs::write(&file, "# Before\n").expect("write sample");
    let mut server = ServeProcess::start(&file);
    let mut stream = TcpStream::connect(("127.0.0.1", server.port)).expect("connect events");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set timeout");
    stream
        .write_all(b"GET /events HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .expect("write request");

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    loop {
        line.clear();
        reader.read_line(&mut line).expect("read header");
        if line == "\r\n" {
            break;
        }
    }

    std::fs::write(&file, "# After\n").expect("modify sample");
    let event = read_until(&mut reader, "data: reload", Duration::from_secs(5));

    assert!(event.contains("data: reload"));
    server.stop();
}

#[test]
fn serve_mode_reload_path_preserves_scroll_and_refreshes_shell_regions() {
    let dir = tempfile::tempdir().expect("temp dir");
    let first = dir.path().join("README.md");
    let second = dir.path().join("second.md");
    std::fs::write(&first, "# Before\n\nContent\n").expect("write first");
    std::fs::write(&second, "# Neighbor\n").expect("write second");
    let mut server = ServeProcess::start_with_files(&[first.as_path(), second.as_path()]);

    let before = http_get(server.port, "/");
    assert!(before.contains("const savedY = window.scrollY"));
    assert!(before.contains("window.scrollTo(0, savedY)"));
    assert!(before.contains("data-markview-nav"));
    assert!(before.contains("Served by"));
    assert!(before.contains("Doc modified"));
    assert!(before.contains("Last refreshed"));

    std::fs::write(&first, "# After\n\nContent\n").expect("update first title");
    let after = http_get(server.port, "/");
    assert!(after.contains("<title>After</title>"));
    assert!(after.contains(">After</a>"));
    assert!(after.contains("Served by"));
    assert!(after.contains("Doc modified"));
    assert!(after.contains("Last refreshed"));
    server.stop();
}

#[test]
fn serve_mode_reload_path_restores_collapsed_sidebar_dirs() {
    let dir = tempfile::tempdir().expect("temp dir");
    let guides = dir.path().join("guides");
    std::fs::create_dir(&guides).expect("guides dir");
    std::fs::write(dir.path().join("README.md"), "# Home\n").expect("write readme");
    std::fs::write(guides.join("setup.md"), "# Setup\n").expect("write setup");
    let mut server = ServeProcess::start_recursive_dir(dir.path());

    let setup = http_get(server.port, "/guides/setup.md");

    assert!(setup.contains(r#"data-markview-dir="/guides" open"#));
    assert!(setup.contains("el.open = openDirs.has(el.dataset.markviewDir);"));
    server.stop();
}

#[test]
fn serve_mode_does_not_log_noise_when_an_events_client_disconnects() {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("README.md");
    std::fs::write(&file, "# Before\n").expect("write sample");
    let mut server = ServeProcess::start_with_disconnect_logging(&file);

    // Connect to the reload stream, then drop it abruptly (like a closed
    // browser tab or tunnel) so the server's next writes to it fail.
    {
        let mut stream = TcpStream::connect(("127.0.0.1", server.port)).expect("connect events");
        stream
            .write_all(b"GET /events HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .expect("write request");
        let mut reader = BufReader::new(&stream);
        let mut line = String::new();
        loop {
            line.clear();
            reader.read_line(&mut line).expect("read header");
            if line == "\r\n" {
                break;
            }
        }
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut attempt = 0;
    while Instant::now() < deadline {
        attempt += 1;
        std::fs::write(&file, format!("# After {attempt}\n")).expect("trigger reload broadcast");
        std::thread::sleep(Duration::from_millis(100));
        if server
            .stderr_so_far()
            .contains("markview: /events client disconnected")
        {
            break;
        }
    }

    let stderr = server.stderr_so_far();
    assert!(
        stderr.contains("markview: /events client disconnected"),
        "disconnect path was not exercised; stderr: {stderr}"
    );
    assert!(
        !stderr.contains("serve error"),
        "unexpected server error output: {stderr}"
    );
    server.stop();
}

#[test]
fn serve_mode_returns_404_for_unknown_routes() {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("README.md");
    std::fs::write(&file, "# Served\n").expect("write sample");
    let mut server = ServeProcess::start(&file);

    let response = http_get(server.port, "/missing");

    assert!(response.contains("HTTP/1.1 404 Not Found"));
    assert!(response.contains("Not found"));
    server.stop();
}

#[test]
fn serve_mode_serves_a_built_in_favicon_when_none_is_referenced() {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("README.md");
    std::fs::write(&file, "# Served\n").expect("write sample");
    let mut server = ServeProcess::start(&file);

    let response = http_get(server.port, "/favicon.ico");

    assert!(response.contains("HTTP/1.1 200 OK"));
    assert!(response.contains("Content-Type: image/png"));
    server.stop();
}

#[test]
fn serve_mode_serves_an_unreferenced_favicon_from_the_served_directory() {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(dir.path().join("README.md"), "# Served\n").expect("write sample");
    std::fs::write(dir.path().join("favicon.ico"), "REAL-FAVICON-ON-DISK").expect("write favicon");
    let mut server = ServeProcess::start_dir(dir.path());

    let response = http_get(server.port, "/favicon.ico");

    assert!(response.contains("HTTP/1.1 200 OK"));
    assert!(
        response.contains("REAL-FAVICON-ON-DISK"),
        "expected the real on-disk favicon to be served, got: {response}"
    );
    assert!(
        !response.contains("Content-Length: 5980"),
        "expected the real favicon's length, not the built-in fallback's: {response}"
    );
    server.stop();
}

#[test]
fn serve_mode_serves_a_root_relative_referenced_favicon_from_the_served_directory() {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(
        dir.path().join("README.md"),
        "# Served\n\n![icon](/favicon.ico)\n",
    )
    .expect("write sample");
    std::fs::write(dir.path().join("favicon.ico"), "REFERENCED-FAVICON-BYTES")
        .expect("write favicon");
    let mut server = ServeProcess::start_dir(dir.path());

    let response = http_get(server.port, "/favicon.ico");

    assert!(response.contains("HTTP/1.1 200 OK"));
    assert!(
        response.contains("REFERENCED-FAVICON-BYTES"),
        "expected the real on-disk favicon to be served, got: {response}"
    );
    server.stop();
}

#[test]
fn serve_mode_serves_the_built_in_favicon_for_a_percent_encoded_request() {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("README.md");
    std::fs::write(&file, "# Served\n").expect("write sample");
    let mut server = ServeProcess::start(&file);

    // "%66" decodes to "f", so this is the same logical request as
    // /favicon.ico, just percent-encoded like any other route already
    // tolerates.
    let response = http_get(server.port, "/%66avicon.ico");

    assert!(
        response.contains("HTTP/1.1 200 OK"),
        "expected the built-in favicon fallback, got: {response}"
    );
    server.stop();
}

#[test]
fn serve_mode_reports_port_in_use() {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("README.md");
    std::fs::write(&file, "# Served\n").expect("write sample");
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind test port");
    let port = listener.local_addr().expect("local addr").port();

    let mut cmd = Command::cargo_bin("markview").expect("binary");
    cmd.args(["--serve"])
        .arg(&file)
        .args(["--port", &port.to_string()])
        .assert()
        .failure()
        .stderr(predicate::str::contains(format!(
            "port {port} is already in use"
        )));
}

#[test]
fn serve_mode_accepts_port_flag_and_rejects_legacy_port_form() {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("README.md");
    std::fs::write(&file, "# Served\n").expect("write sample");
    let port = unused_port();

    let mut server = ServeProcess::start_with_args(&[file.as_path()], port);
    assert!(http_get(server.port, "/").contains("HTTP/1.1 200 OK"));
    server.stop();

    let mut cmd = Command::cargo_bin("markview").expect("binary");
    cmd.args(["--serve", "8080"])
        .arg(&file)
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "replaced by `--serve FILE --port PORT`",
        ));
}

#[test]
fn serve_mode_accepts_single_numeric_named_input_without_legacy_rejection() {
    // A single all-digits *argument string* after --serve (no second positional
    // arg) is ambiguous with a real file/directory that happens to be named e.g.
    // "2024" — unlike the two-argument `--serve PORT FILE` legacy form, it must
    // not be rejected with the legacy-form migration message. The argument has to
    // be passed as a bare relative name (not an absolute temp-dir path) so the
    // literal CLI argument string is actually all-digits.
    let dir = tempfile::tempdir().expect("temp dir");
    let numbered_dir = dir.path().join("2024");
    std::fs::create_dir(&numbered_dir).expect("create numbered dir");
    std::fs::write(numbered_dir.join("README.md"), "# 2024 Notes\n").expect("write readme");

    let mut server = ServeProcess::start_relative(dir.path(), "2024");
    assert!(http_get(server.port, "/").contains(r#"<h1 id="2024-notes">2024 Notes</h1>"#));
    server.stop();
}

#[test]
fn serve_mode_rejects_invalid_port_values() {
    let mut cmd = Command::cargo_bin("markview").expect("binary");
    cmd.args(["--serve", "README.md", "--port", "nope"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid port: nope"));

    let mut cmd = Command::cargo_bin("markview").expect("binary");
    cmd.args(["--serve", "README.md", "--port", "65536"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid port: 65536"));
}

#[test]
fn serve_mode_rejects_mixed_directory_and_files() {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("extra.md");
    std::fs::write(&file, "# Extra\n").expect("write sample");

    let mut cmd = Command::cargo_bin("markview").expect("binary");
    cmd.args(["--serve"])
        .arg(dir.path())
        .arg(&file)
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "cannot mix a served directory with explicit files",
        ));
}

#[cfg(unix)]
#[test]
fn serve_mode_rejects_duplicate_aliased_explicit_files() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("one.md");
    let alias = dir.path().join("alias.md");
    std::fs::write(&file, "# One\n").expect("write sample");
    symlink(&file, &alias).expect("symlink alias");

    let mut cmd = Command::cargo_bin("markview").expect("binary");
    cmd.args(["--serve"])
        .arg(&file)
        .arg(&alias)
        .args(["--port", &unused_port().to_string()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("duplicate or aliased"));
}

#[test]
fn explicit_multi_file_serve_renders_navigation_and_defaults_to_first_input() {
    let dir = tempfile::tempdir().expect("temp dir");
    let first = dir.path().join("first.md");
    let second = dir.path().join("second.md");
    std::fs::write(&first, "# First\n\n[Second](second.md#part)\n").expect("write first");
    std::fs::write(&second, "# Second\n\n## Part\n").expect("write second");
    let mut server = ServeProcess::start_with_files(&[first.as_path(), second.as_path()]);

    let root = http_get(server.port, "/");
    let second_response = http_get(server.port, "/second.md");

    assert!(root.contains(r#"<h1 id="first">First</h1>"#));
    assert!(root.contains("data-markview-nav"));
    assert!(root.contains(r#"class="markview-tabs""#));
    assert!(root.contains(r#"<div class="markview-shell" data-nav="tabs">"#));
    assert!(root.contains(r#"href="/second.md#part""#));
    assert!(second_response.contains(r#"<h1 id="second">Second</h1>"#));
    server.stop();
}

#[test]
fn explicit_multi_file_serve_does_not_force_rescan_reload_for_unserved_markdown() {
    let dir = tempfile::tempdir().expect("temp dir");
    let first = dir.path().join("first.md");
    let second = dir.path().join("second.md");
    let unserved = dir.path().join("unserved.md");
    std::fs::write(&first, "# First\n").expect("write first");
    std::fs::write(&second, "# Second\n").expect("write second");
    let mut server = ServeProcess::start_with_files(&[first.as_path(), second.as_path()]);

    assert!(http_get(server.port, "/unserved.md").contains("HTTP/1.1 404 Not Found"));
    let mut reader = open_reload_stream(server.port);
    std::fs::write(&unserved, "# Unserved\n").expect("write unserved");
    let event = read_until(&mut reader, "data:", Duration::from_secs(5));

    assert!(
        event.contains("data: reload"),
        "unexpected event stream: {event}"
    );
    assert!(
        !event.contains("data: rescan"),
        "unserved file forced a structural reload: {event}"
    );
    assert!(http_get(server.port, "/unserved.md").contains("HTTP/1.1 404 Not Found"));
    server.stop();
}

#[test]
fn directory_serve_selects_default_documents_and_ignores_nested_markdown() {
    let dir = tempfile::tempdir().expect("temp dir");
    let nested = dir.path().join("nested");
    std::fs::create_dir(&nested).expect("nested dir");
    std::fs::write(nested.join("hidden.md"), "# Nested\n").expect("write nested");
    std::fs::write(dir.path().join("zeta.md"), "# Zeta\n").expect("write zeta");
    std::fs::write(dir.path().join("index.md"), "# Index\n").expect("write index");
    std::fs::write(dir.path().join("README.md"), "# Readme\n").expect("write readme");
    let mut server = ServeProcess::start_dir(dir.path());

    let readme = http_get(server.port, "/");
    assert!(readme.contains(r#"<h1 id="readme">Readme</h1>"#));
    assert!(readme.contains(r#"<div class="markview-shell" data-nav="sidebar">"#));
    assert!(readme.contains(r#"class="markview-sidebar""#));
    assert!(http_get(server.port, "/nested/hidden.md").contains("HTTP/1.1 404 Not Found"));
    server.stop();

    std::fs::remove_file(dir.path().join("README.md")).expect("remove readme");
    let mut server = ServeProcess::start_dir(dir.path());
    assert!(http_get(server.port, "/").contains(r#"<h1 id="index">Index</h1>"#));
    server.stop();

    std::fs::remove_file(dir.path().join("index.md")).expect("remove index");
    let mut server = ServeProcess::start_dir(dir.path());
    assert!(http_get(server.port, "/").contains(r#"<h1 id="zeta">Zeta</h1>"#));
    server.stop();
}

#[test]
fn directory_serve_selects_lowercase_readme_as_default_document() {
    // Duplicate-detection above the default-document selection already treats
    // filenames case-insensitively, so a lone lowercase "readme.md" (no
    // differently-cased sibling, hence no collision) should be recognized as
    // the index page too, not skipped in favor of the first alphabetical file.
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(dir.path().join("aaa.md"), "# Aaa\n").expect("write aaa");
    std::fs::write(dir.path().join("readme.md"), "# Readme\n").expect("write readme");
    let mut server = ServeProcess::start_dir(dir.path());

    assert!(http_get(server.port, "/").contains(r#"<h1 id="readme">Readme</h1>"#));
    server.stop();
}

#[test]
fn serve_mode_recurse_discovers_nested_markdown_files_and_prefers_top_level_default() {
    let dir = tempfile::tempdir().expect("temp dir");
    let nested = dir.path().join("nested");
    std::fs::create_dir(&nested).expect("nested dir");
    std::fs::write(dir.path().join("README.md"), "# Home\n").expect("write readme");
    std::fs::write(nested.join("README.md"), "# Guides Home\n").expect("write nested readme");
    std::fs::write(nested.join("setup.md"), "# Setup\n").expect("write nested doc");
    let mut server = ServeProcess::start_recursive_dir(dir.path());

    let home = http_get(server.port, "/");
    let nested_readme = http_get(server.port, "/nested/README.md");
    let nested_setup = http_get(server.port, "/nested/setup.md");

    assert!(home.contains(r#"<h1 id="home">Home</h1>"#));
    assert!(home.contains(r#"href="/nested/README.md""#));
    assert!(home.contains(r#"href="/nested/setup.md""#));
    assert!(nested_readme.contains(r#"<h1 id="guides-home">Guides Home</h1>"#));
    assert!(nested_setup.contains(r#"<h1 id="setup">Setup</h1>"#));
    server.stop();
}

#[test]
fn serve_mode_recurse_allows_same_markdown_filename_in_different_directories() {
    let dir = tempfile::tempdir().expect("temp dir");
    let first = dir.path().join("first");
    let second = dir.path().join("second");
    std::fs::create_dir(&first).expect("first dir");
    std::fs::create_dir(&second).expect("second dir");
    std::fs::write(dir.path().join("README.md"), "# Home\n").expect("write readme");
    std::fs::write(first.join("CLAUDE.md"), "# First Claude\n").expect("write first claude");
    std::fs::write(second.join("CLAUDE.md"), "# Second Claude\n").expect("write second claude");
    let mut server = ServeProcess::start_recursive_dir(dir.path());

    let home = http_get(server.port, "/");
    let first_response = http_get(server.port, "/first/CLAUDE.md");
    let second_response = http_get(server.port, "/second/CLAUDE.md");

    assert!(home.contains(r#"href="/first/CLAUDE.md""#));
    assert!(home.contains(r#"href="/second/CLAUDE.md""#));
    assert!(first_response.contains(r#"<h1 id="first-claude">First Claude</h1>"#));
    assert!(second_response.contains(r#"<h1 id="second-claude">Second Claude</h1>"#));
    server.stop();
}

#[cfg(unix)]
#[test]
fn serve_mode_recurse_drops_symlink_alias_to_markdown_file() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().expect("temp dir");
    let docs = dir.path().join("BaldwinDocs");
    std::fs::create_dir(&docs).expect("docs dir");
    std::fs::write(dir.path().join("README.md"), "# Home\n").expect("write readme");
    std::fs::write(docs.join("AGENTS.md"), "# Agents\n").expect("write agents");
    symlink("AGENTS.md", docs.join("CLAUDE.md")).expect("symlink claude");
    let mut server = ServeProcess::start_recursive_dir(dir.path());

    let home = http_get(server.port, "/");
    let agents = http_get(server.port, "/BaldwinDocs/AGENTS.md");
    let claude = http_get(server.port, "/BaldwinDocs/CLAUDE.md");

    assert!(home.contains(r#"href="/BaldwinDocs/AGENTS.md""#));
    assert!(!home.contains(r#"href="/BaldwinDocs/CLAUDE.md""#));
    assert!(agents.contains(r#"<h1 id="agents">Agents</h1>"#));
    assert!(claude.contains("HTTP/1.1 404 Not Found"));
    server.stop();
}

#[test]
fn serve_mode_recurse_groups_sidebar_nav_by_directory_and_auto_expands_active_path() {
    let dir = tempfile::tempdir().expect("temp dir");
    let guides = dir.path().join("guides");
    let my_docs = dir.path().join("My Docs");
    let cafe = dir.path().join("café");
    let other = dir.path().join("other");
    std::fs::create_dir(&guides).expect("guides dir");
    std::fs::create_dir(&my_docs).expect("my docs dir");
    std::fs::create_dir(&cafe).expect("cafe dir");
    std::fs::create_dir(&other).expect("other dir");
    std::fs::write(dir.path().join("README.md"), "# Home\n").expect("write readme");
    std::fs::write(guides.join("setup.md"), "# Setup\n").expect("write guides doc");
    std::fs::write(my_docs.join("intro.md"), "# Intro\n").expect("write my docs doc");
    std::fs::write(cafe.join("menu.md"), "# Menu\n").expect("write cafe doc");
    std::fs::write(other.join("misc.md"), "# Misc\n").expect("write other doc");
    let mut server = ServeProcess::start_recursive_dir(dir.path());

    let home = http_get(server.port, "/");
    let setup = http_get(server.port, "/guides/setup.md");

    // Nested documents are grouped under a collapsible <details> per directory.
    assert!(home.contains(r#"<details class="markview-nav-dir" data-markview-dir="/guides">"#));
    assert!(home.contains(r#"<details class="markview-nav-dir" data-markview-dir="/My%20Docs">"#));
    assert!(home.contains(r#"<details class="markview-nav-dir" data-markview-dir="/caf%C3%A9">"#));
    assert!(home.contains(r#"<details class="markview-nav-dir" data-markview-dir="/other">"#));
    assert!(home.contains("<summary>guides</summary>"));
    assert!(home.contains("<summary>My Docs</summary>"));
    assert!(home.contains("<summary>caf&#233;</summary>"));
    assert!(home.contains("<summary>other</summary>"));

    // On the homepage neither directory is on the active path, so both start collapsed.
    assert!(!home.contains(r#"data-markview-dir="/guides" open"#));
    assert!(!home.contains(r#"data-markview-dir="/other" open"#));

    // Viewing a document inside guides/ auto-expands just that directory.
    assert!(setup.contains(r#"data-markview-dir="/guides" open"#));
    assert!(!setup.contains(r#"data-markview-dir="/other" open"#));
    server.stop();
}

#[test]
fn serve_mode_recurse_expands_sidebar_nav_multiple_levels_deep() {
    let dir = tempfile::tempdir().expect("temp dir");
    let guides = dir.path().join("guides");
    let deep = guides.join("deep");
    let other = dir.path().join("other");
    std::fs::create_dir_all(&deep).expect("deep guides dir");
    std::fs::create_dir(&other).expect("other dir");
    std::fs::write(dir.path().join("README.md"), "# Home\n").expect("write readme");
    std::fs::write(deep.join("setup.md"), "# Setup\n").expect("write deep setup");
    std::fs::write(other.join("misc.md"), "# Misc\n").expect("write other doc");
    let mut server = ServeProcess::start_recursive_dir(dir.path());

    let home = http_get(server.port, "/");
    let setup = http_get(server.port, "/guides/deep/setup.md");

    assert!(home.contains(r#"data-markview-dir="/guides""#));
    assert!(home.contains(r#"data-markview-dir="/guides/deep""#));
    assert!(!home.contains(r#"data-markview-dir="/guides" open"#));
    assert!(!home.contains(r#"data-markview-dir="/guides/deep" open"#));

    assert!(setup.contains(r#"data-markview-dir="/guides" open"#));
    assert!(setup.contains(r#"data-markview-dir="/guides/deep" open"#));
    assert!(!setup.contains(r#"data-markview-dir="/other" open"#));
    server.stop();
}

#[test]
fn serve_mode_recurse_rescans_when_a_new_nested_file_is_created() {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(dir.path().join("README.md"), "# Home\n").expect("write readme");
    let mut server = ServeProcess::start_recursive_dir(dir.path());

    // The new file's directory doesn't even exist at startup.
    assert!(http_get(server.port, "/guides/setup.md").contains("HTTP/1.1 404 Not Found"));

    let mut stream = TcpStream::connect(("127.0.0.1", server.port)).expect("connect events");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set timeout");
    stream
        .write_all(b"GET /events HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .expect("write request");
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    loop {
        line.clear();
        reader.read_line(&mut line).expect("read header");
        if line == "\r\n" {
            break;
        }
    }

    let guides = dir.path().join("guides");
    std::fs::create_dir(&guides).expect("create guides dir");
    std::fs::write(guides.join("setup.md"), "# Setup\n").expect("write nested doc");
    // A structural change (new document set) gets a distinct "rescan" event
    // rather than "reload", since the client needs a full page reload to pick
    // up any nav-layout shell change (e.g. single-file "no nav" -> sidebar)
    // that a plain innerHTML patch can't apply.
    let event = read_until(&mut reader, "data: rescan", Duration::from_secs(5));
    assert!(event.contains("data: rescan"));

    let setup = http_get(server.port, "/guides/setup.md");
    assert!(setup.contains(r#"<h1 id="setup">Setup</h1>"#));
    assert!(http_get(server.port, "/").contains(r#"href="/guides/setup.md""#));
    server.stop();
}

#[test]
fn serve_mode_recurse_rescans_when_a_served_file_is_renamed_away_from_markdown() {
    let dir = tempfile::tempdir().expect("temp dir");
    let old = dir.path().join("old.md");
    let renamed = dir.path().join("old.txt");
    std::fs::write(dir.path().join("README.md"), "# Home\n").expect("write readme");
    std::fs::write(&old, "# Old\n").expect("write old doc");
    let mut server = ServeProcess::start_recursive_dir(dir.path());

    let before = http_get(server.port, "/");
    assert!(before.contains(r#"href="/old.md""#));
    assert!(http_get(server.port, "/old.md").contains(r#"<h1 id="old">Old</h1>"#));

    let mut reader = open_reload_stream(server.port);
    std::fs::rename(&old, &renamed).expect("rename old doc");
    let event = read_until(&mut reader, "data: rescan", Duration::from_secs(5));
    assert!(
        event.contains("data: rescan"),
        "unexpected event stream: {event}"
    );

    let after = http_get(server.port, "/");
    assert!(
        !after.contains(r#"href="/old.md""#),
        "stale nav link remained:\n{after}"
    );
    assert!(http_get(server.port, "/old.md").contains("HTTP/1.1 404 Not Found"));
    server.stop();
}

#[test]
fn serve_mode_recurse_rescans_when_a_served_file_is_deleted() {
    let dir = tempfile::tempdir().expect("temp dir");
    let stale = dir.path().join("stale.md");
    std::fs::write(dir.path().join("README.md"), "# Home\n").expect("write readme");
    std::fs::write(dir.path().join("other.md"), "# Other\n").expect("write other doc");
    std::fs::write(&stale, "# Stale\n").expect("write stale doc");
    let mut server = ServeProcess::start_recursive_dir(dir.path());

    assert!(http_get(server.port, "/").contains(r#"href="/stale.md""#));
    assert!(http_get(server.port, "/stale.md").contains(r#"<h1 id="stale">Stale</h1>"#));

    let mut reader = open_reload_stream(server.port);
    std::fs::remove_file(&stale).expect("delete stale doc");
    // The page was served, so its store is watched too. That watcher may
    // report the vanished document as an `annotations` event before the
    // rescan lands; read on to the reload or rescan itself.
    let event = read_until(&mut reader, "data: re", Duration::from_secs(5));

    assert!(
        event.contains("data: reload"),
        "unexpected event stream: {event}"
    );
    assert!(
        !event.contains("data: rescan"),
        "unchanged nav layout forced full reload: {event}"
    );
    let after = http_get(server.port, "/");
    assert!(
        !after.contains(r#"href="/stale.md""#),
        "stale nav link remained:\n{after}"
    );
    assert!(http_get(server.port, "/stale.md").contains("HTTP/1.1 404 Not Found"));
    server.stop();
}

#[test]
fn serve_mode_directory_rescans_when_a_new_top_level_file_is_created() {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(dir.path().join("README.md"), "# Home\n").expect("write readme");
    let mut server = ServeProcess::start_dir(dir.path());

    assert!(http_get(server.port, "/notes.md").contains("HTTP/1.1 404 Not Found"));

    let mut stream = TcpStream::connect(("127.0.0.1", server.port)).expect("connect events");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set timeout");
    stream
        .write_all(b"GET /events HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .expect("write request");
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    loop {
        line.clear();
        reader.read_line(&mut line).expect("read header");
        if line == "\r\n" {
            break;
        }
    }

    std::fs::write(dir.path().join("notes.md"), "# Notes\n").expect("write new top-level doc");
    let event = read_until(&mut reader, "data: rescan", Duration::from_secs(5));
    assert!(event.contains("data: rescan"));

    assert!(http_get(server.port, "/notes.md").contains(r#"<h1 id="notes">Notes</h1>"#));
    server.stop();
}

#[test]
fn serve_mode_directory_uses_content_reload_when_added_file_keeps_sidebar_layout() {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(dir.path().join("README.md"), "# Home\n").expect("write readme");
    std::fs::write(dir.path().join("notes.md"), "# Notes\n").expect("write notes");
    let mut server = ServeProcess::start_dir(dir.path());

    let before = http_get(server.port, "/");
    assert!(before.contains(r#"class="markview-sidebar""#));
    assert!(before.contains(r#"href="/notes.md""#));
    assert!(http_get(server.port, "/extra.md").contains("HTTP/1.1 404 Not Found"));

    let mut reader = open_reload_stream(server.port);
    std::fs::write(dir.path().join("extra.md"), "# Extra\n").expect("write extra");
    let event = read_until(&mut reader, "data:", Duration::from_secs(5));

    assert!(
        event.contains("data: reload"),
        "unexpected event stream: {event}"
    );
    assert!(
        !event.contains("data: rescan"),
        "unchanged nav layout forced full reload: {event}"
    );
    let after = http_get(server.port, "/");
    assert!(after.contains(r#"href="/extra.md""#));
    server.stop();
}

#[cfg(unix)]
#[test]
fn serve_mode_recurse_ignores_invalid_rescan_and_keeps_serving() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().expect("temp dir");
    let outside = tempfile::NamedTempFile::new().expect("outside file");
    std::fs::write(dir.path().join("README.md"), "# Home\n").expect("write readme");
    let mut server = ServeProcess::start_recursive_dir(dir.path());

    assert!(http_get(server.port, "/").contains(r#"<h1 id="home">Home</h1>"#));

    // A newly-created Markdown-named symlink whose target escapes the served
    // root makes a rescan fail; the server should log it and keep serving the
    // last-known-good document set rather than falling over.
    symlink(outside.path(), dir.path().join("escape.md")).expect("symlink escape");
    std::thread::sleep(Duration::from_millis(300));

    assert!(http_get(server.port, "/").contains(r#"<h1 id="home">Home</h1>"#));
    assert!(server.stderr_so_far().contains("failed to rescan"));
    server.stop();
}

#[test]
fn serve_mode_recurse_skips_hidden_directories() {
    let dir = tempfile::tempdir().expect("temp dir");
    let hidden = dir.path().join(".git");
    std::fs::create_dir(&hidden).expect("hidden dir");
    std::fs::write(dir.path().join("README.md"), "# Home\n").expect("write readme");
    std::fs::write(hidden.join("notes.md"), "# Should Not Serve\n").expect("write hidden doc");
    let mut server = ServeProcess::start_recursive_dir(dir.path());

    assert!(http_get(server.port, "/").contains(r#"<h1 id="home">Home</h1>"#));
    assert!(http_get(server.port, "/.git/notes.md").contains("HTTP/1.1 404 Not Found"));
    server.stop();
}

#[test]
fn serve_mode_rejects_recurse_without_serve() {
    let mut cmd = Command::cargo_bin("markview").expect("binary");
    cmd.args(["--recurse", "README.md"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown argument: --recurse"));
}

#[test]
fn serve_mode_rejects_open_without_serve() {
    let mut cmd = Command::cargo_bin("markview").expect("binary");
    cmd.args(["--open", "README.md"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown argument: --open"));
}

#[test]
fn serve_mode_accepts_open_and_keeps_serving() {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("doc.md");
    std::fs::write(&file, "# Doc\n").expect("write doc");
    let mut server = ServeProcess::start_with_open(&file);

    // The server must stay up and answer requests regardless of whether the
    // best-effort browser launch actually succeeded (it typically can't in a
    // headless test environment).
    assert!(http_get(server.port, "/").contains(r#"<h1 id="doc">Doc</h1>"#));
    server.stop();
}

#[test]
fn serve_mode_rejects_daemon_without_serve() {
    let mut cmd = Command::cargo_bin("markview").expect("binary");
    cmd.args(["--daemon", "README.md"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown argument: --daemon"));
}

#[test]
fn foreground_port_zero_forms_report_concrete_banner_ports() {
    for args in [vec!["--port", "0"], vec!["--port=0"]] {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("README.md");
        std::fs::write(&file, "# Foreground zero").unwrap();
        let child = std::process::Command::new(cargo_bin("markview"))
            .arg("--serve")
            .args(args)
            .arg(&file)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut server = ServeProcess {
            child,
            port: 0,
            stderr: Arc::new(Mutex::new(String::new())),
        };
        let mut line = String::new();
        BufReader::new(server.child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        server.port = number_after(&line, "http://localhost:");
        assert_ne!(server.port, 0, "{line}");
        assert!(
            poll_http_until_ready(server.port, Duration::from_secs(5)).contains("Foreground zero")
        );
    }
}

#[test]
fn serve_rejects_noncanonical_zero_port() {
    Command::cargo_bin("markview")
        .unwrap()
        .args(["--serve", "--port", "00", "README.md"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid port: 00"));
}

#[test]
fn serve_mode_daemon_detaches_and_prints_pid_and_address() {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("README.md");
    std::fs::write(&file, "# Daemonized\n").expect("write sample");

    let start = Instant::now();
    let output = std::process::Command::new(cargo_bin("markview"))
        .current_dir(dir.path())
        .args(["--serve", "--daemon", "--port", "0"])
        .arg(&file)
        .output()
        .expect("run daemon");
    let startup_elapsed = start.elapsed();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let _daemon = DaemonProcess(number_after(&stdout, "pid "));
    let port: u16 = number_after(&stdout, "http://localhost:");
    assert_ne!(port, 0);
    assert!(
        startup_elapsed < Duration::from_secs(5),
        "the foreground parent should exit quickly instead of blocking, took {startup_elapsed:?}"
    );

    assert!(stdout.contains(&format!("http://localhost:{port}")));
    assert!(stdout.contains("pid "));

    let body = poll_http_until_ready(port, Duration::from_secs(5));
    assert!(body.contains(r#"<h1 id="daemonized">Daemonized</h1>"#));
}

#[test]
fn serve_mode_daemon_with_port_zero_reports_the_port_it_picked() {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("README.md");
    std::fs::write(&file, "# Any port\n").expect("write sample");

    let output = std::process::Command::new(cargo_bin("markview"))
        .current_dir(dir.path())
        .args(["--serve", "--daemon", "--port", "0"])
        .arg(&file)
        .output()
        .expect("run daemon");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let _daemon = DaemonProcess(number_after(&stdout, "pid "));
    let port = number_after(&stdout, "http://localhost:");
    assert_ne!(port, 0, "stdout should name the real port: {stdout}");
    assert!(
        stdout.contains(&format!(".markview-serve-{port}.log")),
        "the log file should be named for the real port: {stdout}"
    );

    let body = poll_http_until_ready(port, Duration::from_secs(5));
    assert!(body.contains(r#"<h1 id="any-port">Any port</h1>"#));
}

#[test]
fn serve_mode_daemon_reports_failure_when_child_fails_to_start() {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("not-markdown.txt");
    std::fs::write(&file, "plain text, not Markdown\n").expect("write sample");
    let port = unused_port();

    let start = Instant::now();
    let output = std::process::Command::new(cargo_bin("markview"))
        .current_dir(dir.path())
        .args(["--serve", "--daemon", "--port"])
        .arg(port.to_string())
        .arg(&file)
        .output()
        .expect("run daemon");
    let elapsed = start.elapsed();

    assert!(
        !output.status.success(),
        "expected failure, but the parent reported success: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "the child fails immediately, so the parent shouldn't wait out the full readiness \
         timeout before reporting failure, took {elapsed:?}"
    );

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        !stdout.contains("serving in background"),
        "parent printed the success message despite the child failing: {stdout}"
    );

    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(stderr.contains("exited before it started serving"));
    assert!(
        stderr.contains(".markview-serve"),
        "stderr should point at the log file: {stderr}"
    );

    // Nothing should actually be listening on the port.
    assert!(TcpStream::connect(("127.0.0.1", port)).is_err());
}

fn number_after<T: std::str::FromStr>(stdout: &str, marker: &str) -> T {
    let after = stdout.split_once(marker).expect("output marker").1;
    let digits: String = after.chars().take_while(|ch| ch.is_ascii_digit()).collect();
    digits.parse().ok().expect("numeric output")
}

struct DaemonProcess(u32);

impl Drop for DaemonProcess {
    fn drop(&mut self) {
        kill_pid(self.0);
    }
}

fn poll_http_until_ready(port: u16, timeout: Duration) -> String {
    let start = Instant::now();
    loop {
        if let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)) {
            let request = write!(
                stream,
                "GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
            );
            let mut response = Vec::new();
            if request.is_ok() && stream.read_to_end(&mut response).is_ok() {
                return String::from_utf8_lossy(&response).to_string();
            }
        }
        if start.elapsed() > timeout {
            panic!("daemonized server on port {port} did not become ready in time");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn kill_pid(pid: u32) {
    let _ = std::process::Command::new("kill")
        .arg("-9")
        .arg(pid.to_string())
        .status();
}

#[test]
fn serve_mode_rejects_recurse_with_non_directory_input() {
    let dir = tempfile::tempdir().expect("temp dir");
    let one = dir.path().join("one.md");
    let two = dir.path().join("two.md");
    std::fs::write(&one, "# One\n").expect("write one");
    std::fs::write(&two, "# Two\n").expect("write two");

    let mut cmd = Command::cargo_bin("markview").expect("binary");
    cmd.args(["--serve", "--recurse"])
        .arg(&one)
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "--recurse requires a single served directory",
        ));

    let mut cmd = Command::cargo_bin("markview").expect("binary");
    cmd.args(["--serve", "--recurse"])
        .arg(&one)
        .arg(&two)
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "--recurse requires a single served directory",
        ));
}

#[cfg(unix)]
#[test]
fn serve_mode_recurse_does_not_follow_symlinked_directories() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().expect("temp dir");
    let nested = dir.path().join("nested");
    std::fs::create_dir(&nested).expect("nested dir");
    std::fs::write(dir.path().join("README.md"), "# Home\n").expect("write readme");
    std::fs::write(nested.join("inner.md"), "# Inner\n").expect("write nested doc");
    symlink(dir.path(), nested.join("loop")).expect("symlink cycle back to root");

    let mut server = ServeProcess::start_recursive_dir(dir.path());

    assert!(http_get(server.port, "/").contains(r#"<h1 id="home">Home</h1>"#));
    assert!(http_get(server.port, "/nested/inner.md").contains(r#"<h1 id="inner">Inner</h1>"#));
    server.stop();
}

#[test]
fn directory_serve_loads_referenced_assets_but_not_unreferenced_files() {
    let dir = tempfile::tempdir().expect("temp dir");
    let assets = dir.path().join("assets");
    std::fs::create_dir(&assets).expect("assets dir");
    std::fs::write(
        dir.path().join("README.md"),
        "# Doc\n\n![Chart](assets/chart.png)\n[Raw](assets/raw.html)\n",
    )
    .expect("write readme");
    std::fs::write(assets.join("chart.png"), b"\x89PNG\r\n\x1a\n").expect("write image");
    std::fs::write(assets.join("raw.html"), "<script>alert(1)</script>").expect("write html");
    std::fs::write(assets.join("secret.txt"), "secret").expect("write secret");
    let mut server = ServeProcess::start_dir(dir.path());
    let document = http_get(server.port, "/");

    let image = http_get(server.port, "/assets/chart.png");
    let html = http_get(server.port, "/assets/raw.html");
    let secret = http_get(server.port, "/assets/secret.txt");

    assert!(document.contains(r#"src="/assets/chart.png""#));
    assert!(document.contains(r#"href="/assets/raw.html""#));
    assert!(image.contains("HTTP/1.1 200 OK"));
    assert!(image.contains("Content-Type: image/png"));
    assert!(image.contains("X-Content-Type-Options: nosniff"));
    assert!(html.contains("HTTP/1.1 200 OK"));
    assert!(html.contains("Content-Type: application/octet-stream"));
    assert!(secret.contains("HTTP/1.1 404 Not Found"));
    server.stop();
}

#[test]
fn serve_mode_serves_assets_referenced_through_links_whose_label_starts_with_a_bracket() {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(dir.path().join("README.md"),
        "# Doc\n\n[[1]](cite.txt)\n[[RFC 2119]](rfc.txt)\n![[x]](pic2.png)\n[see [[x]]](d.txt)\n[[RFC2119]]\n\n[RFC2119]: ref.txt\n").unwrap();
    for asset in ["cite.txt", "rfc.txt", "pic2.png", "d.txt", "ref.txt"] {
        std::fs::write(dir.path().join(asset), "asset").unwrap();
    }
    let mut server = ServeProcess::start_dir(dir.path());
    let document = http_get(server.port, "/");
    for (attribute, asset) in [
        ("href", "cite.txt"),
        ("href", "rfc.txt"),
        ("src", "pic2.png"),
        ("href", "d.txt"),
        ("href", "ref.txt"),
    ] {
        assert!(
            http_get(server.port, &format!("/{asset}")).contains("HTTP/1.1 200 OK"),
            "{asset}"
        );
        assert!(
            document.contains(&format!("{attribute}=\"/{asset}\"")),
            "{asset}: {document}"
        );
    }
    server.stop();
}

#[test]
fn serve_mode_does_not_serve_files_named_only_by_wikilink_style_prose() {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::create_dir(dir.path().join(".hidden")).unwrap();
    let prose = "[[x.txt]] ![[y.png]] [[.hidden/secret.txt]]";
    std::fs::write(
        dir.path().join("README.md"),
        format!("# Doc\n\n{prose}\n\n| Text |\n| --- |\n| {prose} |\n\n- {prose}\n\n> {prose}\n"),
    )
    .unwrap();
    for asset in ["x.txt", "y.png", ".hidden/secret.txt"] {
        std::fs::write(dir.path().join(asset), "private").unwrap();
    }
    let mut server = ServeProcess::start_dir(dir.path());
    for asset in ["x.txt", "y.png", ".hidden/secret.txt"] {
        assert!(
            http_get(server.port, &format!("/{asset}")).contains("HTTP/1.1 404 Not Found"),
            "{asset}"
        );
    }
    server.stop();
}

#[test]
fn serve_mode_preserves_legacy_heading_titles_and_navigation_labels() {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(dir.path().join("README.md"), "# Home").unwrap();
    let cases = [
        ("Title ^sup^ and E=mc^2^", "Title ^sup^ and E=mc^2^"),
        ("Bitwise XOR (^) operator", "Bitwise XOR (^) operator"),
        ("Scaling 10^-3", "Scaling 10^-3"),
        ("Press Ctrl+^", "Press Ctrl+^"),
        // Keep the pre-existing space-joining quirk between parser text events.
        ("See [[Other Page]] now", "See [ [ Other Page ] ] now"),
    ];
    for (index, (heading, _)) in cases.iter().enumerate() {
        std::fs::write(
            dir.path().join(format!("doc{index}.md")),
            format!("# {heading}\n"),
        )
        .unwrap();
    }
    let mut server = ServeProcess::start_dir(dir.path());
    let home = http_get(server.port, "/");
    for (index, (_, title)) in cases.iter().enumerate() {
        let route = format!("/doc{index}.md");
        let page = http_get(server.port, &route);
        assert!(
            page.contains(&format!("<title>{title}</title>")),
            "{title}: {page}"
        );
        assert!(
            home.contains(&format!(
                "<a href=\"{route}\" title=\"{route}\">{title}</a>"
            )),
            "{title}: {home}"
        );
    }
    server.stop();
}

#[test]
fn serve_mode_discovers_assets_added_after_startup() {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(dir.path().join("README.md"), "# Doc\n").expect("write readme");
    let mut server = ServeProcess::start_dir(dir.path());

    assert!(http_get(server.port, "/chart.png").contains("HTTP/1.1 404 Not Found"));

    std::fs::write(dir.path().join("chart.png"), b"\x89PNG\r\n\x1a\n").expect("write chart");
    std::fs::write(
        dir.path().join("README.md"),
        "# Doc\n\n![Chart](chart.png)\n",
    )
    .expect("update readme");
    let document = http_get(server.port, "/");
    let asset = http_get(server.port, "/chart.png");

    assert!(document.contains(r#"src="/chart.png""#));
    assert!(asset.contains("HTTP/1.1 200 OK"));
    assert!(asset.contains("Content-Type: image/png"));
    server.stop();
}

#[test]
fn serve_mode_discovers_an_asset_created_after_its_reference_without_touching_the_document() {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(
        dir.path().join("README.md"),
        "# Doc\n\n![Chart](chart.png)\n",
    )
    .expect("write readme");
    let mut server = ServeProcess::start_dir(dir.path());

    // The asset doesn't exist yet, so it 404s and gets cached as missing.
    assert!(http_get(server.port, "/chart.png").contains("HTTP/1.1 404 Not Found"));

    let mut stream = TcpStream::connect(("127.0.0.1", server.port)).expect("connect events");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set timeout");
    stream
        .write_all(b"GET /events HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .expect("write request");
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    loop {
        line.clear();
        reader.read_line(&mut line).expect("read header");
        if line == "\r\n" {
            break;
        }
    }

    // Create the asset without touching the Markdown document at all.
    std::fs::write(dir.path().join("chart.png"), b"\x89PNG\r\n\x1a\n").expect("write chart");
    let event = read_until(&mut reader, "data: reload", Duration::from_secs(5));
    assert!(event.contains("data: reload"));

    let asset = http_get(server.port, "/chart.png");
    assert!(asset.contains("HTTP/1.1 200 OK"));
    assert!(asset.contains("Content-Type: image/png"));
    server.stop();
}

#[test]
fn serve_mode_supports_percent_encoded_asset_references() {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(
        dir.path().join("README.md"),
        "# Doc\n\n![Space](my%20file.png)\n![Percent](a%252eb.png)\n",
    )
    .expect("write readme");
    std::fs::write(dir.path().join("my file.png"), b"\x89PNG\r\n\x1a\n").expect("write spaced");
    std::fs::write(dir.path().join("a%2eb.png"), b"\x89PNG\r\n\x1a\n").expect("write percent");
    let mut server = ServeProcess::start_dir(dir.path());

    let document = http_get(server.port, "/");
    let spaced = http_get(server.port, "/my%20file.png");
    let percent = http_get(server.port, "/a%252eb.png");

    assert!(document.contains(r#"src="/my%20file.png""#));
    assert!(document.contains(r#"src="/a%252eb.png""#));
    assert!(spaced.contains("HTTP/1.1 200 OK"));
    assert!(percent.contains("HTTP/1.1 200 OK"));
    server.stop();
}

#[test]
fn serve_mode_blocks_traversal_and_non_get_methods() {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("README.md");
    std::fs::write(&file, "# Served\n").expect("write sample");
    let mut server = ServeProcess::start(&file);

    assert!(http_get(server.port, "/../README.md").contains("HTTP/1.1 404 Not Found"));
    assert!(http_get(server.port, "/%2e%2e/README.md").contains("HTTP/1.1 404 Not Found"));
    assert!(http_get(server.port, "/%252e%252e/README.md").contains("HTTP/1.1 404 Not Found"));
    assert!(http_request(server.port, "POST", "/").contains("HTTP/1.1 405 Method Not Allowed"));
    assert!(http_request(server.port, "HEAD", "/").contains("HTTP/1.1 200 OK"));
    let events_head = http_request(server.port, "HEAD", "/events");
    assert!(events_head.contains("HTTP/1.1 200 OK"));
    assert!(!events_head.contains("data: reload"));
    server.stop();
}

#[cfg(unix)]
#[test]
fn serve_mode_blocks_symlink_escape() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().expect("temp dir");
    let outside = tempfile::NamedTempFile::new().expect("outside file");
    std::fs::write(
        dir.path().join("README.md"),
        "# Served\n\n[Outside](escape.txt)\n",
    )
    .expect("write readme");
    symlink(outside.path(), dir.path().join("escape.txt")).expect("symlink escape");
    let mut server = ServeProcess::start_dir(dir.path());

    assert!(http_get(server.port, "/escape.txt").contains("HTTP/1.1 404 Not Found"));
    server.stop();
}

#[cfg(unix)]
#[test]
fn serve_mode_revalidates_documents_on_each_request() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("README.md");
    let outside = tempfile::NamedTempFile::new().expect("outside file");
    std::fs::write(&file, "# Served\n").expect("write readme");
    std::fs::write(outside.path(), "# Escaped\n").expect("write outside");
    let mut server = ServeProcess::start(&file);

    std::fs::remove_file(&file).expect("remove served file");
    symlink(outside.path(), &file).expect("replace with symlink");
    let response = http_get(server.port, "/");

    assert!(response.contains("HTTP/1.1 404 Not Found"));
    assert!(!response.contains("Escaped"));
    server.stop();
}

#[cfg(unix)]
#[test]
fn serve_mode_does_not_allowlist_unserved_markdown_as_asset() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(
        dir.path().join("README.md"),
        "# Served\n\n[Secret](secret-link)\n",
    )
    .expect("write readme");
    std::fs::write(dir.path().join("private-notes.md"), "# Private\n").expect("write private");
    symlink(
        dir.path().join("private-notes.md"),
        dir.path().join("secret-link"),
    )
    .expect("symlink markdown");
    let mut server = ServeProcess::start(&dir.path().join("README.md"));

    let response = http_get(server.port, "/secret-link");

    assert!(response.contains("HTTP/1.1 404 Not Found"));
    assert!(!response.contains("Private"));
    server.stop();
}

#[test]
fn directory_mode_preserves_outside_and_nested_markdown_links() {
    let dir = tempfile::tempdir().expect("temp dir");
    let nested = dir.path().join("nested");
    std::fs::create_dir(&nested).expect("nested dir");
    std::fs::write(
        dir.path().join("README.md"),
        "# Doc\n\n[Other](other.md#frag)\n[Nested](nested/inner.md)\n[Outside](../outside.md)\n",
    )
    .expect("write readme");
    std::fs::write(dir.path().join("other.md"), "# Other\n").expect("write other");
    std::fs::write(nested.join("inner.md"), "# Inner\n").expect("write nested");
    let mut server = ServeProcess::start_dir(dir.path());
    let response = http_get(server.port, "/");

    assert!(response.contains(r#"href="/other.md#frag""#));
    assert!(response.contains(r#"href="nested/inner.md""#));
    assert!(response.contains(r#"href="../outside.md""#));
    server.stop();
}

#[test]
fn serve_mode_rewrites_links_without_touching_code_samples_and_reference_definitions() {
    let dir = tempfile::tempdir().expect("temp dir");
    let first = dir.path().join("README.md");
    let second = dir.path().join("other.md");
    std::fs::write(
        &first,
        "# Doc\n\n```md\n![Chart](chart.png)\n```\n\n![Chart](chart.png)\n[Other][other]\n\n[other]: other.md#frag\n",
    )
    .expect("write readme");
    std::fs::write(&second, "# Other\n").expect("write other");
    std::fs::write(dir.path().join("chart.png"), b"\x89PNG\r\n\x1a\n").expect("write chart");
    let mut server = ServeProcess::start_with_files(&[first.as_path(), second.as_path()]);

    let response = http_get(server.port, "/");

    assert!(response.contains("![Chart](chart.png)"));
    assert!(response.contains(r#"src="/chart.png""#));
    assert!(response.contains(r#"href="/other.md#frag""#));
    server.stop();
}

#[test]
fn serve_mode_does_not_close_a_fence_on_a_shorter_nested_fence_marker() {
    let dir = tempfile::tempdir().expect("temp dir");
    let readme = dir.path().join("README.md");
    std::fs::write(
        &readme,
        "# Doc\n\n````\nHow to show a fenced example:\n```\n![Chart](chart.png)\n```\n````\n\n![Chart](chart.png)\n",
    )
    .expect("write readme");
    std::fs::write(dir.path().join("chart.png"), b"\x89PNG\r\n\x1a\n").expect("write chart");
    let mut server = ServeProcess::start(&readme);

    let response = http_get(server.port, "/");

    // The inner ``` is shorter than the outer ```` fence that opened the block, so
    // per CommonMark it doesn't close it — the example inside must stay literal.
    assert!(
        response.contains("![Chart](chart.png)"),
        "nested fence example was not preserved literally:\n{response}"
    );
    // Only the real, unfenced image reference below the code block should have
    // been rewritten to a served route.
    assert_eq!(
        response.matches(r#"src="/chart.png""#).count(),
        1,
        "expected exactly one rewritten image reference:\n{response}"
    );
    server.stop();
}

#[test]
fn serve_mode_preserves_multibyte_utf8_prose_when_rewriting_links() {
    let dir = tempfile::tempdir().expect("temp dir");
    let first = dir.path().join("README.md");
    let second = dir.path().join("other.md");
    std::fs::write(
        &first,
        "# Café Notes\n\nSee [café notes](other.md) for details.\n\n```\nEmoji: 😀\n```\n",
    )
    .expect("write readme");
    std::fs::write(&second, "# Other\n").expect("write other");
    let mut server = ServeProcess::start_with_files(&[first.as_path(), second.as_path()]);

    let response = http_get(server.port, "/");

    // Rendered HTML entity-escapes all non-ASCII output (see `escape_non_ascii_html`),
    // so a correctly-preserved "é" (U+00E9) appears as `&#233;`, not a literal char.
    assert!(
        response.contains("caf&#233; notes"),
        "multi-byte UTF-8 prose was corrupted:\n{response}"
    );
    assert!(
        !response.contains("&#195;"),
        "response contains a byte-as-char mojibake artifact (0xC3 lead byte of 'é'):\n{response}"
    );
    server.stop();
}

#[test]
fn serve_mode_rewrites_links_with_parenthesized_titles() {
    let dir = tempfile::tempdir().expect("temp dir");
    let first = dir.path().join("README.md");
    let second = dir.path().join("notes.md");
    std::fs::write(
        &first,
        "# Doc\n\nSee [Details](notes.md \"See (details)\") for background.\n",
    )
    .expect("write readme");
    std::fs::write(&second, "# Notes\n").expect("write notes");
    let mut server = ServeProcess::start_with_files(&[first.as_path(), second.as_path()]);

    let response = http_get(server.port, "/");

    // Check the inline link specifically (by pairing it with its title attribute),
    // not just for "/notes.md" anywhere in the page — the nav bar always links to
    // every served document correctly regardless of this bug.
    assert!(
        response.contains(r#"href="/notes.md" title="See (details)""#),
        "titled link destination was not rewritten to the served route:\n{response}"
    );
    server.stop();
}

#[test]
fn serve_mode_does_not_rewrite_footnote_definitions_as_link_references() {
    let dir = tempfile::tempdir().expect("temp dir");
    let first = dir.path().join("README.md");
    let second = dir.path().join("notes.md");
    std::fs::write(
        &first,
        "# Doc\n\nSee the footnote[^1] for background.\n\n[^1]: notes.md has more information.\n",
    )
    .expect("write readme");
    std::fs::write(&second, "# Notes\n").expect("write notes");
    let mut server = ServeProcess::start_with_files(&[first.as_path(), second.as_path()]);

    let response = http_get(server.port, "/");

    assert!(
        response.contains("notes.md has more information."),
        "footnote body text is missing entirely:\n{response}"
    );
    assert!(
        !response.contains("/notes.md has more information."),
        "footnote body was rewritten as if it were a link reference definition:\n{response}"
    );
    server.stop();
}

#[test]
fn serve_annotations_enforces_http_boundaries_and_keeps_stores_in_sync() {
    use markview::annotations::{source_revision, FORM_CAP};
    let dir = tempfile::tempdir().unwrap();
    let docs = dir.path().join("docs");
    std::fs::create_dir(&docs).unwrap();
    let document = docs.join("review.md");
    let source = "😀 **text**";
    std::fs::write(&document, source).unwrap();
    std::fs::write(dir.path().join(".git"), "gitdir: elsewhere").unwrap();
    let reserved = docs.join("_markview");
    std::fs::create_dir(&reserved).unwrap();
    std::fs::write(reserved.join("blocked.md"), "# Reserved").unwrap();

    let server =
        ServeProcess::try_start_with_env(&[&docs], 0, false, true, false, None, true).unwrap();
    let port = server.port;
    let host = format!("localhost:{port}");
    let page = http_get_local(port, "/");
    let token = review_token(&page);
    let token = token.as_str();
    let nonce = page
        .split("nonce-")
        .nth(1)
        .unwrap()
        .split('\'')
        .next()
        .unwrap();
    assert!(page.contains(&format!("nonce=\"{nonce}\"")));
    assert!(!http_get(port, "/").contains(&format!("nonce=\"{nonce}\"")));
    assert!(http_get(port, "/_markview/blocked.md").starts_with("HTTP/1.1 403"));
    let route = "/_markview/annotations?doc=%2Freview.md";
    let get = |method: &str, endpoint: &str, host: &str| {
        raw_http(
            port,
            &format!("{method} {endpoint} HTTP/1.1\r\nHost: {host}\r\n\r\n"),
        )
    };
    assert!(get("GET", route, "evil.example").starts_with("HTTP/1.1 403"));
    assert!(get("OPTIONS", route, &host).starts_with("HTTP/1.1 405"));
    assert!(get("GET", "/_markview/blocked.md", &host).starts_with("HTTP/1.1 404"));
    assert!(
        get("GET", "/_markview/annotations?doc=/../outside.md", &host).starts_with("HTTP/1.1 404")
    );
    let head = get("HEAD", route, &host);
    assert!(head.starts_with("HTTP/1.1 200"));
    assert!(head.ends_with("\r\n\r\n"));
    assert!(!head.contains("Access-Control-Allow"));

    let post = |token: &str, form: &str| {
        raw_http(port, &format!(
        "POST /_markview/annotations HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/x-www-form-urlencoded\r\nX-Markview-Token: {token}\r\nContent-Length: {}\r\n\r\n{form}",
        form.len()
    ))
    };
    let form = format!("op=add&doc=%2Freview.md&body=First&quote=%F0%9F%98%80+text&start=0&end=5&source_revision={}", source_revision(source));
    assert!(post("guessed", &form).starts_with("HTTP/1.1 403"));
    assert!(post(token, "op=add&doc=/review.md&body=%ZZ").starts_with("HTTP/1.1 400"));
    let store_path = dir.path().join(".markview/annotations/docs/review.md");
    assert!(!store_path.exists());
    let oversized = format!("POST /_markview/annotations HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/x-www-form-urlencoded\r\nX-Markview-Token: {token}\r\nContent-Length: {}\r\n\r\n", FORM_CAP + 1);
    assert!(raw_http(port, &oversized).starts_with("HTTP/1.1 400"));

    let mut events = open_reload_stream(port);
    let first = post(token, &form);
    assert!(first.starts_with("HTTP/1.1 200"), "{first}");
    assert!(first.contains("\"range\":[0,5]"));
    let event = read_until(&mut events, "data: /review.md", Duration::from_secs(8));
    assert!(event.contains("event: annotations"), "{event}");
    let revision = first
        .split("\"store_revision\":\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap();

    let large = format!(
        "op=add&doc=/review.md&body={}",
        "%F0%9F%98%80".repeat(10_000)
    );
    assert!(post(token, &large).starts_with("HTTP/1.1 200"));
    assert!(post(
        token,
        &format!("op=delete&doc=/review.md&id=a1&store_revision={revision}")
    )
    .starts_with("HTTP/1.1 409"));
    assert!(get("GET", route, &host).contains("First"));

    let read_only = ServeProcess::start(&document);
    let read_only_host = format!("127.0.0.1:{}", read_only.port);
    let response = raw_http(
        read_only.port,
        &format!("GET {route} HTTP/1.1\r\nHost: {read_only_host}\r\n\r\n"),
    );
    assert!(response.contains("First"));
    let response = raw_http(
        read_only.port,
        &format!("POST /_markview/annotations HTTP/1.1\r\nHost: {read_only_host}\r\n\r\n"),
    );
    assert!(response.starts_with("HTTP/1.1 405"));
}

#[test]
fn serve_mode_blocks_executable_destinations_but_keeps_data_images() {
    let dir = tempfile::tempdir().unwrap();
    let doc = dir.path().join("links.md");
    let other = dir.path().join("other.md");
    std::fs::write(
        &doc,
        concat!(
            "# Links\n\n",
            "[mail](mailto:a@example.com) [phone](tel:+15550100) [host](//example.com/x) [colon](a:b.md)\n",
            "[ref][other] <https://example.com/auto> <mailto:auto@example.com>\n\n",
            "![dot](data:image/png;base64,iVBORw0KGgo=) ![svg](DATA:Image/SVG+xml,%3Csvg/%3E)\n\n",
            "[page](data:text/html,hi) ![page](data:text/html,hi) ![bare](data:,hi)\n",
            "[png](data:image/png;base64,iVBORw0KGgo=)\n\n",
            "[js](javascript:alert(1)) ![js](javascript:alert(1))\n",
            "[tab](java&#x09;script:alert(1)) ![tab](java&#x09;script:alert(1))\n",
            "[case](JaVaScRiPt:alert(1)) ![entity](&#106;avascript:alert(1))\n",
            "[vb](vbscript:msgbox(1)) ![vb](&#118;bscript:msgbox(1))\n\n",
            "[other]: other.md#frag\n",
        ),
    )
    .unwrap();
    std::fs::write(&other, "# Other\n").unwrap();
    let server = ServeProcess::start_with_files(&[doc.as_path(), other.as_path()]);
    let page = http_get(server.port, "/");

    // Safe destinations are rendered as written, and document links rewritten.
    assert!(
        page.contains(concat!(
            r#"<p><a href="mailto:a@example.com">mail</a> <a href="tel:+15550100">phone</a> "#,
            r#"<a href="//example.com/x">host</a> <a href="a:b.md">colon</a>"#,
            "\n",
            r#"<a href="/other.md#frag">ref</a> "#,
            r#"<a href="https://example.com/auto">https://example.com/auto</a> "#,
            r#"<a href="mailto:auto@example.com">mailto:auto@example.com</a></p>"#,
        )),
        "{page}"
    );
    assert!(
        page.contains(concat!(
            r#"<p><img src="data:image/png;base64,iVBORw0KGgo=" alt="dot" /> "#,
            r#"<img src="DATA:Image/SVG+xml,%3Csvg/%3E" alt="svg" /></p>"#,
        )),
        "{page}"
    );
    // `data:` stays blocked for links, and for images that are not images.
    assert!(
        page.contains(concat!(
            r##"<p><a href="#">page</a> <img src="#" alt="page" /> <img src="#" alt="bare" />"##,
            "\n",
            r##"<a href="#">png</a></p>"##,
        )),
        "{page}"
    );
    assert!(
        page.contains(concat!(
            r##"<p><a href="#">js</a> <img src="#" alt="js" />"##,
            "\n",
            r##"<a href="#">tab</a> <img src="#" alt="tab" />"##,
            "\n",
            r##"<a href="#">case</a> <img src="#" alt="entity" />"##,
            "\n",
            r##"<a href="#">vb</a> <img src="#" alt="vb" /></p>"##,
        )),
        "{page}"
    );
}

/// Fetches a page the way a browser on this machine does. The write token is
/// only embedded for a loopback `Host` that names the server's own port.
fn http_get_local(port: u16, route: &str) -> String {
    raw_http(
        port,
        &format!("GET {route} HTTP/1.1\r\nHost: localhost:{port}\r\n\r\n"),
    )
}

/// The review token a `--annotate` page embeds for its own POSTs.
fn review_token(page: &str) -> String {
    page.split("const token = \"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .expect("review token")
        .to_owned()
}

fn post_annotation(port: u16, token: &str, form: &str) -> String {
    raw_http(port, &format!(
        "POST /_markview/annotations HTTP/1.1\r\nHost: localhost:{port}\r\nContent-Type: application/x-www-form-urlencoded\r\nX-Markview-Token: {token}\r\nContent-Length: {}\r\n\r\n{form}",
        form.len()
    ))
}

#[test]
fn serve_annotations_accepts_writes_next_to_a_hand_edited_section() {
    let dir = tempfile::tempdir().unwrap();
    let doc = dir.path().join("review.md");
    std::fs::write(&doc, "one\n\nthree\n").unwrap();
    let store_dir = dir.path().join(".markview/annotations");
    std::fs::create_dir_all(&store_dir).unwrap();
    let store = store_dir.join("review.md");
    let hand_edited = "## a1 [open] line 3\n\nPlease reword this line.\n";
    std::fs::write(&store, hand_edited).unwrap();
    let server =
        ServeProcess::try_start_with_env(&[&doc], 0, false, false, false, None, true).unwrap();
    let token = review_token(&http_get_local(server.port, "/"));

    let added = post_annotation(
        server.port,
        &token,
        "op=add&doc=%2Freview.md&body=Whole+document",
    );
    assert!(added.starts_with("HTTP/1.1 200"), "{added}");
    let written = std::fs::read_to_string(&store).unwrap();
    assert!(written.starts_with(hand_edited), "{written}");
    assert!(written.contains("## a2 [open] document"), "{written}");

    let revision = added
        .split("\"store_revision\":\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .unwrap();
    let unsafe_edit = post_annotation(
        server.port,
        &token,
        &format!("op=edit&doc=%2Freview.md&id=a1&store_revision={revision}&body=%3E+not+a+quote"),
    );
    assert!(unsafe_edit.starts_with("HTTP/1.1 409"), "{unsafe_edit}");
    assert!(unsafe_edit.contains("annotation a1 "), "{unsafe_edit}");
    assert_eq!(std::fs::read_to_string(&store).unwrap(), written);
}

#[test]
fn serve_annotations_rejects_a_body_that_exceeds_the_read_deadline() {
    let dir = tempfile::tempdir().unwrap();
    let doc = dir.path().join("review.md");
    std::fs::write(&doc, "# Test").unwrap();
    let server =
        ServeProcess::try_start_with_env(&[&doc], 0, false, false, false, None, true).unwrap();
    let page = http_get_local(server.port, "/");
    let token = review_token(&page);
    let token = token.as_str();
    let mut stream = TcpStream::connect(("127.0.0.1", server.port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(6)))
        .unwrap();
    write!(stream, "POST /_markview/annotations HTTP/1.1\r\nHost: localhost:{}\r\nContent-Type: application/x-www-form-urlencoded\r\nX-Markview-Token: {token}\r\nContent-Length: 20\r\n\r\nop=add", server.port).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 400"));
    assert!(!dir.path().join(".markview").exists());
}

/// Sends raw request bytes and returns the response up to the end of its
/// head, which is all a never-ending `/events` stream can give.
fn raw_response_head(port: u16, request: &[u8]) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_millis(200)))
        .unwrap();
    stream.write_all(request).unwrap();
    read_until(&mut stream, "\r\n\r\n", Duration::from_secs(8))
}

#[test]
fn serve_mode_reads_ordinary_requests_as_leniently_as_before_annotations() {
    let dir = tempfile::tempdir().unwrap();
    let doc = dir.path().join("doc.md");
    std::fs::write(&doc, "# Doc\n\n![a](a.png)\n").unwrap();
    std::fs::write(dir.path().join("a.png"), b"\x89PNG\r\n\x1a\n").unwrap();
    // `--annotate` must not make pages, assets or `/events` any stricter.
    let server =
        ServeProcess::try_start_with_env(&[&doc], 0, false, false, false, None, true).unwrap();
    let cookie = "a".repeat(40_000);
    for route in ["/", "/a.png", "/events"] {
        let cases: [(&str, Vec<u8>); 4] = [
            (
                "bare LF line endings",
                format!("GET {route} HTTP/1.1\nHost: localhost\n\n").into_bytes(),
            ),
            (
                "Latin-1 header byte",
                [
                    format!("GET {route} HTTP/1.1\r\nHost: localhost\r\nUser-Agent: caf")
                        .as_bytes(),
                    b"\xe9\r\n\r\n",
                ]
                .concat(),
            ),
            (
                "40 KB cookie",
                format!("GET {route} HTTP/1.1\r\nHost: localhost\r\nCookie: {cookie}\r\n\r\n")
                    .into_bytes(),
            ),
            (
                "HTTP/1.0 request line",
                format!("GET {route} HTTP/1.0\r\n\r\n").into_bytes(),
            ),
        ];
        for (name, request) in cases {
            // Repeated: an unread request must not reset the connection.
            for attempt in 0..5 {
                let response = raw_response_head(server.port, &request);
                assert!(
                    response.starts_with("HTTP/1.1 200 OK"),
                    "{route}, {name}, attempt {attempt}: {response:?}"
                );
            }
        }
    }

    // Headers that arrive after the strict reader's 3 s deadline: an ordinary
    // route never waits for them.
    let mut slow = TcpStream::connect(("127.0.0.1", server.port)).unwrap();
    slow.set_read_timeout(Some(Duration::from_millis(200)))
        .unwrap();
    slow.write_all(b"GET / HTTP/1.1\r\n").unwrap();
    std::thread::sleep(Duration::from_millis(3_500));
    let response = read_until(&mut slow, "\r\n\r\n", Duration::from_secs(8));
    assert!(response.starts_with("HTTP/1.1 200 OK"), "{response:?}");
}

#[test]
fn serve_annotations_endpoint_keeps_its_strict_request_parser() {
    let dir = tempfile::tempdir().unwrap();
    let doc = dir.path().join("doc.md");
    std::fs::write(&doc, "# Doc\n").unwrap();
    let server = ServeProcess::start(&doc);
    let host = format!("localhost:{}", server.port);
    let route = "/_markview/annotations?doc=%2Fdoc.md";
    let ok = raw_response_head(
        server.port,
        format!("GET {route} HTTP/1.1\r\nHost: {host}\r\n\r\n").as_bytes(),
    );
    assert!(ok.starts_with("HTTP/1.1 200 OK"), "{ok:?}");
    let cases: [(&str, Vec<u8>); 5] = [
        (
            "duplicate Content-Length",
            format!("GET {route} HTTP/1.1\r\nHost: {host}\r\nContent-Length: 0\r\nContent-Length: 0\r\n\r\n")
                .into_bytes(),
        ),
        (
            "bare LF line endings",
            format!("GET {route} HTTP/1.1\nHost: {host}\n\n").into_bytes(),
        ),
        (
            "non-UTF-8 header",
            [
                format!("GET {route} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: caf").as_bytes(),
                b"\xe9\r\n\r\n",
            ]
            .concat(),
        ),
        (
            "fragment in target",
            format!("GET {route}#top HTTP/1.1\r\nHost: {host}\r\n\r\n").into_bytes(),
        ),
        (
            "space in target",
            format!("GET /_markview/annotations?doc=/doc .md HTTP/1.1\r\nHost: {host}\r\n\r\n")
                .into_bytes(),
        ),
    ];
    for (name, request) in cases {
        let response = raw_response_head(server.port, &request);
        assert!(response.starts_with("HTTP/1.1 400"), "{name}: {response:?}");
    }
}

#[test]
fn serve_annotations_delivers_the_error_when_rejecting_a_large_post() {
    let dir = tempfile::tempdir().unwrap();
    let doc = dir.path().join("doc.md");
    std::fs::write(&doc, "# Doc\n").unwrap();
    let server =
        ServeProcess::try_start_with_env(&[&doc], 0, false, false, false, None, true).unwrap();
    let port = server.port;
    let token = review_token(&http_get_local(port, "/"));
    let body = format!("op=add&doc=%2Fdoc.md&body={}", "x".repeat(150_000));
    let request = |host: &str, token: &str, content_type: &str| {
        format!(
            "POST /_markview/annotations HTTP/1.1\r\nHost: {host}\r\nContent-Type: {content_type}\r\nX-Markview-Token: {token}\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
    };
    let local = format!("localhost:{port}");
    let form = "application/x-www-form-urlencoded";
    for (name, request, status, error) in [
        (
            "wrong token",
            request(&local, "guessed", form),
            "HTTP/1.1 403",
            r#"{"error":"invalid annotation token"}"#,
        ),
        (
            "bad framing",
            request(&local, &token, "text/plain"),
            "HTTP/1.1 400",
            r#"{"error":"invalid annotation request framing"}"#,
        ),
        (
            "bad Host",
            request(&format!("evil.test:{port}"), &token, form),
            "HTTP/1.1 403",
            r#"{"error":"invalid Host"}"#,
        ),
    ] {
        // The server answers before reading the body. Every attempt must
        // still deliver the whole response instead of a connection reset.
        let delivered = (0..20)
            .filter(|_| {
                let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(8)))
                    .unwrap();
                let _ = stream.write_all(request.as_bytes());
                let mut response = Vec::new();
                let _ = stream.read_to_end(&mut response);
                let response = String::from_utf8_lossy(&response);
                response.starts_with(status) && response.ends_with(error)
            })
            .count();
        assert_eq!(delivered, 20, "{name}: complete responses out of 20");
    }
    assert!(!dir.path().join(".markview").exists());
}

/// A copy of `tests/fixtures/serve-baseline`. Its default page, served
/// without annotations, is `tests/fixtures/serve-baseline.expected.html`: no
/// review machinery, only the copy controls, alert styles and live-reload
/// shell.
fn baseline_site() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let site = dir.path().join("site");
    std::fs::create_dir(&site).unwrap();
    for name in ["README.md", "guide.md", "chart.png"] {
        std::fs::copy(
            std::path::Path::new("tests/fixtures/serve-baseline").join(name),
            site.join(name),
        )
        .unwrap();
    }
    (dir, site)
}

/// Replaces the document's mtime, the only part of a page that varies.
fn without_timestamp(body: &str) -> String {
    let marker = "data-timestamp-ms=\"";
    let (before, rest) = body.split_once(marker).expect("modified timestamp");
    let (digits, after) = rest.split_once('"').unwrap();
    assert!(digits.bytes().all(|byte| byte.is_ascii_digit()));
    format!("{before}{marker}TIMESTAMP\"{after}")
}

/// The digest in the copy assets' `?v=` links: FNV-1a/64 over copy.js then
/// copy.css, and nothing else. A plain page links only these two, so editing
/// the review scripts must not change its bytes.
fn copy_assets_version() -> String {
    markview::annotations::source_revision(concat!(
        include_str!("../src/annotations/copy.js"),
        include_str!("../src/annotations/copy.css"),
    ))
}

/// Replaces the copy assets' version, which follows their content, so the
/// fixture does not have to be regenerated when they are edited.
fn without_copy_version(body: &str) -> String {
    let version = copy_assets_version();
    for name in ["copy.css", "copy.js"] {
        assert!(
            body.contains(&format!("/_markview/{name}?v={version}\"")),
            "{name} is not versioned by the copy assets alone"
        );
    }
    body.replace(&format!("?v={version}\""), "?v=COPY_VERSION\"")
}

fn plain_page_body(body: &str) -> String {
    without_copy_version(&without_timestamp(body))
}

const BASELINE_HEAD: &str = "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nX-Content-Type-Options: nosniff\r\nContent-Length: 21928\r\nConnection: close";
const BASELINE_BODY: &str = include_str!("fixtures/serve-baseline.expected.html");

#[test]
fn serve_page_without_annotations_matches_the_plain_page_fixture() {
    let (_dir, site) = baseline_site();
    let server = ServeProcess::start_dir(&site);
    let response = http_get(server.port, "/");
    let (head, body) = response.split_once("\r\n\r\n").unwrap();
    assert_eq!(head, BASELINE_HEAD);
    assert_eq!(plain_page_body(body), BASELINE_BODY);
}

#[test]
fn plain_page_carries_copy_controls_and_no_review_machinery() {
    let (_dir, site) = baseline_site();
    let server = ServeProcess::start_dir(&site);
    let page = http_get(server.port, "/");
    let (head, body) = page.split_once("\r\n\r\n").unwrap();
    for review in [
        "/_markview/review.js",
        "/_markview/review.css",
        "/_markview/verdict.js",
        "markviewServeReview(",
        "createMarkviewVerdict",
        "data-source-revision",
        "data-markview-route",
        "nonce=",
    ] {
        assert!(!body.contains(review), "{review}");
    }
    // No token, so no policy and no nonce to carry.
    assert!(!head.contains("Content-Security-Policy"), "{head}");
    // Its extras: the copy controls, the alert styles, the live-reload shell.
    assert_eq!(body.matches("/_markview/copy.css?v=").count(), 1);
    assert_eq!(body.matches("/_markview/copy.js?v=").count(), 1);
    assert!(body.contains(".markdown-alert-note"));
    assert!(body.contains("new EventSource('/events')"));
    assert_eq!(
        body.matches("<script").count(),
        3,
        "theme, copy.js and live reload"
    );
}

#[test]
fn plain_page_version_is_independent_of_the_review_scripts() {
    use markview::annotations::source_revision;
    let (_dir, site) = baseline_site();
    let server = ServeProcess::start_dir(&site);
    let page = http_get(server.port, "/");
    let token = page
        .split("/_markview/copy.js?v=")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .expect("versioned copy script");
    assert_eq!(token, copy_assets_version());
    // The same digest over the copy assets plus a review script, edited or
    // not, is a different token: the review scripts are not hashed into it.
    let review = include_str!("../src/annotations/review.js");
    let verdict = include_str!("../src/annotations/verdict.js");
    let copy = concat!(
        include_str!("../src/annotations/copy.js"),
        include_str!("../src/annotations/copy.css"),
    );
    for script in [
        review.to_owned(),
        format!("{review}// edited"),
        verdict.to_owned(),
    ] {
        assert_ne!(token, source_revision(&format!("{script}{copy}")));
        assert_ne!(token, source_revision(&format!("{copy}{script}")));
    }
    // A review page versions its own assets separately.
    let store = site.join(".markview/annotations");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(store.join("guide.md"), "## a1 [open] document\n\nExpand.\n").unwrap();
    let annotated = http_get(server.port, "/guide.md");
    let review_token = annotated
        .split("/_markview/review.js?v=")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .expect("versioned review script");
    assert_ne!(review_token, token);
    assert!(annotated.contains(&format!("/_markview/copy.js?v={token}\"")));
}

#[test]
fn serve_injects_review_hooks_only_where_they_are_needed() {
    let (dir, site) = baseline_site();
    let store = dir.path().join("site/.markview/annotations");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(
        store.join("guide.md"),
        "## a1 [open] document\n\nExpand this.\n",
    )
    .unwrap();
    let hooks = [
        "<main data-source-revision=\"",
        "<link rel=\"stylesheet\" href=\"/_markview/review.css?v=",
        " src=\"/_markview/review.js?v=",
        "markviewServeReview(route, token, ",
        "events.addEventListener('annotations'",
    ];

    // Read-only serving: only the document that has a store pays for review.
    let read_only = ServeProcess::start_dir(&site);
    let plain = http_get(read_only.port, "/");
    let (head, body) = plain.split_once("\r\n\r\n").unwrap();
    assert_eq!(head, BASELINE_HEAD);
    assert_eq!(plain_page_body(body), BASELINE_BODY);
    let annotated = http_get(read_only.port, "/guide.md");
    for hook in hooks {
        assert!(annotated.contains(hook), "{hook}: {annotated}");
    }
    assert!(annotated.contains("const token = null"));
    assert!(!annotated.contains("Content-Security-Policy"));

    // `--annotate`: every page can take a first comment.
    let writable =
        ServeProcess::try_start_with_env(&[&site], 0, false, false, false, None, true).unwrap();
    for route in ["/", "/guide.md"] {
        let page = http_get_local(writable.port, route);
        for hook in hooks {
            assert!(page.contains(hook), "{route}, {hook}");
        }
        assert_eq!(review_token(&page).len(), 48);
        let nonce = page
            .split("script-src 'nonce-")
            .nth(1)
            .and_then(|rest| rest.split('\'').next())
            .expect("script nonce in the policy");
        assert!(page.contains(&format!(
            "<script nonce=\"{nonce}\" src=\"/_markview/review.js?v="
        )));
        assert!(!page.contains("<script>") && !page.contains("<script src"));
    }

    // The script and stylesheet are fetched once and cached, not inlined.
    for port in [read_only.port, writable.port] {
        let page = http_get_local(port, "/guide.md");
        assert!(!page.contains("function createMarkviewReview"));
        assert!(!page.contains(".markview-review-toolbar"));
        for (name, content_type, source) in [
            (
                "review.js",
                "text/javascript; charset=utf-8",
                include_str!("../src/annotations/review.js"),
            ),
            (
                "review.css",
                "text/css; charset=utf-8",
                include_str!("../src/annotations/review.css"),
            ),
        ] {
            let link = page
                .split(&format!("\"/_markview/{name}?v="))
                .nth(1)
                .and_then(|rest| rest.split('"').next())
                .map(|version| format!("/_markview/{name}?v={version}"))
                .expect("versioned link");
            // Fetched like any other page asset, whatever the Host.
            let response = raw_http(
                port,
                &format!("GET {link} HTTP/1.1\nHost: other.localhost\n\n"),
            );
            let (head, body) = response.split_once("\r\n\r\n").unwrap();
            assert!(head.starts_with("HTTP/1.1 200 OK"), "{head}");
            assert!(head.contains(&format!("\r\nContent-Type: {content_type}\r\n")));
            assert!(
                head.contains("\r\nCache-Control: public, max-age=31536000, immutable\r\n"),
                "{head}"
            );
            assert_eq!(body, source);
            let head_only = http_request(port, "HEAD", &link);
            assert!(head_only.starts_with("HTTP/1.1 200 OK"));
            assert!(head_only.ends_with("\r\n\r\n"));
        }
    }
}

#[test]
fn serve_reloads_a_plain_page_when_its_first_annotation_appears() {
    let (dir, site) = baseline_site();
    let server = ServeProcess::start_dir(&site);
    // Served without review hooks, so only a full reload can add them.
    assert!(!http_get(server.port, "/guide.md").contains("/_markview/review.js"));
    let mut events = open_reload_stream(server.port);
    let store = dir.path().join("site/.markview/annotations");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(
        store.join("guide.md"),
        "## a1 [open] document\n\nExpand this.\n",
    )
    .unwrap();
    let event = read_until(&mut events, "data: rescan", Duration::from_secs(8));
    assert!(event.contains("data: rescan"), "{event}");
    assert!(http_get(server.port, "/guide.md").contains("/_markview/review.js"));

    // Once the page has its hooks, later store changes only refresh comments.
    std::fs::write(
        store.join("guide.md"),
        "## a1 [resolved] document\n\nExpand this.\n",
    )
    .unwrap();
    let event = read_until(&mut events, "data: /guide.md", Duration::from_secs(8));
    assert!(event.contains("event: annotations"), "{event}");
    assert!(!event.contains("data: rescan"), "{event}");
}

#[test]
fn serve_mode_never_serves_the_markview_tree_as_assets() {
    let dir = tempfile::tempdir().unwrap();
    let hidden = dir.path().join(".assets");
    let store = dir.path().join(".markview/annotations");
    std::fs::create_dir_all(&hidden).unwrap();
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(hidden.join("data.txt"), "allowed").unwrap();
    std::fs::write(
        store.join("doc.md"),
        "## a1 [open] document\n\nPrivate note\n",
    )
    .unwrap();
    std::fs::write(store.join("doc.md.markview-lock"), "").unwrap();
    std::fs::write(dir.path().join(".markview/notes.txt"), "also private").unwrap();
    let doc = dir.path().join("doc.md");
    std::fs::write(
        &doc,
        concat!(
            "# Doc\n\n[hidden](.assets/data.txt) [store](.markview/annotations/doc.md)\n",
            "[lock](.markview/annotations/doc.md.markview-lock) [other](.markview/notes.txt)\n",
        ),
    )
    .unwrap();
    let server = ServeProcess::start(&doc);
    // A referenced asset in a hidden directory is served as before.
    assert!(http_get(server.port, "/.assets/data.txt").ends_with("allowed"));
    // Referencing something under `.markview` does not make it an asset.
    for route in [
        "/.markview/annotations/doc.md",
        "/.markview/annotations/doc.md.markview-lock",
        "/.markview/notes.txt",
    ] {
        let response = http_get(server.port, route);
        assert!(response.starts_with("HTTP/1.1 404"), "{route}: {response}");
        assert!(!response.contains("rivate"), "{route}");
    }
}

#[test]
fn serve_annotate_pages_allow_only_their_own_nonce_scripts() {
    let dir = tempfile::tempdir().unwrap();
    let doc = dir.path().join("doc.md");
    std::fs::write(
        &doc,
        "# Doc\n\n<script>alert(1)</script>\n\n<img src=x onerror=alert(2)>\n\n[x](javascript:alert(3))\n",
    )
    .unwrap();
    let server =
        ServeProcess::try_start_with_env(&[&doc], 0, false, false, false, None, true).unwrap();
    let page = http_get_local(server.port, "/");
    let (head, body) = page.split_once("\r\n\r\n").unwrap();
    let policy = head
        .lines()
        .find_map(|line| line.strip_prefix("Content-Security-Policy: "))
        .expect("policy header");
    let nonce = policy
        .split("'nonce-")
        .nth(1)
        .and_then(|rest| rest.split('\'').next())
        .unwrap();
    assert_eq!(nonce.len(), 48);
    // The exact policy a browser enforces, not a copy kept in a fixture.
    assert_eq!(
        policy,
        format!(
            "default-src 'self'; script-src 'nonce-{nonce}'; style-src 'self' 'unsafe-inline'; img-src 'self' https: http: data:; object-src 'none'; base-uri 'none'; frame-src 'none'; frame-ancestors 'none'; form-action 'none'"
        )
    );
    // Every script element in the page is Markview's and carries the nonce.
    let scripts: Vec<_> = body.match_indices("<script").collect();
    assert_eq!(
        scripts.len(),
        5,
        "theme, review.js, copy.js, start-up call, live reload"
    );
    for (index, _) in &scripts {
        assert!(
            body[*index..].starts_with(&format!("<script nonce=\"{nonce}\"")),
            "{}",
            &body[*index..*index + 60]
        );
    }
    for name in ["review.js", "copy.js"] {
        assert_eq!(
            body.matches(&format!(
                "<script nonce=\"{nonce}\" src=\"/_markview/{name}?v="
            ))
            .count(),
            1,
            "{name}"
        );
    }
    assert_eq!(
        body.matches(&format!("<script nonce=\"{nonce}\">")).count(),
        3,
        "theme, start-up call and live reload"
    );
    // What the document tried to inject is text, or a dead link.
    assert!(body.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
    assert!(body.contains("&lt;img src=x onerror=alert(2)&gt;"));
    assert!(body.contains("<a href=\"#\">x</a>"));
    // A page without the token needs no policy and gets none.
    let plain = ServeProcess::start(&doc);
    assert!(!http_get(plain.port, "/").contains("Content-Security-Policy"));
}

#[test]
fn serve_annotations_accepts_only_loopback_host_spellings() {
    let dir = tempfile::tempdir().unwrap();
    let doc = dir.path().join("doc.md");
    std::fs::write(&doc, "# Doc\n").unwrap();
    let server =
        ServeProcess::try_start_with_env(&[&doc], 0, false, false, false, None, true).unwrap();
    let port = server.port;
    let status = |host: &str| {
        let response = raw_http(
            port,
            &format!("GET /_markview/annotations?doc=%2Fdoc.md HTTP/1.1\r\nHost: {host}\r\n\r\n"),
        );
        response[..response.len().min(12)].to_owned()
    };
    for host in [
        format!("localhost:{port}"),
        format!("LOCALHOST:{port}"),
        format!("localhost.:{port}"),
        format!("LocalHost.:{port}"),
        format!("127.0.0.1:{port}"),
    ] {
        assert_eq!(status(&host), "HTTP/1.1 200", "{host}");
    }
    for host in [
        format!("localhost:{}", port.wrapping_add(1).max(1)),
        "localhost".to_owned(),
        format!("evil.test:{port}"),
        format!("localhost.evil.test:{port}"),
        format!("localhost..:{port}"),
        format!("127.0.0.1.:{port}"),
        format!("127.0.0.2:{port}"),
        format!("localhost:{port}:{port}"),
        format!("localhost:0{port}"),
    ] {
        assert_eq!(status(&host), "HTTP/1.1 403", "{host}");
    }

    // A page reached through any other name is rendered read-only, so a
    // DNS-rebinding page cannot read the write token out of it.
    let page = |host: &str| raw_http(port, &format!("GET / HTTP/1.1\r\nHost: {host}\r\n\r\n"));
    for host in [format!("localhost:{port}"), format!("LOCALHOST.:{port}")] {
        assert_eq!(review_token(&page(&host)).len(), 48, "{host}");
    }
    for request in [
        format!("GET / HTTP/1.1\r\nHost: evil.test:{port}\r\n\r\n"),
        format!("GET / HTTP/1.1\r\nHost: localhost:{port}\r\nHost: evil.test:{port}\r\n\r\n"),
        "GET / HTTP/1.0\r\n\r\n".to_owned(),
    ] {
        let page = raw_http(port, &request);
        assert!(page.starts_with("HTTP/1.1 200"), "{request:?}");
        assert!(page.contains("const token = null"), "{request:?}");
        assert!(!page.contains("const token = \""), "{request:?}");
    }
}

#[test]
fn serve_annotate_sandboxes_assets_and_warns_about_a_reserved_directory() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("site");
    let reserved = root.join("_markview");
    std::fs::create_dir_all(&reserved).unwrap();
    std::fs::write(root.join("doc.md"), "# Doc\n\n[data](data.xml)\n").unwrap();
    std::fs::write(
        root.join("data.xml"),
        "<svg xmlns=\"http://www.w3.org/2000/svg\"/>",
    )
    .unwrap();
    std::fs::write(reserved.join("one.md"), "# One").unwrap();
    std::fs::write(reserved.join("two.md"), "# Two").unwrap();

    let annotate =
        ServeProcess::try_start_with_env(&[&root], 0, false, true, false, None, true).unwrap();
    let asset = http_get(annotate.port, "/data.xml");
    assert!(asset.starts_with("HTTP/1.1 200"), "{asset}");
    assert!(
        asset.contains("\r\nContent-Security-Policy: sandbox\r\n"),
        "{asset}"
    );
    assert!(
        asset.contains("\r\nX-Content-Type-Options: nosniff\r\n"),
        "{asset}"
    );
    // The skipped directory is named once, however many files it holds.
    let warning = format!(
        "markview: warning: not serving {}: the /_markview/ route prefix is reserved",
        reserved.canonicalize().unwrap().display()
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while !annotate.stderr_so_far().contains(&warning) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(annotate.stderr_so_far().matches(&warning).count(), 1);
    assert!(http_get(annotate.port, "/_markview/one.md").starts_with("HTTP/1.1 403"));

    let plain = ServeProcess::start_recursive_dir(&root);
    let asset = http_get(plain.port, "/data.xml");
    assert!(asset.starts_with("HTTP/1.1 200"), "{asset}");
    assert!(!asset.contains("Content-Security-Policy"), "{asset}");
}

#[test]
fn serve_annotations_gives_up_on_headers_that_never_finish() {
    let dir = tempfile::tempdir().unwrap();
    let doc = dir.path().join("doc.md");
    std::fs::write(&doc, "# Doc\n").unwrap();
    let server = ServeProcess::start(&doc);
    let mut stream = TcpStream::connect(("127.0.0.1", server.port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_millis(200)))
        .unwrap();
    // The request line and one header arrive, the blank line never does.
    write!(
        stream,
        "GET /_markview/annotations?doc=%2Fdoc.md HTTP/1.1\r\nHost: localhost:{}\r\n",
        server.port
    )
    .unwrap();
    let started = Instant::now();
    let response = read_until(&mut stream, "Malformed request", Duration::from_secs(15));
    let waited = started.elapsed();
    assert!(response.starts_with("HTTP/1.1 400"), "{response:?}");
    // The endpoint's read deadline is 3 s: it neither answers an unfinished
    // request early nor holds the connection open for it.
    assert!(waited > Duration::from_millis(1_500), "{waited:?}");
    assert!(waited < Duration::from_secs(15), "{waited:?}");
}

#[test]
fn serve_reports_store_replacement_and_deletion_above_the_serve_root() {
    let dir = tempfile::tempdir().unwrap();
    let docs = dir.path().join("docs");
    let store_dir = dir.path().join(".markview/annotations/docs");
    std::fs::create_dir_all(&docs).unwrap();
    std::fs::create_dir_all(&store_dir).unwrap();
    std::fs::write(dir.path().join(".git"), "gitdir: elsewhere").unwrap();
    let doc = docs.join("guide.md");
    std::fs::write(&doc, "# Guide\n").unwrap();
    let store = store_dir.join("guide.md");
    std::fs::write(&store, "## a1 [open] document\n\nExpand this.\n").unwrap();

    // Only `docs/` is served: the store sits outside the served tree.
    let server = ServeProcess::start(&doc);
    let endpoint = "/_markview/annotations?doc=%2Fguide.md";
    assert!(http_get(server.port, "/").contains("/_markview/review.js"));
    assert!(http_get_local(server.port, endpoint).contains("\"status\":\"open\""));
    let mut events = open_reload_stream(server.port);

    // An editor or agent saves atomically: a temporary file renamed over.
    let temporary = store_dir.join("guide.md.tmp");
    std::fs::write(&temporary, "## a1 [resolved] document\n\nExpand this.\n").unwrap();
    std::fs::rename(&temporary, &store).unwrap();
    let event = read_until(&mut events, "data: /guide.md", Duration::from_secs(8));
    // (No claim here about `data: reload`: the file watcher can still be
    // delivering the document's own creation. That a store change does not
    // reload the document is tested at the event handler in `src/main.rs`.)
    assert!(
        event.contains("event: annotations\ndata: /guide.md"),
        "{event}"
    );
    assert!(http_get_local(server.port, endpoint).contains("\"status\":\"resolved\""));

    std::fs::remove_file(&store).unwrap();
    let event = read_until(&mut events, "data: /guide.md", Duration::from_secs(8));
    assert!(
        event.contains("event: annotations\ndata: /guide.md"),
        "{event}"
    );
    assert!(http_get_local(server.port, endpoint).contains("\"annotations\":[]"));
}

#[test]
fn serve_mode_daemon_passes_annotate_to_the_background_server() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("README.md");
    std::fs::write(&file, "# Daemonized\n").unwrap();
    let output = std::process::Command::new(cargo_bin("markview"))
        .env_remove("MARKVIEW_API_TOKEN")
        .current_dir(dir.path())
        .args(["--serve", "--annotate", "--daemon", "--port", "0"])
        .arg(&file)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let _daemon = DaemonProcess(number_after(&stdout, "pid "));
    let port: u16 = number_after(&stdout, "http://localhost:");
    poll_http_until_ready(port, Duration::from_secs(5));

    // The background server accepts a comment, so it was started writable.
    let token = review_token(&http_get_local(port, "/"));
    let added = post_annotation(port, &token, "op=add&doc=%2FREADME.md&body=From+the+daemon");
    assert!(added.starts_with("HTTP/1.1 200"), "{added}");
    let store = std::fs::read_to_string(dir.path().join(".markview/annotations/README.md"));
    assert!(store.unwrap().contains("From the daemon"));
}

fn raw_http(port: u16, request: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(8)))
        .unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}

struct ServeProcess {
    child: Child,
    port: u16,
    stderr: Arc<Mutex<String>>,
}

impl ServeProcess {
    fn start(file: &std::path::Path) -> Self {
        Self::start_with_files(&[file])
    }

    fn start_with_files(files: &[&std::path::Path]) -> Self {
        Self::start_with_args(files, unused_port())
    }

    fn start_dir(directory: &std::path::Path) -> Self {
        Self::start_with_args(&[directory], unused_port())
    }

    fn start_recursive_dir(directory: &std::path::Path) -> Self {
        Self::start_with_env(&[directory], unused_port(), false, true, false, None)
    }

    fn start_with_open(file: &std::path::Path) -> Self {
        Self::start_with_env(&[file], unused_port(), false, false, true, None)
    }

    fn start_with_args(inputs: &[&std::path::Path], port: u16) -> Self {
        Self::start_with_env(inputs, port, false, false, false, None)
    }

    fn start_with_disconnect_logging(file: &std::path::Path) -> Self {
        Self::start_with_env(&[file], unused_port(), true, false, false, None)
    }

    fn start_relative(cwd: &std::path::Path, relative_input: &str) -> Self {
        Self::start_with_env(
            &[std::path::Path::new(relative_input)],
            unused_port(),
            false,
            false,
            false,
            Some(cwd),
        )
    }

    fn start_with_env(
        inputs: &[&std::path::Path],
        port: u16,
        log_disconnects: bool,
        recurse: bool,
        open: bool,
        cwd: Option<&std::path::Path>,
    ) -> Self {
        for attempt in 0..5 {
            let port = if attempt == 0 { port } else { unused_port() };
            match Self::try_start_with_env(inputs, port, log_disconnects, recurse, open, cwd, false)
            {
                Ok(server) => return server,
                Err(message) if attempt < 4 && message.contains("already in use") => continue,
                Err(message) => panic!("{message}"),
            }
        }
        unreachable!("retry loop returns or panics")
    }

    fn try_start_with_env(
        inputs: &[&std::path::Path],
        port: u16,
        log_disconnects: bool,
        recurse: bool,
        open: bool,
        cwd: Option<&std::path::Path>,
        annotate: bool,
    ) -> Result<Self, String> {
        let mut cmd = std::process::Command::new(cargo_bin("markview"));
        cmd.env_remove("MARKVIEW_API_TOKEN").arg("--serve");
        if annotate {
            cmd.arg("--annotate");
        }
        for input in inputs {
            cmd.arg(input);
        }
        if recurse {
            cmd.arg("--recurse");
        }
        if open {
            cmd.arg("--open");
        }
        if let Some(cwd) = cwd {
            cmd.current_dir(cwd);
        }
        cmd.args(["--port", &port.to_string()]);
        if log_disconnects {
            cmd.env("MARKVIEW_LOG_EVENT_DISCONNECTS", "1");
        }
        Self::launch(cmd)
    }

    /// Spawns a prepared `--serve` command and waits for its startup line.
    fn launch(mut cmd: std::process::Command) -> Result<Self, String> {
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd.spawn().expect("spawn server");
        let stdout = child.stdout.take().expect("stdout");
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        if let Err(error) = reader.read_line(&mut line) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("read startup line: {error}"));
        }
        if line.is_empty() {
            let mut stderr = String::new();
            if let Some(mut pipe) = child.stderr.take() {
                let _ = pipe.read_to_string(&mut stderr);
            }
            let _ = child.wait();
            return Err(format!("server exited before startup: {stderr}"));
        }
        let port = parse_served_port(&line);

        let stderr_pipe = child.stderr.take().expect("stderr");
        let stderr = Arc::new(Mutex::new(String::new()));
        let stderr_writer = stderr.clone();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stderr_pipe);
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap_or(0) > 0 {
                if let Ok(mut buffer) = stderr_writer.lock() {
                    buffer.push_str(&line);
                }
                line.clear();
            }
        });

        Ok(Self {
            child,
            port,
            stderr,
        })
    }

    fn stderr_so_far(&self) -> String {
        self.stderr.lock().expect("stderr lock").clone()
    }

    fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for ServeProcess {
    fn drop(&mut self) {
        self.stop();
    }
}

fn parse_served_port(line: &str) -> u16 {
    let prefix = "Serving on http://localhost:";
    let rest = line
        .strip_prefix(prefix)
        .unwrap_or_else(|| panic!("unexpected startup line: {line:?}"));
    rest.split_whitespace()
        .next()
        .expect("port")
        .parse()
        .expect("numeric port")
}

fn http_get(port: u16, route: &str) -> String {
    http_request(port, "GET", route)
}

fn http_request(port: u16, method: &str, route: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    write!(
        stream,
        "{method} {route} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
    )
    .expect("write request");
    let mut response = Vec::new();
    stream.read_to_end(&mut response).expect("read response");
    String::from_utf8_lossy(&response).to_string()
}

fn open_reload_stream(port: u16) -> BufReader<TcpStream> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect events");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set timeout");
    stream
        .write_all(b"GET /events HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .expect("write request");
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    loop {
        line.clear();
        reader.read_line(&mut line).expect("read header");
        if line == "\r\n" {
            break;
        }
    }
    reader
}

fn unused_port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0))
        .expect("bind unused port")
        .local_addr()
        .expect("local addr")
        .port()
}

#[test]
fn event_reader_retries_interrupted_reads() {
    struct InterruptedOnce(bool);
    impl Read for InterruptedOnce {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            if !self.0 {
                self.0 = true;
                return Err(std::io::ErrorKind::Interrupted.into());
            }
            (&b"data: rescan\n\n"[..]).read(buffer)
        }
    }
    assert!(read_until(
        &mut InterruptedOnce(false),
        "data: rescan",
        Duration::from_secs(1)
    )
    .contains("data: rescan"));
}

fn read_until<R: Read>(reader: &mut R, needle: &str, timeout: Duration) -> String {
    let start = Instant::now();
    let mut buffer = [0_u8; 128];
    let mut output = String::new();
    while start.elapsed() < timeout {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => {
                output.push_str(&String::from_utf8_lossy(&buffer[..n]));
                if output.contains(needle) {
                    break;
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::Interrupted
                ) =>
            {
                continue;
            }
            Err(error) => panic!("read event: {error}"),
        }
    }
    output
}

#[test]
fn review_rejects_incompatible_options() {
    for flag in [
        "--serve",
        "--daemon",
        "--annotations",
        "--include-resolved",
        "--html",
        "--open",
        "--annotate",
    ] {
        Command::cargo_bin("markview")
            .unwrap()
            .args(["--review", flag, "sample.md"])
            .assert()
            .failure()
            .stdout("")
            .stderr(format!(
                "markview: --review cannot be combined with {flag}\n"
            ));
    }
    for flag in ["--gate", "--no-open"] {
        Command::cargo_bin("markview")
            .unwrap()
            .args(["--serve", flag, "sample.md"])
            .assert()
            .failure()
            .stdout("")
            .stderr(format!("markview: {flag} requires --review\n"));
    }
}

#[test]
fn review_serves_a_verdict_and_saved_comments_with_optional_gate() {
    use markview::annotations::source_revision;

    let dir = tempfile::tempdir().unwrap();
    let source = "# Reviewed\n";
    std::fs::write(dir.path().join("sample.md"), source).unwrap();
    for (verdict, gate, expected) in [
        ("approved", false, 0),
        ("changes", false, 0),
        ("changes", true, 2),
    ] {
        if verdict == "changes" {
            let store = dir.path().join(".markview/annotations");
            std::fs::create_dir_all(&store).unwrap();
            std::fs::write(
                store.join("sample.md"),
                "## a1 [open] document\n\nPlease revise\n",
            )
            .unwrap();
        }
        let requested = gate.then(unused_port);
        let port_text = requested.unwrap_or(0).to_string();
        let options = if gate {
            &["--gate", "--port", &port_text][..]
        } else {
            &[]
        };
        let mut server = start_review(dir.path(), options);
        let port = server.port;
        assert!(requested.is_none_or(|requested| requested == port));
        let token = &review_token(&http_get_local(port, "/"));
        let host = format!("localhost:{port}");
        let summary = raw_http(
            port,
            &format!("GET /_markview/review HTTP/1.1\r\nHost: {host}\r\n\r\n"),
        );
        assert!(summary.contains("Cache-Control: no-store"));
        let revision = summary
            .split("\"review_revision\":\"")
            .nth(1)
            .unwrap()
            .split('"')
            .next()
            .unwrap();
        let form = format!("op=finish&verdict={verdict}&review_revision={revision}&doc=/sample.md&source_revision={}", source_revision(source));
        let post = |host: &str, token: &str, body: &str| {
            raw_http(port, &format!(
            "POST /_markview/review HTTP/1.1\r\nHost: {host}\r\nX-Markview-Token: {token}\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n{body}", body.len()))
        };
        assert!(raw_http(
            port,
            "GET /_markview/review HTTP/1.1\r\nHost: example.com\r\n\r\n"
        )
        .starts_with("HTTP/1.1 403"));
        assert!(raw_http(
            port,
            &format!("HEAD /_markview/review HTTP/1.1\r\nHost: {host}\r\n\r\n")
        )
        .ends_with("\r\n\r\n"));
        assert!(post("example.com", token, &form).starts_with("HTTP/1.1 403"));
        assert!(post(&host, "wrong", &form).starts_with("HTTP/1.1 403"));
        assert!(post(&host, token, "op=finish&op=finish").starts_with("HTTP/1.1 400"));
        let oversized = format!("POST /_markview/review HTTP/1.1\r\nHost: {host}\r\nX-Markview-Token: {token}\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n", markview::annotations::FORM_CAP + 1);
        assert!(raw_http(port, &oversized).starts_with("HTTP/1.1 400"));
        drop(open_reload_stream(port));
        assert!(
            server.child.try_wait().unwrap().is_none(),
            "Tab disconnect ended the review"
        );
        let mut events = open_reload_stream(port);
        let _stalled = open_reload_stream(port);
        assert!(post(&host, token, &form).starts_with("HTTP/1.1 200"));
        let mut notification = String::new();
        events.read_to_string(&mut notification).unwrap();
        assert!(
            notification.contains("event: review-finished"),
            "{notification}"
        );
        let until = Instant::now() + Duration::from_secs(5);
        let status = loop {
            if let Some(status) = server.child.try_wait().unwrap() {
                break status;
            }
            assert!(Instant::now() < until, "review server did not exit");
            std::thread::sleep(Duration::from_millis(10));
        };
        let mut stdout = String::new();
        server
            .child
            .stdout
            .take()
            .unwrap()
            .read_to_string(&mut stdout)
            .unwrap();
        assert_eq!(status.code(), Some(expected));
        if verdict == "approved" {
            assert_eq!(stdout, "Review: approved (0 open comments in 0 documents)\n\nNo open annotations in the reviewed collection.\n");
        } else {
            assert_eq!(stdout, concat!("Review: changes requested (1 open comment in 1 document)\n\n",
                "# Annotations: sample.md\n",
                "Store: .markview/annotations/sample.md (change `[open]` to `[resolved]` there once addressed)\n\n",
                "## a1 · document\n\nPlease revise\n\n"));
        }
    }
}

fn start_review(cwd: &std::path::Path, options: &[&str]) -> ServeProcess {
    start_review_of(cwd, "sample.md", options)
}

#[test]
fn agent_api_lists_and_fetches_only_served_documents() {
    let dir = tempfile::tempdir().unwrap();
    let doc = dir.path().join("a + b%.md");
    let other = dir.path().join("other.md");
    std::fs::write(&doc, "# Example\r\n\r\n**bold**\r\n").unwrap();
    std::fs::write(&other, "# Excluded\n").unwrap();
    let server = ServeProcess::start(&doc);
    let port = server.port;
    let api = "/_markview/api/v1";
    let catalog = http_get_local(port, &format!("{api}/documents"));
    assert!(
        catalog.contains(r#""default_document":"/a%20%2B%20b%25.md""#),
        "{catalog}"
    );
    assert!(
        catalog.contains(r#""can_annotate":false,"can_finish":false"#),
        "{catalog}"
    );
    assert!(!catalog.contains("other.md"), "{catalog}");
    assert!(
        !catalog.contains(&*dir.path().to_string_lossy()),
        "{catalog}"
    );

    let path = format!("{api}/document?doc=%2Fa%2520%252B%2520b%2525.md");
    let response = http_get_local(port, &path);
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.contains("Cache-Control: no-cache"), "{response}");
    assert!(response.contains("ETag: \""), "{response}");
    assert!(response
        .contains(r##""markdown":"# Example\u000d\u000a\u000d\u000a**bold**\u000d\u000a""##));
    assert!(response.contains(r#""projected_text":"Example bold""#));
    assert!(raw_http(
        port,
        &format!("HEAD {path} HTTP/1.1\r\nHost: localhost:{port}\r\n\r\n")
    )
    .ends_with("\r\n\r\n"));
    assert!(raw_http(
        port,
        &format!("GET {path} HTTP/1.1\r\nHost: example.com:{port}\r\n\r\n")
    )
    .starts_with("HTTP/1.1 403"));
    for route in [
        "%2Fother.md",
        "%2F..%2Fother.md",
        "%2F.markview%2Fannotations%2Fa.md",
    ] {
        let response = http_get_local(port, &format!("{api}/document?doc={route}"));
        assert!(response.starts_with("HTTP/1.1 404"), "{response}");
    }
    assert!(http_get_local(port, &format!("{api}/review")).starts_with("HTTP/1.1 404"));
    assert!(raw_http(
        port,
        &format!("POST {api}/annotations HTTP/1.1\r\nHost: localhost:{port}\r\n\r\n")
    )
    .contains("API writes are disabled"));

    #[cfg(unix)]
    {
        std::fs::remove_file(&doc).unwrap();
        std::os::unix::fs::symlink(&other, &doc).unwrap();
        assert!(http_get_local(port, &path).starts_with("HTTP/1.1 404"));
    }
}

#[test]
fn agent_api_saves_revision_bound_comments_and_finishes_a_review() {
    let dir = tempfile::tempdir().unwrap();
    let source = "# Doc\n\n**😀 anchor**\n";
    std::fs::write(dir.path().join("sample.md"), source).unwrap();
    let token = "a1".repeat(32);
    let child = std::process::Command::new(cargo_bin("markview"))
        .current_dir(dir.path())
        .args(["--review", "--no-open", "--gate", "sample.md"])
        .env("MARKVIEW_API_TOKEN", &token)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut server = waiting_review(child).unwrap();
    let port = server.port;
    let api = "/_markview/api/v1";
    let catalog = http_get_local(port, &format!("{api}/documents"));
    assert!(catalog.contains(r#""can_annotate":true,"can_finish":true"#));
    let page = http_get_local(port, "/");
    assert!(!page.contains(&token));
    let post = |endpoint: &str, credential: &str, body: &str| {
        raw_http(port, &format!(
            "POST {api}/{endpoint} HTTP/1.1\r\nHost: localhost:{port}\r\n{credential}\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        ))
    };
    let credential = format!("Authorization: Bearer {token}");
    let summary_revision = || {
        let response = http_get_local(port, &format!("{api}/review"));
        response
            .split("\"review_revision\":\"")
            .nth(1)
            .unwrap()
            .split('"')
            .next()
            .unwrap()
            .to_owned()
    };
    let initial = summary_revision();
    let bare = "op=add&doc=%2Fsample.md&body=Explain+the+anchor";
    let source_revision = markview::annotations::source_revision(source);
    let comment = format!(
        "{bare}&source_revision={source_revision}&quote=%F0%9F%98%80+anchor&start=3&end=10"
    );
    let browser_credential = format!("X-Markview-Token: {}", review_token(&page));
    assert!(post("annotations", &browser_credential, &comment).starts_with("HTTP/1.1 403"));
    assert!(post("annotations", &credential, bare).starts_with("HTTP/1.1 400"));
    let stale = format!("{bare}&source_revision=old");
    assert!(post("annotations", &credential, &stale).starts_with("HTTP/1.1 409"));
    assert!(!dir.path().join(".markview").exists());
    let saved = post("annotations", &credential, &comment);
    assert!(saved.starts_with("HTTP/1.1 200"), "{saved}");
    assert!(
        saved.contains(r#""quote":"😀 anchor","lines":[3,3]"#),
        "{saved}"
    );
    let whole_document =
        format!("op=add&doc=%2Fsample.md&body=Overall&source_revision={source_revision}");
    assert!(post("annotations", &credential, &whole_document).starts_with("HTTP/1.1 200"));
    let annotations = http_get_local(port, &format!("{api}/annotations?doc=%2Fsample.md"));
    assert!(
        annotations.contains("Explain the anchor") && annotations.contains("Overall"),
        "{annotations}"
    );
    let finish = |revision: &str| {
        let form = format!("op=finish&verdict=changes&doc=%2Fsample.md&source_revision={source_revision}&review_revision={revision}");
        post("review", &credential, &form)
    };
    assert!(finish(&initial).starts_with("HTTP/1.1 409"));
    assert!(finish(&summary_revision()).starts_with("HTTP/1.1 200"));
    let until = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = server.child.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < until, "review server did not exit");
        std::thread::sleep(Duration::from_millis(10));
    };
    let mut output = String::new();
    server
        .child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut output)
        .unwrap();
    assert_eq!(status.code(), Some(2));
    assert!(
        output.contains("Review: changes requested (2 open comments in 1 document)"),
        "{output}"
    );
}

fn start_review_of(cwd: &std::path::Path, input: &str, options: &[&str]) -> ServeProcess {
    let child = std::process::Command::new(cargo_bin("markview"))
        .env_remove("MARKVIEW_API_TOKEN")
        .current_dir(cwd)
        .args(["--review", "--no-open", input])
        .args(options)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    waiting_review(child).unwrap_or_else(|notice| panic!("review did not start: {notice:?}"))
}

/// Reads the "Waiting for review" notice of a review that was just spawned,
/// then keeps collecting its stderr: the server reports problems there for
/// as long as it runs, and a closed pipe would make those reports fail.
/// `Err` is the first line of stderr when it is not the notice.
fn waiting_review(mut child: Child) -> Result<ServeProcess, String> {
    let mut stderr_pipe = BufReader::new(child.stderr.take().unwrap());
    let mut notice = String::new();
    stderr_pipe.read_line(&mut notice).unwrap();
    let Some(port) = notice
        .trim()
        .strip_prefix("Waiting for review at http://localhost:")
        .and_then(|url| url.strip_suffix('/'))
        .and_then(|port| port.parse::<u16>().ok())
        .filter(|port| *port != 0)
    else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(notice);
    };
    let stderr = Arc::new(Mutex::new(String::new()));
    let stderr_writer = stderr.clone();
    std::thread::spawn(move || {
        let mut line = String::new();
        while stderr_pipe.read_line(&mut line).unwrap_or(0) > 0 {
            stderr_writer.lock().unwrap().push_str(&line);
            line.clear();
        }
    });
    Ok(ServeProcess {
        child,
        port,
        stderr,
    })
}

/// Starts `--review` with at most `limit` open file descriptors, keeping its
/// stderr readable for the whole run. `spare` leaves one more descriptor open
/// in the server. `None` (with the reason printed) where the shell cannot
/// lower the limit.
#[cfg(unix)]
fn start_review_with_fd_limit(
    cwd: &std::path::Path,
    limit: u32,
    spare: bool,
) -> Option<ServeProcess> {
    let spare = if spare { "exec 9</dev/null; " } else { "" };
    let child = std::process::Command::new("sh")
        .env_remove("MARKVIEW_API_TOKEN")
        .current_dir(cwd)
        .arg("-c")
        .arg(format!(
            "ulimit -n {limit} || exit 97; {spare}exec \"$0\" --review --no-open sample.md"
        ))
        .arg(cargo_bin("markview"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    match waiting_review(child) {
        Ok(server) => Some(server),
        Err(notice) => {
            assert!(
                notice.contains("ulimit") || notice.is_empty(),
                "review did not start: {notice:?}"
            );
            eprintln!("skipped: this shell cannot lower RLIMIT_NOFILE to {limit}: {notice:?}");
            None
        }
    }
}

/// Runs a review out of descriptors, then finishes it. Returns whether
/// `accept` itself failed, or `None` if the limit could not be lowered.
#[cfg(unix)]
fn finish_review_after_fd_exhaustion(spare: bool) -> Option<bool> {
    let dir = tempfile::tempdir().unwrap();
    let source = "# Sample\n";
    std::fs::write(dir.path().join("sample.md"), source).unwrap();
    let mut server = start_review_with_fd_limit(dir.path(), 40, spare)?;
    let port = server.port;
    let alive = |server: &mut ServeProcess| {
        assert!(
            server.child.try_wait().unwrap().is_none(),
            "the review ended without a verdict: {}",
            server.stderr_so_far()
        );
    };
    let token = review_token(&http_get_local(port, "/"));
    // Idle tabs and stalled clients. Each accepted connection holds two
    // descriptors, so the server runs out well before the sixtieth.
    let idle: Vec<_> = (0..60)
        .map(|_| TcpStream::connect(("127.0.0.1", port)).unwrap())
        .collect();
    let until = Instant::now() + Duration::from_secs(10);
    while !server.stderr_so_far().contains("Too many open files") {
        alive(&mut server);
        assert!(
            Instant::now() < until,
            "60 idle connections did not exhaust 40 descriptors"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    // With an even number of descriptors to spare, the held connections use
    // the last one and the next `accept` fails; give that failure time to be
    // reported. With an odd number it may never happen.
    let until = Instant::now() + Duration::from_secs(2);
    let accept_failed = loop {
        alive(&mut server);
        if server.stderr_so_far().contains("connection failed") {
            break true;
        }
        if Instant::now() >= until {
            break false;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    drop(idle);

    // The session is still there once descriptors are free again.
    let host = format!("localhost:{port}");
    let until = Instant::now() + Duration::from_secs(10);
    let revision = loop {
        let summary = TcpStream::connect(("127.0.0.1", port))
            .and_then(|mut stream| {
                stream.set_read_timeout(Some(Duration::from_secs(8)))?;
                write!(
                    stream,
                    "GET /_markview/review HTTP/1.1\r\nHost: {host}\r\n\r\n"
                )?;
                let mut response = String::new();
                stream.read_to_string(&mut response)?;
                Ok(response)
            })
            .unwrap_or_default();
        if let Some(revision) = summary
            .split("\"review_revision\":\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
        {
            break revision.to_owned();
        }
        alive(&mut server);
        assert!(
            Instant::now() < until,
            "no summary after recovery: {summary}"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    let form = format!(
        "op=finish&verdict=approved&review_revision={revision}&doc=/sample.md&source_revision={}",
        markview::annotations::source_revision(source)
    );
    let finished = raw_http(port, &format!(
        "POST /_markview/review HTTP/1.1\r\nHost: {host}\r\nX-Markview-Token: {token}\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n{form}", form.len()));
    assert!(finished.starts_with("HTTP/1.1 200"), "{finished}");
    let until = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = server.child.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < until, "review server did not exit");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(status.code(), Some(0), "{}", server.stderr_so_far());
    let mut stdout = String::new();
    server
        .child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut stdout)
        .unwrap();
    assert!(stdout.starts_with("Review: approved (0 open comments in 0 documents)"));
    Some(accept_failed)
}

#[cfg(unix)]
#[test]
fn review_survives_running_out_of_file_descriptors() {
    // Whether `accept` or a later `dup` is the call that fails depends on
    // whether the descriptors left after startup are even or odd in number.
    // One spare descriptor flips that, so one of the two runs always has a
    // failing `accept`. Both must still deliver the verdict.
    let Some(plain) = finish_review_after_fd_exhaustion(false) else {
        return;
    };
    let spare = finish_review_after_fd_exhaustion(true).expect("the limit was lowered before");
    assert!(plain || spare, "neither run made `accept` fail");
}

#[test]
fn review_errors_name_the_document_and_the_cause_but_no_path() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    std::fs::write(root.join("a.md"), "# A\n").unwrap();
    std::fs::write(root.join("b.md"), "# B\n").unwrap();
    let store = root.join(".markview/annotations");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(
        store.join("b.md"),
        "## a1 [open] document\n\nOne\n\n## a1 [open] document\n\nTwo\n",
    )
    .unwrap();
    let server = start_review_of(&root, ".", &[]);
    let port = server.port;
    let get = |target: &str| {
        let response = raw_http(
            port,
            &format!("GET {target} HTTP/1.1\r\nHost: localhost:{port}\r\n\r\n"),
        );
        let (head, body) = response.split_once("\r\n\r\n").unwrap();
        // Nothing the page can show says where the collection lives.
        assert!(!response.contains(root.to_str().unwrap()), "{response}");
        assert!(!response.contains(".markview"), "{response}");
        (head.lines().next().unwrap().to_owned(), body.to_owned())
    };
    let duplicate =
        r#"{"error":"b.md: annotation a1 appears more than once; give each section a unique ID"}"#;
    // The bar cannot offer a verdict, and says which document is why.
    assert_eq!(
        get("/_markview/review"),
        (
            "HTTP/1.1 500 Internal Server Error".to_owned(),
            duplicate.to_owned()
        )
    );
    // That document's own page says the same.
    assert_eq!(
        get("/_markview/annotations?doc=/b.md"),
        ("HTTP/1.1 409 Conflict".to_owned(), duplicate.to_owned())
    );
    // Where exactly, for whoever runs the command.
    let store_path = store.join("b.md").display().to_string();
    let until = Instant::now() + Duration::from_secs(5);
    while !server.stderr_so_far().contains(&store_path) {
        assert!(Instant::now() < until, "{}", server.stderr_so_far());
        std::thread::sleep(Duration::from_millis(10));
    }

    std::fs::remove_file(store.join("b.md")).unwrap();
    std::fs::write(root.join("a.md"), [0xff, 0xfe]).unwrap();
    assert_eq!(
        get("/_markview/review"),
        (
            "HTTP/1.1 500 Internal Server Error".to_owned(),
            r#"{"error":"a.md: the document is not valid UTF-8"}"#.to_owned()
        )
    );
}

#[test]
fn review_endpoint_is_disabled_in_ordinary_serve() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("sample.md");
    std::fs::write(&file, "# Sample").unwrap();
    let server = ServeProcess::start(&file);
    for (method, status) in [("GET", 404), ("HEAD", 404), ("POST", 405)] {
        let response = raw_http(
            server.port,
            &format!(
                "{method} /_markview/review HTTP/1.1\r\nHost: localhost:{}\r\n\r\n",
                server.port
            ),
        );
        assert!(
            response.starts_with(&format!("HTTP/1.1 {status}")),
            "{response}"
        );
        assert!(response.contains("Cache-Control: no-store"));
    }
}

#[test]
fn review_finish_rejects_missing_token_and_times_out_an_incomplete_form() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("sample.md"), "# Sample").unwrap();
    let mut server = start_review(dir.path(), &[]);
    let port = server.port;
    let headers = format!("POST /_markview/review HTTP/1.1\r\nHost: localhost:{port}\r\nContent-Type: application/x-www-form-urlencoded\r\n");
    assert!(raw_http(
        port,
        &format!("{headers}Content-Length: 9\r\n\r\nop=finish")
    )
    .starts_with("HTTP/1.1 403"));

    let token = review_token(&http_get_local(port, "/"));
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(6)))
        .unwrap();
    write!(
        stream,
        "{headers}X-Markview-Token: {token}\r\nContent-Length: 20\r\n\r\nop=finish"
    )
    .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 400"), "{response}");
    assert!(response.contains("incomplete annotation request"));
    assert!(server.child.try_wait().unwrap().is_none());
}

#[cfg(unix)]
#[test]
fn review_signals_cancel_without_losing_comments() {
    use std::os::unix::process::ExitStatusExt;

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("sample.md"), "# Sample").unwrap();
    let annotation_dir = dir.path().join(".markview/annotations");
    std::fs::create_dir_all(&annotation_dir).unwrap();
    let annotation_path = annotation_dir.join("sample.md");
    let comment = "## a1 [open] document\n\nSaved comment\n";
    std::fs::write(&annotation_path, comment).unwrap();

    for (signal, number) in [("INT", 2), ("TERM", 15)] {
        let mut server = start_review(dir.path(), &[]);
        assert!(std::process::Command::new("kill")
            .args(["-s", signal, &server.child.id().to_string()])
            .status()
            .unwrap()
            .success());
        let deadline = Instant::now() + Duration::from_secs(5);
        let status = loop {
            if let Some(status) = server.child.try_wait().unwrap() {
                break status;
            }
            assert!(Instant::now() < deadline, "review ignored SIG{signal}");
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(status.signal(), Some(number));
        let mut stdout = String::new();
        server
            .child
            .stdout
            .take()
            .unwrap()
            .read_to_string(&mut stdout)
            .unwrap();
        assert!(stdout.is_empty());
        assert_eq!(std::fs::read_to_string(&annotation_path).unwrap(), comment);
    }
}

/// Starts `--serve --annotate` on `dir` with `MARKVIEW_API_TOKEN` set.
fn start_api_server(dir: &std::path::Path, token: &str, recurse: bool) -> ServeProcess {
    let mut cmd = std::process::Command::new(cargo_bin("markview"));
    cmd.args(["--serve", "--annotate", "--port", "0"])
        .arg(dir)
        .env("MARKVIEW_API_TOKEN", token);
    if recurse {
        cmd.arg("--recurse");
    }
    ServeProcess::launch(cmd).unwrap_or_else(|message| panic!("{message}"))
}

/// `GET` that gives up (`None`) when no complete response arrives within `limit`.
fn get_within(port: u16, route: &str, limit: Duration) -> Option<String> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
    stream.set_read_timeout(Some(limit)).ok()?;
    write!(
        stream,
        "GET {route} HTTP/1.1\r\nHost: localhost:{port}\r\nConnection: close\r\n\r\n"
    )
    .ok()?;
    let mut response = String::new();
    stream.read_to_string(&mut response).ok()?;
    Some(response)
}

#[cfg(unix)]
#[test]
fn api_write_waiting_on_a_store_lock_does_not_stall_a_rescan_or_other_requests() {
    use std::os::unix::io::AsRawFd;
    let dir = tempfile::tempdir().unwrap();
    let source = "# Home\n";
    std::fs::write(dir.path().join("README.md"), source).unwrap();
    let token = "b2".repeat(32);
    let server = start_api_server(dir.path(), &token, true);
    let port = server.port;

    // Hold the document's store lock so the next API write blocks on it.
    let store = dir.path().join(".markview/annotations");
    std::fs::create_dir_all(&store).unwrap();
    let lock = std::fs::File::create(store.join("README.md.markview-lock")).unwrap();
    assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) }, 0);

    let form = format!(
        "op=add&doc=%2FREADME.md&body=Queued&source_revision={}",
        markview::annotations::source_revision(source)
    );
    let mut pending = TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(
        pending,
        "POST /_markview/api/v1/annotations HTTP/1.1\r\nHost: localhost:{port}\r\nAuthorization: Bearer {token}\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{form}",
        form.len()
    )
    .unwrap();
    // Let the write reach the lock before the rescan is triggered.
    std::thread::sleep(Duration::from_millis(300));

    std::fs::write(dir.path().join("added.md"), "# Added\n").unwrap();
    let until = Instant::now() + Duration::from_secs(20);
    loop {
        let catalog = get_within(port, "/_markview/api/v1/documents", Duration::from_secs(5))
            .expect("the catalog is answered while an API write waits for a store lock");
        assert!(catalog.starts_with("HTTP/1.1 200"), "{catalog}");
        if catalog.contains("/added.md") {
            break;
        }
        assert!(Instant::now() < until, "rescan never published: {catalog}");
        std::thread::sleep(Duration::from_millis(20));
    }

    assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_UN) }, 0);
    pending
        .set_read_timeout(Some(Duration::from_secs(8)))
        .unwrap();
    let mut response = String::new();
    pending.read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(std::fs::read_to_string(store.join("README.md"))
        .unwrap()
        .contains("Queued"));
}

fn serve_annotate_command(dir: &std::path::Path, args: &[&str]) -> std::process::Command {
    let mut cmd = std::process::Command::new(cargo_bin("markview"));
    cmd.args(args).args(["--port", "0"]).arg(dir);
    cmd
}

/// Polls the server's stderr until it contains `needle`.
fn stderr_within(server: &ServeProcess, needle: &str, limit: Duration) -> bool {
    let until = Instant::now() + limit;
    while Instant::now() < until {
        if server.stderr_so_far().contains(needle) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    server.stderr_so_far().contains(needle)
}

#[test]
fn an_empty_api_token_variable_counts_as_unset() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.md"), "# A\n").unwrap();
    let mut cmd = serve_annotate_command(dir.path(), &["--serve", "--annotate"]);
    cmd.env("MARKVIEW_API_TOKEN", "");
    let server = ServeProcess::launch(cmd).unwrap();
    let catalog = http_get_local(server.port, "/_markview/api/v1/documents");
    assert!(catalog.contains(r#""can_annotate":false"#), "{catalog}");
    assert!(!server.stderr_so_far().contains("MARKVIEW_API_TOKEN"));
}

#[test]
fn a_malformed_api_token_stops_annotate_and_review_with_a_clear_message() {
    use std::os::unix::ffi::OsStringExt;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.md"), "# A\n").unwrap();
    let bad: Vec<std::ffi::OsString> = vec![
        "A1".repeat(32).into(),
        format!("{}a", "a1".repeat(31)).into(),
        "zz".repeat(32).into(),
        "a1".repeat(33).into(),
        std::ffi::OsString::from_vec(vec![0xff; 64]),
    ];
    for value in &bad {
        for mode in [["--serve", "--annotate"], ["--review", "--no-open"]] {
            let output = serve_annotate_command(dir.path(), &mode)
                .env("MARKVIEW_API_TOKEN", value)
                .output()
                .unwrap();
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert_eq!(
                output.status.code(),
                Some(1),
                "{value:?} {mode:?}: {stderr}"
            );
            assert!(
                stderr.contains("MARKVIEW_API_TOKEN must contain 64 lowercase hex digits"),
                "{stderr}"
            );
        }
    }
}

#[test]
fn startup_says_whether_api_writes_are_enabled_or_the_token_is_ignored() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.md"), "# A\n").unwrap();
    let token = "a1".repeat(32);

    let mut cmd = serve_annotate_command(dir.path(), &["--serve", "--annotate"]);
    cmd.env("MARKVIEW_API_TOKEN", &token);
    let server = ServeProcess::launch(cmd).unwrap();
    assert!(stderr_within(
        &server,
        "API writes enabled",
        Duration::from_secs(5)
    ));
    assert!(!server.stderr_so_far().contains(&token));

    // Plain --serve ignores the variable, even a malformed one.
    let mut cmd = serve_annotate_command(dir.path(), &["--serve"]);
    cmd.env("MARKVIEW_API_TOKEN", "bad");
    let server = ServeProcess::launch(cmd).unwrap();
    assert!(stderr_within(
        &server,
        "MARKVIEW_API_TOKEN is set but ignored",
        Duration::from_secs(5)
    ));
}

#[test]
fn disabled_api_writes_say_how_to_enable_them() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.md"), "# A\n").unwrap();
    let server = ServeProcess::start(&dir.path().join("a.md"));
    let port = server.port;
    let response = raw_http(
        port,
        &format!("POST /_markview/api/v1/annotations HTTP/1.1\r\nHost: localhost:{port}\r\n\r\n"),
    );
    assert!(response.starts_with("HTTP/1.1 403"), "{response}");
    assert!(
        response.contains(
            "API writes are disabled: set MARKVIEW_API_TOKEN and use --annotate or --review"
        ),
        "{response}"
    );
}
