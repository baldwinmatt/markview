use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};
use std::time::SystemTime;

use markview::annotations::{self, Error, Location, Review, Status, TextProjection};

use super::{format_annotations, ServeConfig, ServedDocument};

pub const ENDPOINT: &str = "/_markview/review";
const CHANGED: Error =
    Error::Conflict("The collection changed. Review the latest page and confirm again.");
const FINISHED_MESSAGE: &str = "Review finished. You can close this tab.";
const FINISHED: Error = Error::Conflict(FINISHED_MESSAGE);

/// Whether `error` is the refusal every write gets once a verdict has been
/// accepted. Its response says so in a field of its own, so the page does
/// not have to recognise the message.
pub fn is_finished(error: &Error) -> bool {
    matches!(error, Error::Conflict(message) if *message == FINISHED_MESSAGE)
}

/// Why one served document cannot be reviewed.
///
/// The page gets `public`: the document as its route names it, and the cause
/// in Markview's own words. That text never holds an absolute path or an
/// operating-system message. `Display` is for stderr, where whoever ran the
/// command can use the full path and the error as it was reported.
#[derive(Debug)]
pub struct DocumentProblem {
    pub public: String,
    pub status: &'static str,
    detail: String,
}

impl std::fmt::Display for DocumentProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.detail)
    }
}

impl std::error::Error for DocumentProblem {}

impl DocumentProblem {
    /// `error` as a problem with `doc`. `what` is the file that failed, "document"
    /// or "annotation store", and `path` is where it is.
    pub fn wrap(
        config: &ServeConfig,
        doc: &ServedDocument,
        what: &str,
        path: &Path,
        error: Error,
        status: &'static str,
    ) -> Error {
        let cause = match &error {
            Error::Io(error) => match error.kind() {
                io::ErrorKind::InvalidData => format!("the {what} is not valid UTF-8"),
                io::ErrorKind::NotFound => format!("the {what} is missing"),
                io::ErrorKind::FileTooLarge => format!("the {what} is too large"),
                io::ErrorKind::PermissionDenied => {
                    format!("the {what} cannot be read: permission denied")
                }
                _ => format!("the {what} cannot be read"),
            },
            // A fixed message of Markview's, or one that adds an annotation ID.
            error => error.to_string(),
        };
        // The path under the served root, which is also what the route shows.
        let name = doc
            .source_path
            .strip_prefix(&config.root)
            .ok()
            .or_else(|| doc.source_path.file_name().map(Path::new))
            .unwrap_or(Path::new("document"));
        Error::Io(io::Error::other(Self {
            public: format!("{}: {cause}", name.display()),
            status,
            detail: format!("{}: {error}", path.display()),
        }))
    }

    /// The problem `error` carries, if it is one.
    pub fn of(error: &Error) -> Option<&Self> {
        match error {
            Error::Io(error) => error.get_ref()?.downcast_ref(),
            _ => None,
        }
    }
}

/// A collection that cannot be read is the server's failure to answer.
const UNREADABLE: &str = "500 Internal Server Error";

/// What identifies one version of a file without reading it. A rename into
/// place (how Markview and most editors save) changes the inode; an in-place
/// write changes the size or a timestamp.
#[derive(Debug, PartialEq, Eq)]
struct FileKey(Option<(u64, Option<SystemTime>, [i64; 4])>);

impl FileKey {
    fn of(path: &Path) -> Self {
        Self(fs::symlink_metadata(path).ok().map(|meta| {
            #[cfg(unix)]
            let identity = {
                use std::os::unix::fs::MetadataExt;
                [
                    meta.dev() as i64,
                    meta.ino() as i64,
                    meta.ctime(),
                    meta.ctime_nsec(),
                ]
            };
            #[cfg(not(unix))]
            let identity = [0; 4];
            (meta.len(), meta.modified().ok(), identity)
        }))
    }
}

/// One served document as the verdict bar counts it.
#[derive(Debug)]
struct DocumentCount {
    source: FileKey,
    store_path: PathBuf,
    store: FileKey,
    revisions: Revisions,
    open: usize,
}

