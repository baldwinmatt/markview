use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};

use markview::annotations::{self, Error, Location, Review, Status, TextProjection};

use super::{format_annotations, validated_served_path, ServeConfig, ServedDocument};

pub const ENDPOINT: &str = "/_markview/review";
const CHANGED: Error =
    Error::Conflict("The collection changed. Review the latest page and confirm again.");
const FINISHED: Error = Error::Conflict("Review finished. You can close this tab.");

#[derive(Debug)]
pub struct Session {
    accepted: Mutex<bool>,
    result: mpsc::Sender<AcceptedReview>,
}

pub struct AcceptedReview {
    pub output: String,
    pub changes: bool,
}

impl Session {
    pub fn new() -> (Arc<Self>, mpsc::Receiver<AcceptedReview>) {
        let (result, receiver) = mpsc::channel();
        (
            Arc::new(Self {
                accepted: Mutex::new(false),
                result,
            }),
            receiver,
        )
    }

    pub fn finished(&self) -> bool {
        *self.accepted.lock().expect("review lock")
    }

    pub fn mutate<T>(&self, operation: impl FnOnce() -> Result<T, Error>) -> Result<T, Error> {
        let accepted = self.accepted.lock().expect("review lock");
        if *accepted {
            return Err(FINISHED);
        }
        operation()
    }

    pub fn summary(&self, config: &ServeConfig) -> Result<String, Error> {
        let accepted = self.accepted.lock().expect("review lock");
        if *accepted {
            return Ok(r#"{"finished":true}"#.to_owned());
        }
        let snapshot = Snapshot::capture(config)?;
        let (comments, documents) = snapshot.counts();
        Ok(format!(
            r#"{{"review_revision":"{}","comments":{comments},"documents":{documents}}}"#,
            snapshot.revision
        ))
    }

    pub fn finish(&self, config: &ServeConfig, body: &[u8]) -> Result<AcceptedReview, Error> {
        let mut accepted = self.accepted.lock().expect("review lock");
        if *accepted {
            return Err(FINISHED);
        }
        let mut fields = annotations::parse_form(body)?;
        let invalid = Error::Invalid("missing or invalid review fields");
        let doc = fields
            .remove("doc")
            .ok_or(Error::Invalid("expected a document route"))?;
        let wanted_revision = fields
            .remove("review_revision")
            .ok_or(Error::Invalid("expected a review revision"))?;
        let source_revision = fields
            .remove("source_revision")
            .ok_or(Error::Invalid("expected a source revision"))?;
        let changes = match fields.remove("verdict").as_deref() {
            Some("approved") => false,
            Some("changes") => true,
            _ => return Err(invalid),
        };
        if fields.remove("op").as_deref() != Some("finish") || !fields.is_empty() {
            return Err(invalid);
        }
        let page = if doc == "/" {
            config.default_document()
        } else {
            config.document_by_route(&doc)
        }
        .ok_or(CHANGED)?;
        let snapshot = Snapshot::capture(config)?;
        if snapshot.revision != wanted_revision
            || !snapshot.documents.iter().any(|doc| {
                doc.path == page.source_path && doc.review.source_revision == source_revision
            })
        {
            return Err(CHANGED);
        }
        if changes && snapshot.counts().0 == 0 {
            return Err(Error::Invalid(
                "Request changes requires at least one open comment.",
            ));
        }
        let output = snapshot.output(changes, &std::env::current_dir()?);
        if snapshot.revision
            != collection_revision(
                config,
                config
                    .documents
                    .iter()
                    .map(|doc| document_revisions(config, doc)),
            )
            .map_err(|_| CHANGED)?
        {
            return Err(CHANGED);
        }
        *accepted = true;
        Ok(AcceptedReview { output, changes })
    }

    pub fn publish(&self, result: AcceptedReview) {
        let _ = self.result.send(result);
    }
}

type Revisions = (String, String);

struct DocumentSnapshot {
    path: PathBuf,
    store: PathBuf,
    review: Review,
}

struct Snapshot {
    documents: Vec<DocumentSnapshot>,
    revision: String,
}

impl Snapshot {
    fn capture(config: &ServeConfig) -> Result<Self, Error> {
        let documents = config
            .documents
            .iter()
            .map(|doc| {
                let (location, source, bytes) = read_document(config, doc)?;
                let review = Review::from_snapshot(
                    &location,
                    bytes.as_deref(),
                    &TextProjection::new(&source),
                )
                .map_err(|error| {
                    io::Error::other(format!("{}: {error}", location.path.display()))
                })?;
                Ok(DocumentSnapshot {
                    path: location.document,
                    store: location.path,
                    review,
                })
            })
            .collect::<Result<Vec<_>, Error>>()?;
        let revision = collection_revision(
            config,
            documents.iter().map(|doc| {
                Ok((
                    doc.review.source_revision.clone(),
                    doc.review.store_revision.clone(),
                ))
            }),
        )?;
        Ok(Self {
            documents,
            revision,
        })
    }

