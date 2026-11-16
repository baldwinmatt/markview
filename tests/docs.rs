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

#[test]
fn review_workflow_is_documented_for_reviewers_and_agents() {
    let readme = fs::read_to_string("README.md").expect("read README");
    // The single-key shortcuts, their toggle and the quick labels.
    for row in [
        "| `j` / `k` | Focus the next or previous visible annotation, wrapping at the ends. |",
        "| `c` | Comment on the selection or, with no selection, on the document. |",
        "| `e` | Edit the focused comment. |",
        "| `r` | Resolve or reopen the focused comment. |",
        "| `Escape` | Cancel the composer or clear the focused annotation. |",
    ] {
        assert_eq!(readme.matches(row).count(), 1, "{row}");
    }
    assert!(readme.contains("**Single-key shortcuts** toggle"));
    for label in [
        "**Question**",
        "**Needs a test**",
        "**Out of scope**",
        "**Nit**",
    ] {
        assert!(readme.contains(label), "{label}");
    }
    for flag in ["`--review`", "`--gate`", "`--no-open`"] {
        assert!(readme.contains(flag), "{flag}");
    }

    // An agent is told how to wait for a verdict and where each part of the
    // result arrives.
    let skill = fs::read_to_string("skills/markview/SKILL.md").expect("read skill");
    let verdict = skill
        .split("## Request a human verdict\n")
        .nth(1)
        .expect("the skill explains --review")
        .split("\n## ")
        .next()
        .unwrap();
    for term in [
        "`markview --review [--recurse] <FILE>... | <DIR>`",
        "foreground process and keep it alive",
        "Read stderr\nfor the browser URL",
        "Capture stdout through completion",
        "`--gate`",
        "`--no-open --port <PORT>`",
        "recover saved comments with `markview --annotations`",
        "ended without a verdict",
    ] {
        assert!(verdict.contains(term), "{term}");
    }

    let architecture = fs::read_to_string("ARCHITECTURE.md").expect("read ARCHITECTURE.md");
    assert!(architecture.contains("--review"));
    let roadmap = fs::read_to_string("ROADMAP.md").expect("read ROADMAP.md");
    assert!(roadmap.contains("[Review workflow specification](REVIEW_WORKFLOW_SPEC.md)"));
}
