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

fn store_fixture(source: &str, store: &str) -> (tempfile::TempDir, Location) {
    let dir = tempdir().unwrap();
    let doc = dir.path().join("doc.md");
    fs::write(&doc, source).unwrap();
    let location = resolve(&doc).unwrap();
    fs::create_dir_all(location.path.parent().unwrap()).unwrap();
    fs::write(&location.path, store).unwrap();
    (dir, location)
}

#[test]
fn hand_edited_sections_do_not_freeze_store_writes() {
    let source = "one\n\nthree\n";
    for store in [
        "## a1 [open] line 3\n\nPlease reword this line.\n",
        "## a1 [open] document\n<!-- markview: what is this -->\n\nBody\n",
    ] {
        let (_dir, location) = store_fixture(source, store);
        let added = mutate(&location, add("whole document")).unwrap();
        assert_eq!(added.annotations.len(), 2, "{store}");
        let written = fs::read_to_string(&location.path).unwrap();
        // The untouched section is written back byte for byte.
        assert!(written.starts_with(store), "{written}");
        assert!(written.contains("\n## a2 [open] document\n"), "{written}");

        let resolved = mutate(
            &location,
            change(1, &added, Change::Status(Status::Resolved)),
        );
        assert_eq!(resolved.unwrap().annotations.len(), 2, "{store}");
        let written = fs::read_to_string(&location.path).unwrap();
        assert!(written.starts_with(&store.replace("[open]", "[resolved]")));
    }
}

#[test]
fn unsafe_rewrites_are_refused_and_name_the_annotation() {
    let duplicate = "## a1 [open] document\n\nA\n\n## a01 [open] document\n\nB\n";
    let (_dir, location) = store_fixture("text", duplicate);
    let error = mutate(&location, add("must not overwrite")).unwrap_err();
    assert!(error.to_string().contains("a1"), "{error}");
    assert_eq!(fs::read_to_string(&location.path).unwrap(), duplicate);

    // A line-anchored section without a quote cannot hold a comment that
    // starts with `>`: reading it back would turn the comment into the quote.
    let quoteless = "## a7 [open] line 1\n\nPlease reword this line.\n";
    let (_dir, location) = store_fixture("text", quoteless);
    let review = read(&location, &TextProjection::new("text")).unwrap();
    let edit = Change::Edit("> not a quote".to_owned());
    let error = mutate(&location, change(7, &review, edit)).unwrap_err();
    assert!(error.to_string().contains("a7"), "{error}");
    assert_eq!(fs::read_to_string(&location.path).unwrap(), quoteless);
}

#[test]
fn byte_order_mark_before_the_first_header_is_ignored() {
    let store = "\u{feff}## a1 [open] document\n\nFrom a Windows editor.\n";
    let parsed = Store::parse(store);
    assert_eq!(parsed.annotations.len(), 1);
    assert_eq!(parsed.annotations[0].id, 1);
    let (_dir, location) = store_fixture("text", store);
    let review = mutate(&location, add("second")).unwrap();
    let ids: Vec<_> = review.annotations.iter().map(|record| record.id).collect();
    assert_eq!(ids, [1, 2]);
    assert!(fs::read_to_string(&location.path)
        .unwrap()
        .starts_with(store));
}

