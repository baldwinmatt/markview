use std::fs;

#[test]
fn ci_runs_core_and_macos_gui_checks() {
    let workflow = fs::read_to_string(".github/workflows/ci.yml").expect("read CI workflow");

    assert!(workflow.contains("cargo fmt --check"));
    assert!(workflow.contains("cargo clippy --all-targets -- -D warnings"));
    assert!(workflow.contains("cargo test"));
    assert!(workflow.contains("cargo test --features highlight"));
    assert!(workflow.contains("actions/setup-node@v4"));
    assert!(workflow.contains("node --test tests/editor.test.cjs"));
    assert!(workflow.contains("node --test tests/review.test.cjs"));
    assert!(workflow.contains("runs-on: macos-latest"));
    assert!(workflow.contains("cargo clippy --features gui --all-targets -- -D warnings"));
    assert!(workflow.contains("cargo test --features gui"));
    assert!(workflow.contains("cargo build --features gui --bin markview-gui"));
}

#[test]
fn release_workflow_publishes_packaged_macos_zip() {
    let workflow =
        fs::read_to_string(".github/workflows/release.yml").expect("read release workflow");

    assert!(workflow.contains("tags:"));
    assert!(workflow.contains("\"v*\""));
    assert!(workflow.contains("runs-on: macos-latest"));
    assert!(workflow.contains("make package-macos"));
    assert!(workflow.contains("actions/upload-artifact@v4"));
    assert!(workflow.contains("softprops/action-gh-release@v2"));
    assert!(workflow.contains("target/dist/*.zip"));
    assert!(workflow.contains("body_path: RELEASE_NOTES.md"));
}

#[test]
fn review_script_tests_run_wherever_the_editor_tests_do() {
    // The review script is shared by the browser and the macOS app. Its
    // tests must run in `make test`, in CI and before a release.
    for file in ["Makefile", ".github/workflows/ci.yml", "ARCHITECTURE.md"] {
        let text = fs::read_to_string(file).expect("read file");
        assert_eq!(
            text.matches("node --test tests/editor.test.cjs").count(),
            text.matches("node --test tests/review.test.cjs").count(),
            "{file}"
        );
        assert!(text.contains("node --test tests/review.test.cjs"), "{file}");
    }
    assert!(fs::metadata("tests/review.test.cjs").is_ok());
}
