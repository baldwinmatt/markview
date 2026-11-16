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

#[test]
fn plain_page_claims_match_what_a_plain_page_carries() {
    // A page without annotations is no longer the page served before
    // annotations existed: it has copy controls, alert styles and a merged
    // live-reload script. No document may still promise identical bytes.
    for file in [
        "ANNOTATIONS_SPEC.md",
        "ARCHITECTURE.md",
        "ANNOTATIONS_TICKETS.md",
        "README.md",
        "src/main.rs",
        "src/serve_annotations.rs",
    ] {
        let text = fs::read_to_string(file).expect("read file").to_lowercase();
        for (number, line) in text.lines().enumerate() {
            let identical = ["byte-identi", "byte identi", "byte for byte"]
                .iter()
                .any(|claim| line.contains(claim))
                || line.contains("exactly what") && line.contains("before annotations");
            let lines: Vec<_> = text
                .lines()
                .skip(number.saturating_sub(1))
                .take(3)
                .collect();
            assert!(
                !(identical && lines.iter().any(|near| near.contains("annotation"))),
                "{file}:{}: {line}",
                number + 1
            );
        }
    }
    // What they say instead, and where the copy assets' own version is noted.
    for file in ["ANNOTATIONS_SPEC.md", "ARCHITECTURE.md"] {
        let text = fs::read_to_string(file).expect("read file");
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        for claim in [
            "carries no review machinery",
            "copy controls, alert styles and live-reload shell",
            "`tests/fixtures/serve-baseline.expected.html`",
            "versioned independently",
        ] {
            assert!(text.contains(claim), "{file}: {claim}");
        }
    }
}

