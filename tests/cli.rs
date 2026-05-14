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