impl DocumentCount {
    fn read(config: &ServeConfig, doc: &ServedDocument) -> Result<Self, Error> {
        let location = locate(config, doc)?;
        // Taken before the bytes are read: a write that lands in between
        // leaves a key older than the content, so the next summary reads it
        // again instead of trusting a count it may not match.
        let source = FileKey::of(&location.document);
        let store = FileKey::of(&location.path);
        let (text, bytes) = read_located(config, doc, &location)?;
        // Whether a comment is open does not depend on where it sits, so
        // counting builds no projection and locates nothing.
        let open = annotations::open_count(&location, bytes.as_deref())
            .map_err(|error| store_problem(config, doc, &location, error))?;
        Ok(Self {
            source,
            store,
            revisions: (
                annotations::source_revision(&text),
                annotations::store_revision(&location.path, bytes.as_deref()),
            ),
            open,
            store_path: location.path,
        })
    }

    fn current(&self, doc: &ServedDocument) -> bool {
        self.source == FileKey::of(&doc.source_path) && self.store == FileKey::of(&self.store_path)
    }
}

#[derive(Default)]
struct Stale {
    all: bool,
    documents: Vec<PathBuf>,
}

/// The collection summary of one `ServeConfig`: each document's revisions and
/// open count, keyed by its path and by the `FileKey`s of its source and
/// store. A summary stats every document and reads only those whose keys
/// moved. Three things drop entries regardless of the keys: the store watcher
/// (a store that changed or moved to another root), a finish that found the
/// summary out of date, and a rescan, which publishes a config with an empty
/// cache.
#[derive(Default)]
pub struct SummaryCache {
    /// Held while a summary is computed, so concurrent pollers share one
    /// read of a changed document. Nothing else waits on it.
    documents: Mutex<HashMap<PathBuf, DocumentCount>>,
    stale: Mutex<Stale>,
    #[cfg(test)]
    reads: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    hold: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl std::fmt::Debug for SummaryCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SummaryCache").finish_non_exhaustive()
    }
}

impl SummaryCache {
    /// The review revision, the open comments and the documents that have
    /// any. The revision is the one `Snapshot::capture` computes.
    fn read(&self, config: &ServeConfig) -> Result<(String, usize, usize), Error> {
        let mut documents = self.documents.lock().expect("summary lock");
        self.before_summary();
        let stale = std::mem::take(&mut *self.stale.lock().expect("stale lock"));
        if stale.all {
            documents.clear();
        }
        for document in &stale.documents {
            documents.remove(document);
        }
        let mut revisions = Vec::with_capacity(config.documents.len());
        let (mut comments, mut with_comments) = (0, 0);
        for doc in &config.documents {
            if !documents
                .get(&doc.source_path)
                .is_some_and(|cached| cached.current(doc))
            {
                documents.remove(&doc.source_path);
                let count = DocumentCount::read(config, doc)?;
                documents.insert(doc.source_path.clone(), count);
            }
            let count = &documents[&doc.source_path];
            revisions.push(Ok(count.revisions.clone()));
            comments += count.open;
            with_comments += usize::from(count.open > 0);
        }
        let revision = collection_revision(config, revisions.into_iter())?;
        Ok((revision, comments, with_comments))
    }

    /// Forgets one document: its store changed, or now lives elsewhere.
    pub fn invalidate(&self, document: &Path) {
        if let Ok(mut stale) = self.stale.lock() {
            stale.documents.push(document.to_owned());
        }
    }

    pub fn invalidate_all(&self) {
        if let Ok(mut stale) = self.stale.lock() {
            *stale = Stale {
                all: true,
                documents: Vec::new(),
            };
        }
    }

