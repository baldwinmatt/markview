use std::fs;

#[test]
fn ci_runs_core_and_macos_gui_checks() {
    let workflow = fs::read_to_string(".github/workflows/ci.yml").expect("read CI workflow");

    assert!(workflow.contains("cargo fmt --check"));
    assert!(workflow.contains("cargo clippy --all-targets -- -D warnings"));
    assert!(workflow.contains("cargo test"));
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