#[test]
fn rewriting_a_crlf_store_keeps_crlf_line_endings() {
    let store = "# Notes\r\n\r\n## a1 [open] document\r\n\r\nFirst line\r\nsecond line\r\n";
    let (_dir, location) = store_fixture("text", store);
    let review = mutate(&location, add("added\nwith two lines")).unwrap();
    assert_eq!(review.annotations[0].body, "First line\nsecond line");
    let written = fs::read_to_string(&location.path).unwrap();
    assert!(written.starts_with(store), "{written:?}");
    assert!(written.contains("added\r\nwith two lines\r\n"));
    assert!(
        !written.replace("\r\n", "").contains(['\r', '\n']),
        "{written:?}"
    );
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

#[test]
fn revisions_use_a_fixed_hash_with_known_answers() {
    // Revisions are written into git-tracked store files, so the algorithm
    // must not change between Rust releases. These are FNV-1a/64 vectors; the
    // first four are the published ones.
    for (source, expected) in [
        ("", "cbf29ce484222325"),
        ("a", "af63dc4c8601ec8c"),
        ("foobar", "85944171f73967e8"),
        ("hello", "a430d84680aabd0b"),
        ("naïve 😀 – ok\r\n", "5ca6e7db9489a75c"),
    ] {
        assert_eq!(source_revision(source), expected, "{source:?}");
        assert_eq!(TextProjection::new(source).revision, expected);
    }
    let path = Path::new("/docs/.markview/annotations/a.md");
    let bytes = b"## a1 [open] document\n\nHi\n";
    assert_eq!(store_revision(path, None), "54debf3b3bde8c14");
    assert_eq!(store_revision(path, Some(b"")), "54dec03b3bde8dc7");
    assert_eq!(store_revision(path, Some(bytes)), "0157a2338906b383");
    assert_eq!(
        store_revision(Path::new("/docs/.markview/annotations/b.md"), Some(bytes)),
        "a9522d9f65517264"
    );
}

#[test]
fn stored_start_keeps_its_fast_path_after_reopening() {
    // The quote is ambiguous, so only the persisted `source_revision` and
    // `start` can place it. The revision literal is FNV-1a/64 of the source.
    let dir = tempdir().unwrap();
    let doc = dir.path().join("doc.md");
    let source = "same same";
    fs::write(&doc, source).unwrap();
    let location = resolve(&doc).unwrap();
    fs::create_dir_all(location.path.parent().unwrap()).unwrap();
    fs::write(
        &location.path,
        concat!(
            "## a1 [open] line 1\n",
            "<!-- markview: source_revision=\"bf2fba57fca01dd7\" start=\"4\" -->\n\n",
            "> same\n\nSecond one.\n"
        ),
    )
    .unwrap();
    let review = read(&location, &TextProjection::new(source)).unwrap();
    assert_eq!(review.annotations[0].range, Some(4..8));
    assert!(review.annotations[0].attached);
}

fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).unwrap().permissions().mode() & 0o7777
}

#[test]
fn replacing_a_store_keeps_its_permission_bits() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempdir().unwrap();
    let doc = dir.path().join("doc.md");
    fs::write(&doc, "text").unwrap();
    let location = resolve(&doc).unwrap();
    let review = mutate(&location, add("shared with the group")).unwrap();
    fs::set_permissions(&location.path, fs::Permissions::from_mode(0o664)).unwrap();
    mutate(
        &location,
        change(1, &review, Change::Status(Status::Resolved)),
    )
    .unwrap();
    assert!(fs::read_to_string(&location.path)
        .unwrap()
        .contains("[resolved]"));
    assert_eq!(mode(&location.path), 0o664);
}

#[test]
fn umask_child() {
    if let Some(path) = std::env::var_os("MARKVIEW_ANNOTATION_UMASK_DOCUMENT") {
        let umask = std::env::var("MARKVIEW_ANNOTATION_UMASK").unwrap();
        unsafe { libc::umask(libc::mode_t::from_str_radix(&umask, 8).unwrap()) };
        mutate(&resolve(Path::new(&path)).unwrap(), add("first write")).unwrap();
    }
}

#[test]
fn new_store_files_follow_the_umask() {
    // The umask is process-wide, so each case runs in its own child process.
    for (umask, expected) in [("022", 0o644), ("027", 0o640)] {
        let dir = tempdir().unwrap();
        let doc = dir.path().join("doc.md");
        fs::write(&doc, "text").unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "annotations::tests::umask_child"])
            .env("MARKVIEW_ANNOTATION_UMASK_DOCUMENT", &doc)
            .env("MARKVIEW_ANNOTATION_UMASK", umask)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
        let store = resolve(&doc).unwrap().path;
        assert_eq!(mode(&store), expected, "store under umask {umask}");
        assert_eq!(
            mode(&store.with_file_name("doc.md.markview-lock")),
            expected,
            "lock under umask {umask}"
        );
    }
}

