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
        "`--html`",
        "`--serve`",
        "terminal renderer",
        "git",
        "hunk",
    ] {
        assert!(diff.contains(term), "missing diff documentation: {term}");
    }
    assert!(!diff.contains("`dff`"));
    assert_eq!(readme.matches("### Diff blocks\n").count(), 1);
}

#[test]
fn review_comments_are_documented_for_people_and_agents() {
    let readme = fs::read_to_string("README.md").expect("read README");
    let review = readme
        .split("### Review comments\n")
        .nth(1)
        .expect("Review comments section")
        .split("\n### ")
        .next()
        .unwrap();
    // Every review flag has a row in the options table.
    for option in [
        "| `--annotate` |",
        "| `--annotations` |",
        "| `--include-resolved` |",
    ] {
        assert_eq!(review.matches(option).count(), 1, "{option}");
    }
    // Saving writes the comment; a draft is what exists before that.
    assert!(review.contains("to save the comment"));
    assert!(!readme.contains("save a draft"));
    assert!(review.contains("`## a<N> [open|resolved] <anchor>`"));
    assert!(review.contains("`/_markview/` is a reserved route prefix"));
    // Committing or ignoring the store is the user's choice; both are shown.
    assert!(review.contains("Commit\n`.markview/` to share") && review.contains("`.gitignore`"));
    // Hidden directories can hold assets, but never the annotation store.
    assert!(
        readme.contains("except anything under a `.markview` directory: the whole tree is blocked")
    );

    let skill = fs::read_to_string("skills/markview/SKILL.md").expect("read skill");
    assert!(skill.contains("Header lines must stay exactly `## a<N> [open|resolved] <anchor>`"));
    assert!(skill.contains("nothing under a `.markview` directory is ever served"));
    for mistake in [
        "`[Resolved]`",
        "`[OPEN]`",
        "trailing space",
        "second\nspace",
    ] {
        assert!(skill.contains(mistake), "{mistake}");
    }
}
