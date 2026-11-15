use super::*;
use std::os::unix::fs::symlink;
use tempfile::tempdir;

fn add(body: &str) -> Operation {
    Operation::Add {
        body: body.to_owned(),
        selection: None,
    }
}

fn selection(source: &str, quote: &str, range: Range<usize>, body: &str) -> Operation {
    Operation::Add {
        body: body.to_owned(),
        selection: Some(Selection {
            quote: quote.to_owned(),
            range,
            revision: source_revision(source),
        }),
    }
}

fn change(id: u64, review: &Review, change: Change) -> Operation {
    Operation::Change {
        id,
        revision: review.store_revision.clone(),
        change,
    }
}

#[test]
fn codec_preserves_agent_content_and_escaped_header_lines() {
    let source = concat!(
        "My preamble\r\n## a9 [broken] document\r\n\r\n",
        "## a3 [resolved] lines 2-3\n",
        "<!-- markview: section=\"One &gt; Two\" extra=\"&amp;&#45;- &quot;&lt;x&gt;&quot;\" -->\n\n",
        "> first\n> second\n\n",
        "> body begins with a quote\n\n",
        "```md\n\\## a4 [open] document\n\\\\## a5 [open] line 4\n```\n",
        "## a10 [open] line nope\nAgent appended prose.  \n\n",
        "## a4 [open] document\n\n> also a body\n"
    );
    let store = Store::parse(source);
    assert_eq!(store.annotations.len(), 2);
    let record = &store.annotations[0];
    assert_eq!(record.status, Status::Resolved);
    assert_eq!(record.quote.as_deref(), Some("first second"));
    assert_eq!(record.metadata["extra"], "&-- \"<x>\"");
    assert!(record.body.contains("\n## a4 [open] document\n\\## a5"));
    assert!(record.body.ends_with("Agent appended prose.  "));
    assert_eq!(store.annotations[1].body, "> also a body");
    let encoded = store.encode();
    assert!(encoded.contains("&amp;-&#45; &quot;&lt;x&gt;"));
    assert!(encoded.starts_with(&store.preamble));
    assert_eq!(Store::parse(&encoded).annotations, store.annotations);
    assert_eq!(
        Store::parse("```\n## a8 [open] document\n```").annotations[0].id,
        8
    );
}

#[test]
fn path_precedence_and_symlinks_are_checked_without_creating_files() {
    let dir = tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let nested = root.join("deep/docs");
    fs::create_dir_all(&nested).unwrap();
    let doc = nested.join("spec.md");
    fs::write(&doc, "text").unwrap();
    assert_eq!(
        resolve(&doc).unwrap().path,
        nested.join(".markview/annotations/spec.md")
    );

    fs::write(root.join(".git"), "gitdir: somewhere").unwrap();
    let base = root.join(".markview/annotations");
    let location = resolve(&doc).unwrap();
    assert_eq!(location.path, base.join("deep/docs/spec.md"));
    assert!(snapshot(&location).unwrap().is_none());
    assert!(!base.exists());

    let nearer = root.join("deep/.markview/annotations");
    fs::create_dir_all(&nearer).unwrap();
    assert_eq!(resolve(&doc).unwrap().path, nearer.join("docs/spec.md"));

    let outside = root.join("outside");
    fs::create_dir(&outside).unwrap();
    symlink(&outside, nested.join(".markview")).unwrap();
    assert!(resolve(&doc).is_err());
    fs::remove_file(nested.join(".markview")).unwrap();
    fs::create_dir(nested.join(".markview")).unwrap();
    symlink(&outside, nested.join(".markview/annotations")).unwrap();
    assert!(resolve(&doc).is_err());
    fs::remove_file(nested.join(".markview/annotations")).unwrap();
    symlink(&outside, nearer.join("docs")).unwrap();
    let location = resolve(&doc).unwrap();
    assert!(snapshot(&location).is_err());
    assert!(mutate(&location, add("blocked")).is_err());
    fs::remove_file(nearer.join("docs")).unwrap();
    fs::create_dir(nearer.join("docs")).unwrap();
    fs::write(outside.join("note.md"), "private").unwrap();
    symlink(outside.join("note.md"), &location.path).unwrap();
    assert!(snapshot(&location).is_err());
    assert!(mutate(&location, add("blocked")).is_err());
    fs::remove_file(&location.path).unwrap();
    fs::create_dir(&location.path).unwrap();
    assert!(snapshot(&location).is_err());
    assert_eq!(
        fs::read_to_string(outside.join("note.md")).unwrap(),
        "private"
    );
}

