use std::fs;

#[test]
fn diff_blocks_are_documented_in_usage_with_current_aliases_and_frontends() {
    let readme = fs::read_to_string("README.md").expect("read README");
    let usage = readme
        .split("## Usage\n")
        .nth(1)
        .expect("Usage section")
        .split("\n## ")
        .next()
        .unwrap();
    let diff = usage
        .split("### Diff blocks\n")
        .nth(1)
        .expect("Diff blocks belong in Usage")
        .split("\n### ")
        .next()
        .unwrap();
    for term in [
        "case-insensitive",
        "`diff`",
        "`patch`",
        "`udiff`",
        "`dff`",
        "`--html`",
        "`--serve`",
        "terminal renderer",
        "git",
        "hunk",
    ] {
        assert!(diff.contains(term), "missing diff documentation: {term}");
    }
    assert_eq!(readme.matches("### Diff blocks\n").count(), 1);
}