fn normalized(file: &str) -> String {
    fs::read_to_string(file)
        .unwrap_or_else(|error| panic!("read {file}: {error}"))
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn specs_describe_the_agent_api_and_its_credential() {
    let annotations = normalized("ANNOTATIONS_SPEC.md");
    // The statements the API made false are gone.
    for old in [
        "accepted on `/_markview/annotations` only",
        "per-process random token on every `POST`",
    ] {
        assert!(!annotations.contains(old), "outdated: {old}");
    }
    for claim in [
        "then `POST` is accepted on `/_markview/annotations` and, with `--annotate` or `--review` and a `MARKVIEW_API_TOKEN`, on `/_markview/api/v1/annotations` and, under `--review`, `/_markview/api/v1/review`",
        "Require a per-process random token on every browser `POST`",
        "Require `Authorization: Bearer <MARKVIEW_API_TOKEN>` on every API `POST`",
        "compared in constant time",
        "An API route that does not exist, or that this server does not offer, returns `404` for every method; a route that exists and gets another method returns `405`; a write while API writes are disabled returns `403`",
        "`GET /_markview/api/v1/documents`, `/document` and `/annotations` need no credential, only a loopback `Host`",
        "no CORS headers",
        "an `Authorization` header forces a preflight",
        "Both kinds of add carry one `source_revision`",
    ] {
        assert!(annotations.contains(claim), "ANNOTATIONS_SPEC.md: {claim}");
    }
    let review = normalized("REVIEW_WORKFLOW_SPEC.md");
    for claim in [
        "`/_markview/api/v1/review` follows the same rule",
        "`Authorization: Bearer <MARKVIEW_API_TOKEN>` in place of the page token",
        "`404` for every method outside `--review`",
    ] {
        assert!(review.contains(claim), "REVIEW_WORKFLOW_SPEC.md: {claim}");
    }
}

#[test]
fn architecture_skill_help_and_roadmap_cover_the_agent_api() {
    let architecture = normalized("ARCHITECTURE.md");
    for claim in [
        "## Agent HTTP API",
        "`src/serve_api.rs`",
        "second credential",
        "`MARKVIEW_API_TOKEN`",
        "request flow",
        "`ServeSession::api_access`",
    ] {
        assert!(architecture.contains(claim), "ARCHITECTURE.md: {claim}");
    }
    let skill = normalized("skills/markview/SKILL.md");
    for claim in [
        "### Use the HTTP API",
        "`/_markview/api/v1/documents`",
        "`MARKVIEW_API_TOKEN`",
        "`openssl rand -hex 32`",
        "`--annotate` or `--review`",
        "started without the token must be restarted",
    ] {
        assert!(skill.contains(claim), "SKILL.md: {claim}");
    }
    let help = markview::help();
    assert!(help.contains("MARKVIEW_API_TOKEN"), "{help}");
    assert!(help.contains("/_markview/api/v1"), "{help}");
    let roadmap = normalized("ROADMAP.md");
    assert!(roadmap.contains("### FEAT-006: Agent HTTP API"));
}

fn agent_api_section() -> String {
    let readme = fs::read_to_string("README.md").expect("read README");
    readme
        .split("### Agent HTTP API\n")
        .nth(1)
        .expect("Agent HTTP API section")
        .split("\n### ")
        .next()
        .unwrap()
        .to_owned()
}

#[test]
fn readme_api_examples_show_error_bodies() {
    let section = agent_api_section();
    let mut blocks = 0;
    for block in section.split("```sh\n").skip(1) {
        let block = block.split("```").next().unwrap();
        if !block.contains("curl") {
            continue;
        }
        blocks += 1;
        for word in block.split_whitespace() {
            let short_f = word.starts_with('-')
                && !word.starts_with("--")
                && word.chars().skip(1).all(|ch| ch.is_ascii_alphabetic())
                && word.contains('f');
            assert!(!short_f, "`{word}` hides the error body: {block}");
            assert!(word != "--fail", "`--fail` hides the error body: {block}");
        }
        assert!(block.contains("--fail-with-body"), "{block}");
    }
    assert!(blocks >= 2, "the section lost its curl examples");
}

#[test]
fn readme_states_the_api_contract_precisely() {
    let section = agent_api_section();
    let text = section.split_whitespace().collect::<Vec<_>>().join(" ");
    for claim in [
        r#"{"error":"<message>"}"#,
        "`400`",
        "`403`",
        "`404`",
        "`405`",
        "`409`",
        "`413`",
        "`500`",
        "10,000 characters",
        r#"`id` is a string such as `"a1"`"#,
        "`null` for a whole-document comment",
        r#"`{"finished":true}`"#,
        "`finished`",
        "`client_id`",
        "`ETag`",
        "`If-None-Match`",
        "`304`",
        "`can_annotate`",
        "`can_finish`",
        "a credential was configured",
        "Unicode `White_Space`",
        "`char::is_whitespace`",
        "not Python's `str.isspace` and not JavaScript's `\\s`",
        "Unicode scalar values of `projected_text`",
        "`markdown` and `projected_text` can differ",
        "quote from `projected_text`",
        "may still be applied",
        "`alpha\\u001fbeta gamma`",
        "`[10, 15)`",
        "`api_version`",
    ] {
        assert!(text.contains(claim), "README Agent HTTP API: {claim}");
    }
}

#[test]
fn the_credential_model_and_its_threat_model_are_stated() {
    let spec = normalized("ANNOTATIONS_SPEC.md");
    for claim in [
        "The threat model is local web attackers",
        "a hostile page in the user's browser, DNS rebinding and another site's script",
        "Other processes of the same user are out of scope",
        "A token with fewer than 8 distinct digits is refused at startup",
    ] {
        assert!(spec.contains(claim), "ANNOTATIONS_SPEC.md: {claim}");
    }
    let readme = agent_api_section()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    for claim in [
        "`mode`",
        "`serve`, `annotate` or `review`",
        "local web attackers",
        "Other processes of the same user are out of scope",
        "fewer than 8 distinct digits",
        "MARKVIEW_API_TOKEN=$(openssl rand -hex 32)",
    ] {
        assert!(readme.contains(claim), "README Agent HTTP API: {claim}");
    }
}
