use assert_cmd::Command;
use predicates::prelude::*;

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
