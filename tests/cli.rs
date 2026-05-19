use assert_cmd::cargo::cargo_bin;
use assert_cmd::Command;
use predicates::prelude::*;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Stdio};
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
    std::fs::write(&file, "# Served\n\nRemote **view**.\n").expect("write sample");
    let mut server = ServeProcess::start(&file);

    let response = http_get(server.port, "/");

    assert!(response.contains("HTTP/1.1 200 OK"));
    assert!(response.contains(r#"<h1 id="served">Served</h1>"#));
    assert!(response.contains("<strong>view</strong>"));
    assert!(response.contains("new EventSource('/events')"));
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
    cmd.args(["--serve", &port.to_string()])
        .arg(&file)
        .assert()
        .failure()
        .stderr(predicate::str::contains(format!(
            "port {port} is already in use"
        )));
}

struct ServeProcess {
    child: Child,
    port: u16,
}

impl ServeProcess {
    fn start(file: &std::path::Path) -> Self {
        let mut cmd = std::process::Command::new(cargo_bin("markview"));
        cmd.args(["--serve", "0"]).arg(file).stdout(Stdio::piped());
        let mut child = cmd.spawn().expect("spawn server");
        let stdout = child.stdout.take().expect("stdout");
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        reader.read_line(&mut line).expect("startup line");
        let port = parse_served_port(&line);
        Self { child, port }
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
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    write!(
        stream,
        "GET {route} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
    )
    .expect("write request");
    let mut response = String::new();
    stream.read_to_string(&mut response).expect("read response");
    response
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