#[test]
fn pinned_parent_and_final_comparison_prevent_redirects_and_lost_updates() {
    let dir = tempdir().unwrap();
    let doc = dir.path().join("spec.md");
    fs::write(&doc, "# Text").unwrap();
    let location = resolve(&doc).unwrap();
    mutate(&location, add("a comment")).unwrap();
    let parent = location.parent(false).unwrap().unwrap();
    let _lock = parent.lock().unwrap();
    let snapshot = parent.read().unwrap().unwrap();
    let edited = String::from_utf8(snapshot.clone())
        .unwrap()
        .replace("[open]", "[resolved]");
    fs::write(&location.path, &edited).unwrap();
    assert!(matches!(
        parent.commit(Some(&snapshot), Some("new")),
        Err(Error::Conflict(_))
    ));
    assert_eq!(fs::read_to_string(&location.path).unwrap(), edited);

    let store_dir = location.path.parent().unwrap();
    let moved = dir.path().join("moved");
    let unrelated = dir.path().join("unrelated");
    fs::create_dir(&unrelated).unwrap();
    fs::write(unrelated.join("spec.md"), "outside").unwrap();
    fs::rename(store_dir, &moved).unwrap();
    symlink(&unrelated, store_dir).unwrap();
    parent.commit(Some(edited.as_bytes()), Some("new")).unwrap();
    assert_eq!(fs::read_to_string(moved.join("spec.md")).unwrap(), "new");
    assert_eq!(
        fs::read_to_string(unrelated.join("spec.md")).unwrap(),
        "outside"
    );
}

#[test]
fn projection_tracks_unicode_structure_html_and_multiline_code() {
    let source = concat!(
        "# Routing\n\n",
        "Before **two** &amp; [a link](target.md) 😀![alt](x.png).[^n]\n\n",
        "```diff\n-old\n+new\n```\n\n",
        "<div>\nliteral &amp;\n</div>\n\n",
        "- [x] task\n\n",
        "| first | second |\n| - | - |\n| a | b |\n\n",
        "[^n]: Footnote *prose*.\n\n",
        "â€™ `two\nlines`\n"
    );
    let projection = TextProjection::new(source);
    assert_eq!(projection.revision, source_revision(source));
    assert_eq!(projection.text(), "Routing Before two & a link 😀. -old +new <div> literal &amp; </div> task first second a b Footnote prose. ’ two lines");
    let offsets = |quote: &str| {
        let token = projection::normalized(quote);
        let text = projection::normalized(&projection.text());
        let start = text
            .windows(token.len())
            .position(|candidate| candidate == token)
            .unwrap();
        start..start + token.len()
    };
    for (quote, line) in [
        ("-old", 6),
        ("+new", 7),
        ("literal", 11),
        ("lines", 23),
        ("😀", 3),
    ] {
        let range = offsets(quote);
        assert_eq!(projection.lines(range.clone()), (line, line), "{quote}");
        assert_eq!(projection.section(range.start), "Routing");
        assert_eq!(projection.quote(range).as_deref(), Some(quote));
    }
    let crlf = TextProjection::new("```\r\none\r\ntwo\r\n```\r\n");
    assert_eq!(crlf.lines(3..6), (3, 3));
}

#[test]
fn anchored_comments_reopen_at_the_exact_offset_and_can_detach_then_reattach() {
    let dir = tempdir().unwrap();
    let doc = dir.path().join("spec.md");
    let source = "# Group\n\none **same** twice same\n";
    fs::write(&doc, source).unwrap();
    let location = resolve(&doc).unwrap();
    let first = mutate(&location, selection(source, "same", 8..12, "first")).unwrap();
    let second = mutate(&location, selection(source, "same", 17..21, "second")).unwrap();
    let reopened = read(&location, &TextProjection::new(source)).unwrap();
    assert_eq!(second, reopened);
    assert_eq!(reopened.annotations[0].range, Some(8..12));
    assert_eq!(reopened.annotations[1].range, Some(17..21));
    let moved = read(&location, &TextProjection::new(&format!("\n{source}"))).unwrap();
    assert_eq!(moved.annotations[1].lines, Some((4, 4)));
    let detached = read(&location, &TextProjection::new("Gone.")).unwrap();
    assert!(!detached.annotations[0].attached);
    assert_eq!(detached.annotations[0].lines, Some((3, 3)));
    assert_eq!(reopened.store_revision, detached.store_revision);
    let ambiguous = Store::parse("## a1 [open] line 1\n\n> same\n\nbody");
    assert!(TextProjection::new("same same")
        .locate(&ambiguous.annotations[0])
        .is_none());
    let section =
        Store::parse("## a1 [open] line 3\n<!-- markview: section=\"B\" -->\n\n> same\n\nbody");
    assert_eq!(
        TextProjection::new("# A\n\nsame\n\n# B\n\nsame").locate(&section.annotations[0]),
        Some(6..10)
    );
    assert!(matches!(
        mutate(&location, change(1, &first, Change::Delete)),
        Err(Error::Conflict(_))
    ));
    fs::write(&doc, "new source").unwrap();
    for action in [
        Change::Edit("new".to_owned()),
        Change::Status(Status::Resolved),
        Change::Delete,
    ] {
        assert!(matches!(
            mutate(&location, change(1, &first, action)),
            Err(Error::Conflict(_))
        ));
    }
    assert!(matches!(
        mutate(&location, selection(source, "same", 8..12, "stale")),
        Err(Error::Conflict(_))
    ));
    assert_eq!(
        read(&location, &TextProjection::new(source)).unwrap(),
        reopened
    );
}