#[test]
fn long_document_names_can_be_annotated() {
    // `<name>.markview-lock` would be 262 bytes here, past the 255-byte limit.
    let dir = tempdir().unwrap();
    let name = format!("{}.md", "n".repeat(245));
    assert_eq!(name.len(), 248);
    let doc = dir.path().join(&name);
    fs::write(&doc, "text").unwrap();
    let location = resolve(&doc).unwrap();
    let first = mutate(&location, add("first")).unwrap();
    let second = mutate(
        &location,
        change(1, &first, Change::Edit("edited".to_owned())),
    )
    .unwrap();
    assert_eq!(second.annotations[0].body, "edited");
    let locks: Vec<_> = fs::read_dir(location.path.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|entry| *entry != name)
        .collect();
    // One lock per document, reused across writes, still matching the
    // documented `*.markview-lock` ignore pattern.
    assert_eq!(locks.len(), 1, "{locks:?}");
    assert!(locks[0].ends_with(".markview-lock"));
}

#[test]
fn failed_first_write_leaves_no_new_directories() {
    let dir = tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    fs::create_dir(root.join("sub")).unwrap();
    fs::write(root.join(".git"), "gitdir: elsewhere").unwrap();
    let doc = root.join("sub/doc.md");
    fs::write(&doc, "text").unwrap();
    let stale = resolve(&doc).unwrap();
    assert_eq!(stale.root, root);
    // A nearer store appears after the location was resolved, so the add is
    // refused only after the directories for the old location were created.
    fs::create_dir_all(root.join("sub/.markview/annotations")).unwrap();
    assert!(matches!(
        mutate(&stale, add("first")),
        Err(Error::Conflict("annotation store moved"))
    ));
    assert!(!root.join(".markview").exists());
    assert_eq!(
        fs::read_dir(root.join("sub/.markview/annotations"))
            .unwrap()
            .count(),
        0
    );

    // Directories that already existed are left alone, and a later write to
    // the same location still works.
    fs::remove_dir_all(root.join("sub/.markview")).unwrap();
    fs::create_dir_all(root.join(".markview/annotations")).unwrap();
    fs::create_dir_all(root.join("sub/.markview/annotations")).unwrap();
    assert!(mutate(&stale, add("first")).is_err());
    assert!(root.join(".markview/annotations").is_dir());
    assert!(!root.join(".markview/annotations/sub").exists());
    fs::remove_dir_all(root.join("sub/.markview")).unwrap();
    assert_eq!(mutate(&stale, add("first")).unwrap().annotations.len(), 1);
}

fn counted<T>(work: impl FnOnce() -> T) -> (T, usize, usize) {
    use projection::{BUILDS, LOCATES};
    let before = (BUILDS.get(), LOCATES.get());
    let result = work();
    (result, BUILDS.get() - before.0, LOCATES.get() - before.1)
}