    fn counts(&self) -> (usize, usize) {
        self.documents
            .iter()
            .fold((0, 0), |(comments, documents), doc| {
                let count = doc
                    .review
                    .annotations
                    .iter()
                    .filter(|item| item.status == Status::Open)
                    .count();
                (comments + count, documents + usize::from(count > 0))
            })
    }

    fn output(&self, changes: bool, cwd: &Path) -> String {
        let (comments, documents) = self.counts();
        let verdict = if changes {
            "changes requested"
        } else {
            "approved"
        };
        let comment = if comments == 1 { "comment" } else { "comments" };
        let document = if documents == 1 {
            "document"
        } else {
            "documents"
        };
        let mut output =
            format!("Review: {verdict} ({comments} open {comment} in {documents} {document})\n\n");
        let mut has_records = false;
        for doc in &self.documents {
            if doc.review.annotations.is_empty() {
                continue;
            }
            has_records = true;
            format_annotations(
                &mut output,
                doc.path.strip_prefix(cwd).unwrap_or(&doc.path),
                doc.store.strip_prefix(cwd).unwrap_or(&doc.store),
                &doc.review,
                false,
            );
        }
        if !has_records {
            output.push_str("No open annotations in the reviewed collection.\n");
        }
        output
    }
}

fn read_document(
    config: &ServeConfig,
    doc: &ServedDocument,
) -> io::Result<(Location, String, Option<Vec<u8>>)> {
    let path = validated_served_path(&config.root, &doc.source_path)
        .filter(|path| path == &doc.source_path)
        .ok_or_else(|| {
            io::Error::other(format!(
                "served document is unavailable: {}",
                doc.source_path.display()
            ))
        })?;
    let location = annotations::resolve(&path)?;
    if location.document != path {
        return Err(io::Error::other("served document changed location"));
    }
    let source = annotations::read_source(&location)?;
    let store = annotations::snapshot(&location)?;
    Ok((location, source, store))
}

fn document_revisions(config: &ServeConfig, doc: &ServedDocument) -> Result<Revisions, Error> {
    let (location, source, store) = read_document(config, doc)?;
    Ok((
        annotations::source_revision(&source),
        annotations::store_revision(&location.path, store.as_deref()),
    ))
}

fn collection_revision(
    config: &ServeConfig,
    revisions: impl Iterator<Item = Result<Revisions, Error>>,
) -> Result<String, Error> {
    let mut hash = DefaultHasher::new();
    config.default_document.hash(&mut hash);
    config.documents.len().hash(&mut hash);
    for (doc, revisions) in config.documents.iter().zip(revisions) {
        (&doc.route_path, &doc.source_path, revisions?).hash(&mut hash);
    }
    Ok(format!("{:016x}", hash.finish()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct Fixture {
        directory: tempfile::TempDir,
        first: PathBuf,
        nested: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let first = directory.path().join("first.md");
            let nested = directory.path().join("sub/next.md");
            fs::create_dir_all(nested.parent().unwrap()).unwrap();
            fs::write(&first, "# First\n\nOriginal line.\n").unwrap();
            fs::write(&nested, "# Next\n").unwrap();
            Self {
                directory,
                first,
                nested,
            }
        }

        fn write_store(path: &Path, content: &str) {
            let location = annotations::resolve(path).unwrap();
            fs::create_dir_all(location.path.parent().unwrap()).unwrap();
            fs::write(location.path, content).unwrap();
        }

        fn config(&self, recurse: bool) -> ServeConfig {
            ServeConfig::from_inputs(vec![self.directory.path().into()], 0, recurse).unwrap()
        }

        fn finish_form(config: &ServeConfig, verdict: &str) -> String {
            let snapshot = Snapshot::capture(config).unwrap();
            let doc = &config.documents[0];
            format!(
                "op=finish&verdict={verdict}&doc={}&review_revision={}&source_revision={}",
                doc.route_path, snapshot.revision, snapshot.documents[0].review.source_revision
            )
        }
    }

    #[test]
    fn review_output_uses_served_order_and_only_saved_records() {
        let fixture = Fixture::new();
        Fixture::write_store(&fixture.first, "## a1 [resolved] document\n\nDone\n");
        Fixture::write_store(&fixture.nested, "## a1 [open] document\n\nNested\n");
        let root = fixture
            .first
            .canonicalize()
            .unwrap()
            .parent()
            .unwrap()
            .to_owned();
        let snapshot = Snapshot::capture(&fixture.config(false)).unwrap();
        let resolved = concat!(
            "# Annotations: first.md\n",
            "Store: .markview/annotations/first.md (change `[open]` to `[resolved]` there once addressed)\n\n",
            "No open annotations for first.md.\n\n1 resolved annotation not shown.\n\n",
        );
        assert_eq!(
            snapshot.output(false, &root),
            format!("Review: approved (0 open comments in 0 documents)\n\n{resolved}")
        );
        let snapshot = Snapshot::capture(&fixture.config(true)).unwrap();
        let nested = concat!(
            "# Annotations: sub/next.md\n",
            "Store: .markview/annotations/sub/next.md (change `[open]` to `[resolved]` there once addressed)\n\n",
            "## a1 · document\n\nNested\n\n",
        );
        assert_eq!(
            snapshot.output(true, &root),
            format!(
                "Review: changes requested (1 open comment in 1 document)\n\n{resolved}{nested}"
            )
        );
        let explicit = ServeConfig::from_inputs(
            vec![fixture.nested.clone(), fixture.first.clone()],
            0,
            false,
        )
        .unwrap();
        assert_eq!(
            Snapshot::capture(&explicit).unwrap().output(false, &root),
            format!("Review: approved (1 open comment in 1 document)\n\n{nested}{resolved}")
        );
        let single = ServeConfig::from_inputs(vec![fixture.first.clone()], 0, false).unwrap();
        assert_eq!(
            Snapshot::capture(&single).unwrap().output(false, &root),
            format!("Review: approved (0 open comments in 0 documents)\n\n{resolved}")
        );
    }

    #[test]
    fn review_conflicts_leave_session_open() {
        let fixture = Fixture::new();
        let config = fixture.config(false);
        let (session, _) = Session::new();
        let changes = Fixture::finish_form(&config, "changes");
        assert!(matches!(
            session.finish(&config, changes.as_bytes()),
            Err(Error::Invalid(_))
        ));

        let old = Fixture::finish_form(&config, "approved");
        assert!(matches!(
            session.finish(&fixture.config(true), old.as_bytes()),
            Err(Error::Conflict(_))
        ));
        assert!(matches!(
            session.finish(
                &config,
                old.replace("doc=/first.md", "doc=/outside.md").as_bytes()
            ),
            Err(Error::Conflict(_))
        ));
        Fixture::write_store(&fixture.first, "## a1 [open] document\n\nNew comment\n");
        assert!(matches!(
            session.finish(&config, old.as_bytes()),
            Err(Error::Conflict(_))
        ));
        let old = Fixture::finish_form(&config, "approved");
        fs::write(&fixture.first, "# Edited\n").unwrap();
        assert!(matches!(
            session.finish(&config, old.as_bytes()),
            Err(Error::Conflict(_))
        ));
        fs::write(&fixture.first, [0xff]).unwrap();
        assert!(matches!(session.summary(&config), Err(Error::Io(_))));
        fs::write(&fixture.first, "# Valid again\n").unwrap();
        let location = annotations::resolve(&fixture.first).unwrap();
        fs::write(&location.path, [0xff]).unwrap();
        assert!(matches!(session.summary(&config), Err(Error::Io(_))));
        assert!(!session.finished());
    }

    #[test]
    fn review_finish_waits_for_mutations_and_freezes_the_accepted_snapshot() {
        let fixture = Fixture::new();
        let config = fixture.config(false);
        let (session, _) = Session::new();
        let old = Fixture::finish_form(&config, "approved");
        let (admitted, entered) = mpsc::channel();
        let (release, proceed) = mpsc::channel();
        let first = &fixture.first;
        std::thread::scope(|scope| {
            scope.spawn(|| {
                session
                    .mutate(move || {
                        admitted.send(()).unwrap();
                        proceed.recv().unwrap();
                        Fixture::write_store(
                            first,
                            "## a1 [open] line 3\n\n> Original line.\n\nCheck this\n",
                        );
                        Ok(())
                    })
                    .unwrap();
            });
            entered.recv().unwrap();
            let pending_finish = scope.spawn(|| session.finish(&config, old.as_bytes()));
            release.send(()).unwrap();
            assert!(matches!(
                pending_finish.join().unwrap(),
                Err(Error::Conflict(_))
            ));
        });
        let form = Fixture::finish_form(&config, "changes").replace("doc=/first.md", "doc=/");
        let result = std::thread::scope(|scope| {
            let other = scope.spawn(|| session.finish(&config, form.as_bytes()));
            match (
                session.finish(&config, form.as_bytes()),
                other.join().unwrap(),
            ) {
                (Ok(result), Err(Error::Conflict(_))) | (Err(Error::Conflict(_)), Ok(result)) => {
                    result
                }
                _ => panic!("exactly one finish must succeed"),
            }
        });
        assert!(matches!(
            session.mutate(|| panic!("mutation after finish")),
            Err::<(), _>(Error::Conflict(_))
        ));
        fs::write(&config.documents[0].source_path, "\n\n\nOriginal line.").unwrap();
        assert!(result.output.contains("1 open comment in 1 document"));
        assert!(result.output.contains("## a1 · line 3 · First"));
    }
}