#[test]
fn mutation_refreshes_attached_hints_preserves_preamble_and_reuses_lock() {
    use std::os::unix::fs::MetadataExt;
    let dir = tempdir().unwrap();
    let doc = dir.path().join("doc.md");
    let source = "# Title\n\ntext";
    fs::write(&doc, source).unwrap();
    let location = resolve(&doc).unwrap();
    let review = mutate(&location, selection(source, "text", 5..9, "content")).unwrap();
    let lock = location.path.with_file_name("doc.md.markview-lock");
    let inode = fs::metadata(&lock).unwrap().ino();
    fs::write(&doc, "\n# Title\n\ntext").unwrap();
    let refreshed = mutate(
        &location,
        change(1, &review, Change::Status(Status::Resolved)),
    )
    .unwrap();
    assert!(fs::read_to_string(&location.path)
        .unwrap()
        .contains("## a1 [resolved] line 4"));
    let deleted = mutate(&location, change(1, &refreshed, Change::Delete)).unwrap();
    assert!(!location.path.exists());
    fs::write(&location.path, "").unwrap();
    let empty = read(&location, &TextProjection::new(source)).unwrap();
    assert_ne!(empty.store_revision, deleted.store_revision);
    fs::write(
        &location.path,
        "My notes\n\n## a9 [open] document\n\nExternal\n",
    )
    .unwrap();
    let merge = mutate(&location, add("extra")).unwrap();
    assert_eq!(merge.annotations[1].id, 10);
    let mut result = merge;
    for id in [9, 10] {
        result = mutate(&location, change(id, &result, Change::Delete)).unwrap();
    }
    assert_eq!(fs::read_to_string(&location.path).unwrap(), "My notes\n\n");
    assert_eq!(fs::metadata(lock).unwrap().ino(), inode);
}

#[test]
fn refuses_ambiguous_or_lossy_store_rewrites() {
    let dir = tempdir().unwrap();
    let doc = dir.path().join("doc.md");
    fs::write(&doc, "yes").unwrap();
    let location = resolve(&doc).unwrap();
    mutate(&location, add("setup")).unwrap();
    for contents in [
        "## a1 [open] document\n\nA\n\n## a01 [open] document\n\nB",
        "## a1 [open] document\n<!-- markview: what is this -->\n\nBody",
        "## a1 [open] line 1\n\nText section with no quote",
    ] {
        fs::write(&location.path, contents).unwrap();
        assert!(mutate(&location, add("must not overwrite")).is_err());
        assert_eq!(fs::read_to_string(&location.path).unwrap(), contents);
    }
}

#[test]
fn concurrent_child() {
    if let Some(path) = std::env::var_os("MARKVIEW_ANNOTATION_TEST_DOCUMENT") {
        mutate(
            &resolve(Path::new(&path)).unwrap(),
            add("from another process"),
        )
        .unwrap();
    }
}