    fn note_read(&self) {
        #[cfg(test)]
        self.reads
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    fn before_summary(&self) {
        #[cfg(test)]
        if let Some(hold) = self.hold.lock().expect("hold lock").take() {
            hold();
        }
    }

    /// How many documents summaries and snapshots have read from disk.
    #[cfg(test)]
    pub fn reads(&self) -> usize {
        self.reads.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Runs `hold` once, inside the next summary, before it reads anything.
    #[cfg(test)]
    pub fn hold_next(&self, hold: impl FnOnce() + Send + 'static) {
        *self.hold.lock().expect("hold lock") = Some(Box::new(hold));
    }
}

// How many text projections this thread has built for review snapshots.
#[cfg(test)]
thread_local! {
    static PROJECTIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn project(source: &str) -> TextProjection {
    #[cfg(test)]
    PROJECTIONS.set(PROJECTIONS.get() + 1);
    TextProjection::new(source)
}

#[derive(Debug)]
pub struct Session {
    accepted: Mutex<bool>,
    result: mpsc::Sender<io::Result<AcceptedReview>>,
}

pub struct AcceptedReview {
    pub output: String,
    pub changes: bool,
}

impl Session {
    pub fn new() -> (Arc<Self>, mpsc::Receiver<io::Result<AcceptedReview>>) {
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

    /// The counts the verdict bar shows. Only the finished flag is read
    /// under the session lock: a save must not wait for a summary, and the
    /// finish handler captures its own fresh snapshot.
    pub fn summary(&self, config: &ServeConfig) -> Result<String, Error> {
        if self.finished() {
            return Ok(r#"{"finished":true}"#.to_owned());
        }
        let (revision, comments, documents) = config.review_summary.read(config)?;
        Ok(format!(
            r#"{{"review_revision":"{revision}","comments":{comments},"documents":{documents}}}"#
        ))
    }

    /// Accepts a verdict for exactly the collection the reviewer confirmed.
    /// `published` says whether `config` is still the served collection: the
    /// caller does not hold the config lock across the capture, so a rescan
    /// that was published meanwhile is a conflict.
    pub fn finish(
        &self,
        config: &ServeConfig,
        body: &[u8],
        published: impl FnOnce() -> bool,
    ) -> Result<AcceptedReview, Error> {
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
        // The same check a page's annotations go through: a route that is
        // not on the allowlist, or whose file is no longer the one that was
        // allowlisted, is not a page a verdict can be given from.
        let page = config
            .document_for_route(&doc)
            .filter(|page| config.served_location(page).is_ok())
            .ok_or(CHANGED)?;
        let snapshot = Snapshot::capture(config)?;
        if snapshot.revision != wanted_revision
            || !snapshot.documents.iter().any(|doc| {
                doc.path == page.source_path && doc.review.source_revision == source_revision
            })
        {
            // The reviewer confirmed counts that this fresh capture does
            // not match. Whatever the cache missed, the retry reads anew.
            config.review_summary.invalidate_all();
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
            || !published()
        {
            return Err(CHANGED);
        }
        *accepted = true;
        Ok(AcceptedReview { output, changes })
    }

    pub fn publish(&self, result: AcceptedReview) {
        let _ = self.result.send(Ok(result));
    }

    /// Ends the wait without a verdict: the server can no longer be reached.
    pub fn fail(&self, error: io::Error) {
        let _ = self.result.send(Err(error));
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
                let review = Review::from_snapshot(&location, bytes.as_deref(), &project(&source))
                    .map_err(|error| store_problem(config, doc, &location, error))?;
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

fn store_problem(
    config: &ServeConfig,
    doc: &ServedDocument,
    location: &Location,
    error: Error,
) -> Error {
    DocumentProblem::wrap(
        config,
        doc,
        "annotation store",
        &location.path,
        error,
        UNREADABLE,
    )
}

fn locate(config: &ServeConfig, doc: &ServedDocument) -> Result<Location, Error> {
    config.served_location(doc).map_err(|error| {
        DocumentProblem::wrap(
            config,
            doc,
            "document",
            &doc.source_path,
            error.into(),
            UNREADABLE,
        )
    })
}

fn read_located(
    config: &ServeConfig,
    doc: &ServedDocument,
    location: &Location,
) -> Result<(String, Option<Vec<u8>>), Error> {
    config.review_summary.note_read();
    let source = annotations::read_source(location).map_err(|error| {
        DocumentProblem::wrap(
            config,
            doc,
            "document",
            &location.document,
            error.into(),
            UNREADABLE,
        )
    })?;
    let store = annotations::snapshot(location)
        .map_err(|error| store_problem(config, doc, location, error.into()))?;
    Ok((source, store))
}

fn read_document(
    config: &ServeConfig,
    doc: &ServedDocument,
) -> Result<(Location, String, Option<Vec<u8>>), Error> {
    let location = locate(config, doc)?;
    let (source, store) = read_located(config, doc, &location)?;
    Ok((location, source, store))
}

fn document_revisions(config: &ServeConfig, doc: &ServedDocument) -> Result<Revisions, Error> {
    let (location, source, store) = read_document(config, doc)?;
    Ok((
        annotations::source_revision(&source),
        annotations::store_revision(&location.path, store.as_deref()),
    ))
}

/// The review revision: one digest over the default page, the served routes
/// and paths in served order, and each document's source and store revision.
///
/// It is opaque and lives only inside one process: a page reads it from the
/// summary and hands it back with the verdict. It still uses the module's
/// FNV-1a digest, like every other revision, instead of `DefaultHasher`,
/// whose output Rust does not promise to keep.
fn collection_revision(
    config: &ServeConfig,
    revisions: impl Iterator<Item = Result<Revisions, Error>>,
) -> Result<String, Error> {
    let mut fields = vec![
        config
            .default_document
            .as_os_str()
            .as_encoded_bytes()
            .to_vec(),
        config.documents.len().to_string().into_bytes(),
    ];
    for (doc, revisions) in config.documents.iter().zip(revisions) {
        let (source, store) = revisions?;
        fields.extend([
            doc.route_path.clone().into_bytes(),
            doc.source_path.as_os_str().as_encoded_bytes().to_vec(),
            source.into_bytes(),
            store.into_bytes(),
        ]);
    }
    // No field can hold a NUL (a path, a route, digits), so one after each
    // keeps two different lists of fields from spelling the same bytes.
    let parts: Vec<&[u8]> = fields
        .iter()
        .flat_map(|field| [field.as_slice(), &[0][..]])
        .collect();
    Ok(annotations::revision(&parts))
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn summary_json(revision: &str, comments: usize, documents: usize) -> String {
        format!(
            r#"{{"review_revision":"{revision}","comments":{comments},"documents":{documents}}}"#
        )
    }

    #[test]
    fn repeated_summaries_read_each_document_once_and_build_no_projection() {
        let fixture = Fixture::new();
        Fixture::write_store(&fixture.nested, "## a1 [open] line 1\n\n> Next\n\nNested\n");
        let config = fixture.config(true);
        let (session, _) = Session::new();
        // The revision the finish handler will compare against.
        let revision = Snapshot::capture(&config).unwrap().revision;
        let (reads, projections) = (config.review_summary.reads(), PROJECTIONS.get());
        // The bar asks again on every page load and every `annotations`
        // event. Unchanged documents are neither read nor parsed again.
        for _ in 0..6 {
            assert_eq!(
                session.summary(&config).unwrap(),
                summary_json(&revision, 1, 1)
            );
        }
        assert_eq!(
            config.review_summary.reads() - reads,
            config.documents.len()
        );
        assert_eq!(
            PROJECTIONS.get(),
            projections,
            "counting needs no projection"
        );

        // A change is seen at once, and only that document is read again.
        let reads = config.review_summary.reads();
        Fixture::write_store(
            &fixture.first,
            "## a1 [open] document\n\nOne\n\n## a2 [resolved] document\n\nDone\n",
        );
        let changed = session.summary(&config).unwrap();
        assert_eq!(config.review_summary.reads() - reads, 1);
        fs::write(&fixture.nested, "# Next, edited\n").unwrap();
        let edited = session.summary(&config).unwrap();
        assert_eq!(config.review_summary.reads() - reads, 2);
        assert_ne!(changed, edited);
        let snapshot = Snapshot::capture(&config).unwrap();
        assert_eq!(edited, summary_json(&snapshot.revision, 2, 2));
        assert_eq!(snapshot.counts(), (2, 2));
    }

    #[test]
    fn invalidated_documents_are_read_again_even_when_their_files_look_unchanged() {
        let fixture = Fixture::new();
        let config = fixture.config(true);
        let (session, _) = Session::new();
        let first = session.summary(&config).unwrap();
        let reads = config.review_summary.reads();
        // The store watcher saw this document's store change or move.
        config
            .review_summary
            .invalidate(&config.documents[0].source_path);
        assert_eq!(session.summary(&config).unwrap(), first);
        assert_eq!(config.review_summary.reads() - reads, 1);
        config.review_summary.invalidate_all();
        assert_eq!(session.summary(&config).unwrap(), first);
        assert_eq!(
            config.review_summary.reads() - reads,
            1 + config.documents.len()
        );
    }

    #[test]
    fn a_summary_in_progress_does_not_hold_up_mutations() {
        let fixture = Fixture::new();
        let config = fixture.config(true);
        let (session, _) = Session::new();
        let (entered, held) = mpsc::channel();
        let (release, proceed) = mpsc::channel::<()>();
        config.review_summary.hold_next(move || {
            entered.send(()).unwrap();
            let _ = proceed.recv();
        });
        std::thread::scope(|scope| {
            let summary = scope.spawn(|| session.summary(&config));
            held.recv().unwrap();
            let (done, admitted) = mpsc::channel();
            let session = &session;
            let mutation = scope.spawn(move || {
                session.mutate(|| Ok(())).unwrap();
                done.send(()).unwrap();
            });
            // A save must be admitted while the summary is still reading.
            let admitted = admitted.recv_timeout(std::time::Duration::from_secs(5));
            release.send(()).unwrap();
            mutation.join().unwrap();
            summary.join().unwrap().unwrap();
            assert!(admitted.is_ok(), "the save queued behind the summary");
        });
    }

    #[test]
    fn review_revision_follows_the_collection_and_nothing_else() {
        let fixture = Fixture::new();
        let explicit = |files: &[&PathBuf]| {
            let files = files.iter().map(|file| (*file).clone()).collect();
            let config = ServeConfig::from_inputs(files, 0, false).unwrap();
            Snapshot::capture(&config).unwrap().revision
        };
        let both = explicit(&[&fixture.first, &fixture.nested]);
        // A digest in the form of every other revision, the same each time.
        assert_eq!(both.len(), 16);
        assert!(both
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')));
        assert_eq!(both, explicit(&[&fixture.first, &fixture.nested]));
        // Which documents are served, in which order, and which is the
        // default page are all part of what the reviewer confirmed.
        let reversed = explicit(&[&fixture.nested, &fixture.first]);
        let one = explicit(&[&fixture.first]);
        assert_ne!(both, reversed);
        assert_ne!(both, one);
        assert_ne!(reversed, one);
        // So is every document's source and store.
        fs::write(&fixture.nested, "# Next, edited\n").unwrap();
        let edited = explicit(&[&fixture.first, &fixture.nested]);
        assert_ne!(both, edited);
        Fixture::write_store(&fixture.first, "## a1 [open] document\n\nNew\n");
        assert_ne!(edited, explicit(&[&fixture.first, &fixture.nested]));
    }

    #[test]
    fn review_conflicts_leave_session_open() {
        let fixture = Fixture::new();
        let config = fixture.config(false);
        let (session, _) = Session::new();
        let changes = Fixture::finish_form(&config, "changes");
        assert!(matches!(
            session.finish(&config, changes.as_bytes(), || true),
            Err(Error::Invalid(_))
        ));

        let old = Fixture::finish_form(&config, "approved");
        // A rescan published a new collection while this one was captured.
        assert!(matches!(
            session.finish(&config, old.as_bytes(), || false),
            Err(Error::Conflict(_))
        ));
        assert!(matches!(
            session.finish(&fixture.config(true), old.as_bytes(), || true),
            Err(Error::Conflict(_))
        ));
        assert!(matches!(
            session.finish(
                &config,
                old.replace("doc=/first.md", "doc=/outside.md").as_bytes(),
                || true
            ),
            Err(Error::Conflict(_))
        ));
        Fixture::write_store(&fixture.first, "## a1 [open] document\n\nNew comment\n");
        assert!(matches!(
            session.finish(&config, old.as_bytes(), || true),
            Err(Error::Conflict(_))
        ));
        let old = Fixture::finish_form(&config, "approved");
        fs::write(&fixture.first, "# Edited\n").unwrap();
        assert!(matches!(
            session.finish(&config, old.as_bytes(), || true),
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
            let pending_finish = scope.spawn(|| session.finish(&config, old.as_bytes(), || true));
            release.send(()).unwrap();
            assert!(matches!(
                pending_finish.join().unwrap(),
                Err(Error::Conflict(_))
            ));
        });
        let form = Fixture::finish_form(&config, "changes").replace("doc=/first.md", "doc=/");
        let result = std::thread::scope(|scope| {
            let other = scope.spawn(|| session.finish(&config, form.as_bytes(), || true));
            match (
                session.finish(&config, form.as_bytes(), || true),
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
