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
fn renders_html_stdin() {
    let mut cmd = Command::cargo_bin("markview").expect("binary");
    cmd.arg("--html")
        .write_stdin("# Piped\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("<title>Untitled Markdown</title>"))
        .stdout(predicate::str::contains(r#"<h1 id="piped">Piped</h1>"#));
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
    assert!(!response.contains(r#"<div class="markview-shell">"#));
    assert!(response
        .contains(r#"Served by <a href="https://github.com/baldwinmatt/markview">markview</a>"#));
    assert!(response.contains(r#"<footer class="markview-footer">"#));
    assert!(response.contains(r#"class="markview-refreshed""#));
    assert!(response.contains("Last refreshed <time data-markview-refreshed-at>"));
    assert!(response.contains("new EventSource('/events')"));
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
    assert!(before.contains("Last refreshed"));

    std::fs::write(&first, "# After\n\nContent\n").expect("update first title");
    let after = http_get(server.port, "/");
    assert!(after.contains("<title>After</title>"));
    assert!(after.contains(">After</a>"));
    assert!(after.contains("Served by"));
    assert!(after.contains("Last refreshed"));
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

    let mut cmd = Command::cargo_bin("markview").expect("binary");
    cmd.args(["--serve", "8080"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "replaced by `--serve FILE --port PORT`",
        ));
}

#[test]
fn serve_mode_rejects_invalid_port_values() {
    let mut cmd = Command::cargo_bin("markview").expect("binary");
    cmd.args(["--serve", "README.md", "--port", "nope"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid port: nope"));

    let mut cmd = Command::cargo_bin("markview").expect("binary");
    cmd.args(["--serve", "README.md", "--port", "0"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid port: 0"));
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
fn serve_mode_rejects_explicit_files_with_only_filesystem_root_common() {
    let dir = tempfile::tempdir_in("/private/tmp").expect("tmp dir");
    let tmp_file = dir.path().join("one.md");
    let workspace_file = std::env::current_dir()
        .expect("cwd")
        .join("target/root-common-test.md");
    std::fs::write(&tmp_file, "# Tmp\n").expect("write tmp file");
    std::fs::write(&workspace_file, "# Workspace\n").expect("write workspace file");

    let mut cmd = Command::cargo_bin("markview").expect("binary");
    cmd.args(["--serve"])
        .arg(&tmp_file)
        .arg(&workspace_file)
        .args(["--port", &unused_port().to_string()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("non-root ancestor"));

    let _ = std::fs::remove_file(workspace_file);
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
    assert!(!root.contains(r#"<div class="markview-shell">"#));
    assert!(root.contains(r#"href="/second.md#part""#));
    assert!(second_response.contains(r#"<h1 id="second">Second</h1>"#));
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
    assert!(readme.contains(r#"<div class="markview-shell">"#));
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

    fn start_with_args(inputs: &[&std::path::Path], port: u16) -> Self {
        Self::start_with_env(inputs, port, false)
    }

    fn start_with_disconnect_logging(file: &std::path::Path) -> Self {
        Self::start_with_env(&[file], unused_port(), true)
    }

    fn start_with_env(inputs: &[&std::path::Path], port: u16, log_disconnects: bool) -> Self {
        for attempt in 0..5 {
            let port = if attempt == 0 { port } else { unused_port() };
            match Self::try_start_with_env(inputs, port, log_disconnects) {
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
    ) -> Result<Self, String> {
        let mut cmd = std::process::Command::new(cargo_bin("markview"));
        cmd.arg("--serve");
        for input in inputs {
            cmd.arg(input);
        }
        cmd.args(["--port", &port.to_string()])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if log_disconnects {
            cmd.env("MARKVIEW_LOG_EVENT_DISCONNECTS", "1");
        }
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

fn unused_port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0))
        .expect("bind unused port")
        .local_addr()
        .expect("local addr")
        .port()
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
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                continue;
            }
            Err(error) => panic!("read event: {error}"),
        }
    }
    output
}