#[test]
fn simultaneous_adds_merge_across_processes() {
    let dir = tempdir().unwrap();
    let doc = dir.path().join("doc.md");
    fs::write(&doc, "hello").unwrap();
    let children: Vec<_> = (0..6)
        .map(|_| {
            std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "annotations::tests::concurrent_child"])
                .env("MARKVIEW_ANNOTATION_TEST_DOCUMENT", &doc)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    for child in children {
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let review = read(&resolve(&doc).unwrap(), &TextProjection::new("hello")).unwrap();
    assert_eq!(
        review
            .annotations
            .iter()
            .map(|record| record.id)
            .collect::<Vec<_>>(),
        [1, 2, 3, 4, 5, 6]
    );
}

#[test]
fn form_enforces_encoded_and_unicode_limits() {
    let scalar = "%F0%9F%98%80";
    let max = format!(
        "op=add&source_revision=revision&start=0&end=1000&quote={}&body={}",
        scalar.repeat(1000),
        scalar.repeat(10000)
    );
    assert!(Request::parse(max.as_bytes()).is_ok());
    assert!(Request::parse(format!("{max}{scalar}").as_bytes()).is_err());
    for malformed in [
        "op=add&body=%XZ",
        "op=add&body=%FF",
        "op=add&body=%00",
        "op=add&body=%",
        "op=add&body=+++",
        "op=add&body=abc&start=1",
        "op=add&body=abc&body=xyz",
        "op=edit&body=abc&id=a1",
        "op=status&id=a1&store_revision=rev&status=bad",
        "op=delete&id=a+1&store_revision=abc",
        "op=add&unexpected=true&body=abc",
    ] {
        assert!(Request::parse(malformed.as_bytes()).is_err(), "{malformed}");
    }
    assert!(
        Request::parse(format!("op=add&body=x&doc={}", "x".repeat(16 * 1024)).as_bytes()).is_err()
    );
    assert!(Request::parse(&vec![b'x'; FORM_CAP + 1]).is_err());
}

#[test]
fn watch_plan_reports_creation_replacement_deletion_and_ancestor_switch() {
    use notify::{event::ModifyKind, Event, EventKind};
    let dir = tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let docs = root.join("docs");
    fs::create_dir(&docs).unwrap();
    let doc = docs.join("a.md");
    let other = docs.join("b.md");
    fs::write(&doc, "a").unwrap();
    fs::write(&other, "b").unwrap();
    let mut watcher = StoreWatcher::new(|_| {}).unwrap();
    watcher.sync([doc.clone(), other.clone()]);
    let changed = |watcher: &mut StoreWatcher, path: std::path::PathBuf| {
        watcher.changed(&[Event::new(EventKind::Modify(ModifyKind::Any)).add_path(path)])
    };
    let store = root.join(".markview/annotations/docs");
    fs::create_dir_all(&store).unwrap();
    let file = store.join("a.md");
    fs::write(&file, "## a1 [open] document\n\nA\n").unwrap();
    assert_eq!(changed(&mut watcher, root.join(".markview")).len(), 2);
    watcher.sync([doc.clone()]);
    fs::write(store.join("b.md"), "unrelated").unwrap();
    assert!(changed(&mut watcher, store.join("b.md")).is_empty());

    let replacement = store.join("new.tmp");
    fs::write(&replacement, "## a1 [resolved] document\n\nA\n").unwrap();
    assert!(changed(&mut watcher, replacement.clone()).is_empty());
    fs::rename(replacement, &file).unwrap();
    assert_eq!(
        changed(&mut watcher, file.clone()),
        std::slice::from_ref(&doc)
    );
    fs::remove_file(&file).unwrap();
    assert_eq!(changed(&mut watcher, file), std::slice::from_ref(&doc));
    fs::remove_dir_all(root.join(".markview")).unwrap();
    assert_eq!(changed(&mut watcher, root.join(".markview")), [doc]);
}

#[test]
fn store_file_operations_use_libc_instead_of_hand_declared_flags() {
    // Hand-copied `O_*` values are wrong on some architectures, which silently
    // turns `O_NOFOLLOW` into a different flag. Every flag must come from libc.
    let source = include_str!("paths.rs");
    for flag in [
        "O_NOFOLLOW",
        "O_DIRECTORY",
        "O_CREAT",
        "O_EXCL",
        "O_CLOEXEC",
        "O_NONBLOCK",
    ] {
        assert!(source.contains(&format!("libc::{flag}")), "{flag}");
    }
    assert!(
        !source.contains("extern \"C\""),
        "hand-declared C functions"
    );
    assert!(
        !source.contains("target_arch"),
        "per-architecture constants"
    );
    let literal = source.lines().find(|line| {
        let line = line.trim_start();
        (line.starts_with("pub const") || line.starts_with("const")) && line.contains("i32 =")
    });
    assert_eq!(literal, None, "hand-defined open flag");
}

#[test]
fn unsupported_platform_store_operations_report_a_clear_error() {
    let message = "annotations are unsupported on this platform";
    let error = unsupported::resolve(Path::new("doc.md")).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    assert_eq!(error.to_string(), message);
    let location = unsupported::Location {
        document: "doc.md".into(),
        root: ".".into(),
        path: ".markview/annotations/doc.md".into(),
        relative: "doc.md".into(),
    };
    assert_eq!(location.parent(true).err().unwrap().to_string(), message);
    assert_eq!(
        unsupported::read_source(&location).unwrap_err().to_string(),
        message
    );
}