#[test]
fn review_cache_resolves_once_per_source_and_store_revision() {
    let dir = tempdir().unwrap();
    let doc = dir.path().join("doc.md");
    let source = "# Title\n\nfirst second third";
    fs::write(&doc, source).unwrap();
    let location = resolve(&doc).unwrap();
    let cache = ReviewCache::default();
    let (added, builds, _) =
        counted(|| cache.mutate(&location, selection(source, "second", 10..16, "one")));
    let added = added.unwrap();
    // The selection is validated before and under the lock: one projection.
    assert_eq!(builds, 1);

    // Reads of unchanged revisions reuse both the projection and the result.
    let (reviews, builds, locates) = counted(|| {
        (0..5)
            .map(|_| cache.read(&location, source).unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!((builds, locates), (0, 0));
    assert!(reviews.iter().all(|review| *review == added));

    // A store change resolves again, against the cached projection.
    let external = fs::read_to_string(&location.path)
        .unwrap()
        .replace("[open]", "[resolved]");
    fs::write(&location.path, external).unwrap();
    let (review, builds, locates) = counted(|| cache.read(&location, source).unwrap());
    assert_eq!((builds, locates), (0, 1));
    assert_eq!(review.annotations[0].status, Status::Resolved);
    assert_ne!(review.store_revision, added.store_revision);

    // A source change builds one new projection, then is cached in turn.
    let edited = format!("\n{source}");
    let (review, builds, _) = counted(|| {
        cache.read(&location, &edited).unwrap();
        cache.read(&location, &edited).unwrap()
    });
    assert_eq!(builds, 1);
    assert_eq!(review.annotations[0].lines, Some((4, 4)));
    assert_eq!(
        review,
        read(&location, &TextProjection::new(&edited)).unwrap()
    );
    // The cache's own tally, which other crates use to check their wiring:
    // two projections, and three resolutions (the add and two changes).
    assert_eq!(cache.work(), (2, 3));
}

#[test]
fn mutation_locates_each_annotation_once() {
    let dir = tempdir().unwrap();
    let doc = dir.path().join("doc.md");
    let source = "alpha beta gamma delta";
    fs::write(&doc, source).unwrap();
    let location = resolve(&doc).unwrap();
    mutate(&location, selection(source, "alpha", 0..5, "one")).unwrap();
    mutate(&location, selection(source, "beta", 5..9, "two")).unwrap();
    let review = mutate(&location, selection(source, "gamma", 9..14, "three")).unwrap();
    // Stale hints force a real search, the expensive case.
    let stale = fs::read_to_string(&location.path)
        .unwrap()
        .replace("source_revision=\"", "source_revision=\"stale-");
    fs::write(&location.path, &stale).unwrap();
    let review = read(&location, &TextProjection::new(source)).unwrap_or(review);
    let (result, _, locates) = counted(|| {
        mutate(
            &location,
            change(2, &review, Change::Status(Status::Resolved)),
        )
    });
    let result = result.unwrap();
    assert_eq!(locates, 3, "one search per stored annotation");
    assert_eq!(result.annotations.len(), 3);
    assert_eq!(
        result,
        read(&location, &TextProjection::new(source)).unwrap()
    );
}

#[test]
fn locating_quotes_does_not_scan_the_whole_document_per_annotation() {
    use std::time::Instant;
    // About 2 MB of prose with 500 distinct, findable quotes.
    let mut source = String::new();
    let mut quotes = Vec::new();
    let mut paragraph = 0;
    while source.len() < 2_000_000 {
        if paragraph % 40 == 0 && quotes.len() < 500 {
            let quote = format!("Marker {} closes the section", quotes.len());
            source.push_str(&format!("Review note. {quote}. "));
            quotes.push(quote);
        }
        source.push_str("The quick brown fox jumps over the lazy dog near the riverbank. ");
        paragraph += 1;
        if paragraph % 8 == 0 {
            source.push_str("\n\n");
        }
    }
    assert_eq!(quotes.len(), 500);
    let projection = TextProjection::new(&source);
    // No `source_revision` hint, as after the document was edited.
    let records: Vec<_> = quotes
        .iter()
        .map(|quote| {
            Store::parse(&format!("## a1 [open] line 1\n\n> {quote}\n\nNote\n"))
                .annotations
                .remove(0)
        })
        .collect();

    let start = Instant::now();
    let located: Vec<_> = records
        .iter()
        .map(|record| projection.locate(record))
        .collect();
    let fast = start.elapsed() / records.len() as u32;
    // The full scan is slow, so time it on a sample and compare per record.
    let sample = records.iter().step_by(50);
    let start = Instant::now();
    let naive: Vec<_> = sample
        .clone()
        .map(|record| projection.locate_naive(record))
        .collect();
    let slow = start.elapsed() / naive.len() as u32;

    assert!(located.iter().all(Option::is_some));
    assert_eq!(
        located.iter().step_by(50).cloned().collect::<Vec<_>>(),
        naive,
        "the indexed search must agree with the full scan"
    );
    eprintln!("locate per annotation: indexed {fast:?}, full scan {slow:?}");
    assert!(
        fast * 5 < slow,
        "per annotation: indexed {fast:?}, full scan {slow:?}"
    );
}

#[test]
fn external_status_flip_before_commit_is_a_conflict_not_a_lost_update() {
    let dir = tempdir().unwrap();
    let doc = dir.path().join("doc.md");
    fs::write(&doc, "text").unwrap();
    let location = resolve(&doc).unwrap();
    let review = mutate(&location, add("needs work")).unwrap();
    // An agent resolves the comment in its editor, which takes no lock, after
    // Markview read the store but before it replaces the file.
    let store = location.path.clone();
    let resolved = fs::read_to_string(&store)
        .unwrap()
        .replace("[open]", "[resolved]");
    let external = resolved.clone();
    BEFORE_COMMIT.set(Some(Box::new(move || fs::write(&store, external).unwrap())));
    let edit = change(1, &review, Change::Edit("rewritten".to_owned()));
    assert!(matches!(
        mutate(&location, edit),
        Err(Error::Conflict("annotation store changed on disk"))
    ));
    // The agent's edit survives, and Markview's did not land on top of it.
    assert_eq!(fs::read_to_string(&location.path).unwrap(), resolved);
    let current = read(&location, &TextProjection::new("text")).unwrap();
    assert_eq!(current.annotations[0].status, Status::Resolved);
    assert_eq!(current.annotations[0].body, "needs work");
    // With the list refreshed, the same edit goes through.
    let edit = change(1, &current, Change::Edit("rewritten".to_owned()));
    assert_eq!(
        mutate(&location, edit).unwrap().annotations[0].body,
        "rewritten"
    );
}

#[test]
fn cards_from_before_a_source_change_conflict_until_refreshed() {
    let dir = tempdir().unwrap();
    let doc = dir.path().join("doc.md");
    let source = "# Title\n\nalpha beta";
    fs::write(&doc, source).unwrap();
    let location = resolve(&doc).unwrap();
    let shown = mutate(&location, selection(source, "alpha", 5..10, "first")).unwrap();
    // The document gains a line, and another writer's add rewrites the store
    // with refreshed line hints. The reviewer's cards are now out of date.
    let edited = format!("\n{source}");
    fs::write(&doc, &edited).unwrap();
    mutate(&location, add("from another window")).unwrap();
    let rewritten = fs::read_to_string(&location.path).unwrap();
    assert!(rewritten.contains("## a1 [open] line 4"), "{rewritten}");

    for stale in [
        Change::Edit("changed".to_owned()),
        Change::Status(Status::Resolved),
        Change::Delete,
    ] {
        assert!(matches!(
            mutate(&location, change(1, &shown, stale)),
            Err(Error::Conflict("annotation store changed on disk"))
        ));
        assert_eq!(fs::read_to_string(&location.path).unwrap(), rewritten);
    }
    let fresh = read(&location, &TextProjection::new(&edited)).unwrap();
    assert_eq!(fresh.annotations[0].lines, Some((4, 4)));
    let resolved = mutate(
        &location,
        change(1, &fresh, Change::Status(Status::Resolved)),
    );
    assert_eq!(resolved.unwrap().annotations[1].status, Status::Resolved);
}

#[test]
fn first_writes_never_follow_a_symlinked_store_component() {
    for component in [
        ".markview",
        ".markview/annotations",
        ".markview/annotations/docs",
    ] {
        let dir = tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        fs::create_dir(root.join("docs")).unwrap();
        fs::write(root.join(".git"), "gitdir: elsewhere").unwrap();
        let doc = root.join("docs/spec.md");
        fs::write(&doc, "text").unwrap();
        // The store location is resolved while nothing exists yet.
        let location = resolve(&doc).unwrap();
        assert_eq!(
            location.path,
            root.join(".markview/annotations/docs/spec.md")
        );
        // Then one component appears as a link to somewhere else.
        let outside = root.join("outside");
        fs::create_dir(&outside).unwrap();
        let link = root.join(component);
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        symlink(&outside, &link).unwrap();

        assert!(
            mutate(&location, add("first write")).is_err(),
            "{component}"
        );
        assert!(
            ReviewCache::default()
                .mutate(&location, add("again"))
                .is_err(),
            "{component}"
        );
        // Nothing was created through the link: no store, lock or directory.
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 0, "{component}");
        assert!(fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
    }
}

#[test]
fn ties_between_identical_quotes_go_to_the_nearest_stored_line() {
    // No context or section hints, so only the last known line can choose.
    let source = "same\n\nfiller\n\nfiller\n\nsame\n";
    let projection = TextProjection::new(source);
    let near_top = Store::parse("## a1 [open] line 2\n\n> same\n\nNote\n");
    let near_end = Store::parse("## a1 [open] line 6\n\n> same\n\nNote\n");
    let halfway = Store::parse("## a1 [open] line 4\n\n> same\n\nNote\n");
    assert_eq!(projection.locate(&near_top.annotations[0]), Some(0..4));
    assert_eq!(projection.locate(&near_end.annotations[0]), Some(16..20));
    // Equally far from both: ambiguous, so the comment detaches.
    assert_eq!(projection.locate(&halfway.annotations[0]), None);
}

#[test]
fn a_quote_across_blocks_reports_its_line_range_and_detached_hints_survive_rewrites() {
    let dir = tempdir().unwrap();
    let doc = dir.path().join("doc.md");
    let source = "# Title\n\nfirst **bold** end\n\nsecond paragraph\n";
    fs::write(&doc, source).unwrap();
    let location = resolve(&doc).unwrap();
    // "bold end second": out of the emphasis, across the paragraph break.
    let review = mutate(
        &location,
        selection(source, "bold end second", 10..23, "spans blocks"),
    )
    .unwrap();
    assert_eq!(review.annotations[0].lines, Some((3, 5)));
    assert_eq!(
        review.annotations[0].quote.as_deref(),
        Some("bold end second")
    );
    let stored = fs::read_to_string(&location.path).unwrap();
    assert!(stored.contains("## a1 [open] lines 3-5\n"), "{stored}");

    // The text goes away: the comment detaches but keeps what it knew.
    fs::write(&doc, "# Title\n\nrewritten\n").unwrap();
    let detached = mutate(&location, add("another comment")).unwrap();
    assert!(!detached
        .annotations
        .iter()
        .any(|record| record.id == 1 && record.attached));
    let rewritten = fs::read_to_string(&location.path).unwrap();
    let section = |text: &str| text.split("## a2 ").next().unwrap().to_owned();
    assert_eq!(section(&rewritten), section(&stored));
    // The text comes back one line lower and the comment reattaches there.
    fs::write(&doc, format!("\n{source}")).unwrap();
    let back = read(&location, &TextProjection::new(&format!("\n{source}"))).unwrap();
    assert_eq!(back.annotations[0].lines, Some((4, 6)));
    assert!(back.annotations[0].attached);
}

#[test]
fn review_styles_keep_annotations_out_of_print() {
    let print = REVIEW_STYLE
        .split("@media print {")
        .nth(1)
        .expect("print rules");
    for selector in [
        ".markview-review,",
        ".markview-review-selection,",
        ".markview-review-selection-note,",
        ".markview-review-overlay",
    ] {
        assert!(print.contains(selector), "{selector}");
    }
    assert!(print.contains("display: none !important"));
    assert!(print.contains("::highlight(markview-annotations) { background: transparent;"));
}

#[test]

fn alert_projection_excludes_marker_and_preserves_source_lines() {
    let projection = TextProjection::new("> [!NOTE]\n> Body **text**\n>\n> - Nested\n");
    assert_eq!(projection.text(), "Body text Nested");
    assert_eq!(projection.lines(0..8), (2, 2));
    assert_eq!(projection.lines(8..14), (4, 4));
}
