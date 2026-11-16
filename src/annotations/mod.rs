#[cfg(unix)]
mod paths;
#[cfg(not(unix))]
#[path = "unsupported.rs"]
mod paths;
mod projection;
mod store;
#[cfg(all(test, unix))]
mod unsupported;
mod watch;

pub use paths::{
    read_canonical_source, read_canonical_source_limited, read_source, read_source_limited,
    resolve, Location,
};
pub use projection::{collapse, TextProjection};
pub use store::{Annotation, Status, Store};
pub use watch::StoreWatcher;

use std::collections::HashMap;
use std::fmt::{self, Write as _};
use std::fs;
use std::io::{self, Read};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

pub const REVIEW_SCRIPT: &str = include_str!("review.js");
pub const REVIEW_STYLE: &str = include_str!("review.css");
/// The verdict bar of `markview --review`, loaded after the review script.
pub const VERDICT_SCRIPT: &str = include_str!("verdict.js");
pub const COPY_SCRIPT: &str = include_str!("copy.js");
pub const COPY_STYLE: &str = include_str!("copy.css");
pub const FORM_CAP: usize = 160 * 1024;

#[derive(Debug)]
pub enum Error {
    Invalid(&'static str),
    Conflict(&'static str),
    /// One annotation, by ID, that stops the store from being written.
    Section(u64, &'static str),
    Io(io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(message) | Self::Conflict(message) => f.write_str(message),
            Self::Section(id, problem) => write!(f, "annotation a{id} {problem}"),
            Self::Io(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

pub fn random_hex() -> io::Result<String> {
    let mut bytes = [0; 24];
    fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes
        .iter()
        .fold(String::with_capacity(48), |mut result, byte| {
            let _ = write!(result, "{byte:02x}");
            result
        }))
}

/// Revisions are FNV-1a/64 digests, printed as 16 lowercase hex digits.
///
/// `source_revision` and `start` are persisted in git-tracked store files and
/// trusted by `locate`, so the algorithm is fixed here instead of borrowing
/// `std::hash::DefaultHasher`, which may change between Rust releases.
///
/// The parts are hashed as one run of bytes. A caller whose parts could be
/// split differently and still spell the same bytes separates them itself.
pub fn revision(parts: &[&[u8]]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in parts.iter().flat_map(|part| part.iter()) {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Hashes the exact source bytes, before mojibake repair or link rewriting.
pub fn source_revision(source: &str) -> String {
    revision(&[source.as_bytes()])
}

/// Hashes, in order: the store path's bytes (the raw `OsStr` bytes on Unix),
/// a `0x00` separator (a path cannot contain NUL), a presence byte (`0x00`
/// for a missing file, `0x01` for an existing one), then the exact file bytes.
/// A missing file and an empty file therefore have different revisions.
pub fn store_revision(path: &Path, bytes: Option<&[u8]>) -> String {
    revision(&[
        path.as_os_str().as_encoded_bytes(),
        &[0, u8::from(bytes.is_some())],
        bytes.unwrap_or_default(),
    ])
}

pub fn annotation_store_root(document: &Path) -> io::Result<PathBuf> {
    Ok(resolve(document)?.root)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedAnnotation {
    pub id: u64,
    pub status: Status,
    pub body: String,
    pub quote: Option<String>,
    pub lines: Option<(usize, usize)>,
    pub section: Option<String>,
    pub attached: bool,
    pub range: Option<Range<usize>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Review {
    pub source_revision: String,
    pub store_revision: String,
    pub annotations: Vec<ResolvedAnnotation>,
}

impl Review {
    pub fn from_snapshot(
        location: &Location,
        bytes: Option<&[u8]>,
        projection: &TextProjection,
    ) -> Result<Self, Error> {
        let store = parse_snapshot(location, bytes)?;
        // Two sections with one ID cannot be told apart in a list either.
        store.check_ids()?;
        let ranges = store
            .annotations
            .iter()
            .map(|record| projection.locate(record))
            .collect();
        Ok(Self::resolved(
            &store.annotations,
            ranges,
            projection,
            store_revision(&location.path, bytes),
        ))
    }

    /// Builds the review from records and where each one was located.
    /// Locating is the expensive part, so callers do it once and pass it in.
    fn resolved(
        records: &[Annotation],
        ranges: Vec<Option<Range<usize>>>,
        projection: &TextProjection,
        store_revision: String,
    ) -> Self {
        let mut annotations: Vec<_> = records
            .iter()
            .zip(ranges)
            .map(|(record, range)| ResolvedAnnotation {
                id: record.id,
                status: record.status,
                body: record.body.clone(),
                quote: record.quote.clone(),
                lines: range
                    .clone()
                    .map(|range| projection.lines(range))
                    .or(record.lines),
                section: range
                    .as_ref()
                    .map(|range| projection.section(range.start).to_owned())
                    .or_else(|| record.metadata.get("section").cloned())
                    .filter(|section| !section.is_empty()),
                attached: record.lines.is_none() || range.is_some(),
                range,
            })
            .collect();
        // Open before resolved. Within each: text comments in document
        // order, then whole-document comments, then detached ones, each of
        // the last two in the order the store lists them.
        annotations.sort_by_key(|record| {
            let placement = match (&record.range, record.attached) {
                (Some(_), _) => 0,
                (None, true) => 1,
                (None, false) => 2,
            };
            (
                record.status == Status::Resolved,
                placement,
                record.range.as_ref().map_or(0, |range| range.start),
            )
        });
        Self {
            source_revision: projection.revision.clone(),
            store_revision,
            annotations,
        }
    }

    pub fn json(&self) -> String {
        let mut output = format!(
            "{{\"source_revision\":{},\"store_revision\":{},\"annotations\":[",
            json_string(&self.source_revision),
            json_string(&self.store_revision)
        );
        for (index, record) in self.annotations.iter().enumerate() {
            if index != 0 {
                output.push(',');
            }
            let lines = record.lines.map_or("null".to_owned(), |(first, last)| {
                format!("[{first},{last}]")
            });
            let range = record.range.as_ref().map_or("null".to_owned(), |range| {
                format!("[{},{}]", range.start, range.end)
            });
            let _ = write!(output, "{{\"id\":\"a{}\",\"status\":\"{}\",\"body\":{},\"quote\":{},\"lines\":{lines},\"section\":{},\"attached\":{},\"range\":{range}}}",
                record.id, record.status.label(), json_string(&record.body), json_optional(record.quote.as_deref()), json_optional(record.section.as_deref()), record.attached);
        }
        output.push_str("]}");
        output
    }
}

fn json_optional(value: Option<&str>) -> String {
    value.map_or("null".to_owned(), json_string)
}

pub fn json_string(value: &str) -> String {
    let mut output = String::with_capacity(value.len() + 2);
    push_json_string(&mut output, value);
    output
}

/// Appends `value` to `output` as a JSON string, escaped for embedding in
/// HTML as well.
pub fn push_json_string(output: &mut String, value: &str) {
    output.push('"');
    for ch in value.chars() {
        match ch {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '<' => output.push_str("\\u003c"),
            '>' => output.push_str("\\u003e"),
            '&' => output.push_str("\\u0026"),
            ch if ch <= '\u{1f}' || matches!(ch, '\u{2028}' | '\u{2029}') => {
                let _ = write!(output, "\\u{:04x}", ch as u32);
            }
            _ => output.push(ch),
        }
    }
    output.push('"');
}

fn parse_snapshot(location: &Location, bytes: Option<&[u8]>) -> Result<Store, Error> {
    match bytes {
        Some(bytes) => {
            Ok(Store::parse(std::str::from_utf8(bytes).map_err(|_| {
                Error::Invalid("annotation store is not UTF-8")
            })?))
        }
        None => Ok(Store::new(Store::generated_preamble(&location.relative))),
    }
}

/// How many documents a `ReviewCache` remembers. A projection holds several
/// words per character of the document, so the cache is kept small.
const CACHED_DOCUMENTS: usize = 8;

struct CachedDocument {
    document: PathBuf,
    projection: Arc<TextProjection>,
    /// The review resolved against `projection`, and its store revision.
    review: Option<Review>,
}

/// Remembers the projection and resolved review of recently used documents,
/// keyed by document path, source revision and store revision. Building a
/// projection parses the whole document and resolving searches it for every
/// quote, while most reads repeat the revisions of the one before.
#[derive(Clone, Default)]
pub struct ReviewCache(Arc<CacheState>);

#[derive(Default)]
struct CacheState {
    /// Most recently used first.
    documents: Mutex<Vec<CachedDocument>>,
    projections: AtomicUsize,
    resolutions: AtomicUsize,
    /// Held while a projection is built, so that concurrent requests for one
    /// cold revision wait for the first build instead of repeating it.
    building: Mutex<()>,
}

impl fmt::Debug for ReviewCache {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReviewCache")
            .field("work", &self.work())
            .finish()
    }
}

/// A cache holds no state of its own, so it never makes two owners differ.
impl PartialEq for ReviewCache {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for ReviewCache {}

impl ReviewCache {
    /// How many projections this cache has built and how many reviews it has
    /// resolved (searched a projection for every quote) since it was made.
    /// Both stay flat while callers repeat the same revisions.
    pub fn work(&self) -> (usize, usize) {
        (
            self.0.projections.load(Ordering::Relaxed),
            self.0.resolutions.load(Ordering::Relaxed),
        )
    }

    fn lookup<T>(
        &self,
        location: &Location,
        revision: &str,
        take: impl FnOnce(&CachedDocument) -> Option<T>,
    ) -> Option<T> {
        let documents = self.0.documents.lock().ok()?;
        let cached = documents.iter().find(|cached| {
            cached.document == location.document && cached.projection.revision == revision
        })?;
        take(cached)
    }

    fn remember(
        &self,
        location: &Location,
        projection: &Arc<TextProjection>,
        review: Option<Review>,
    ) {
        let Ok(mut documents) = self.0.documents.lock() else {
            return;
        };
        documents.retain(|cached| cached.document != location.document);
        documents.insert(
            0,
            CachedDocument {
                document: location.document.clone(),
                projection: projection.clone(),
                review,
            },
        );
        documents.truncate(CACHED_DOCUMENTS);
    }

    /// The projection of `source`, built only if its revision is not cached.
    /// Concurrent callers share one build.
    pub fn projection(&self, location: &Location, source: &str) -> Arc<TextProjection> {
        let revision = source_revision(source);
        let cached = || {
            self.lookup(location, &revision, |cached| {
                Some(cached.projection.clone())
            })
        };
        if let Some(projection) = cached() {
            return projection;
        }
        let _building = self
            .0
            .building
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        cached().unwrap_or_else(|| {
            self.0.projections.fetch_add(1, Ordering::Relaxed);
            let projection = Arc::new(TextProjection::new(source));
            self.remember(location, &projection, None);
            projection
        })
    }

    /// Resolves the annotations of `location` against `source`, which may
    /// differ from the file on disk (the GUI previews unsaved edits).
    pub fn read(&self, location: &Location, source: &str) -> Result<Review, Error> {
        let projection = self.projection(location, source);
        let bytes = snapshot(location)?;
        let revision = store_revision(&location.path, bytes.as_deref());
        let cached = self.lookup(location, &projection.revision, |cached| {
            cached
                .review
                .clone()
                .filter(|review| review.store_revision == revision)
        });
        if let Some(review) = cached {
            return Ok(review);
        }
        self.0.resolutions.fetch_add(1, Ordering::Relaxed);
        let review = Review::from_snapshot(location, bytes.as_deref(), &projection)?;
        self.remember(location, &projection, Some(review.clone()));
        Ok(review)
    }

    /// Applies `operation` and remembers the result, so the read that
    /// follows a write is already answered.
    pub fn mutate(&self, location: &Location, operation: Operation) -> Result<Review, Error> {
        let (projection, review) = mutate_with(location, operation, &mut |source| {
            self.projection(location, source)
        })?;
        self.0.resolutions.fetch_add(1, Ordering::Relaxed);
        self.remember(location, &projection, Some(review.clone()));
        Ok(review)
    }
}

/// How many annotations in a store snapshot are open. Whether a comment is
/// open does not depend on where it sits, so this needs no projection. It
/// fails for exactly the stores `Review::from_snapshot` rejects.
pub fn open_count(location: &Location, bytes: Option<&[u8]>) -> Result<usize, Error> {
    let store = parse_snapshot(location, bytes)?;
    store.check_ids()?;
    Ok(store
        .annotations
        .iter()
        .filter(|record| record.status == Status::Open)
        .count())
}

pub fn read(location: &Location, projection: &TextProjection) -> Result<Review, Error> {
    let bytes = snapshot(location)?;
    Review::from_snapshot(location, bytes.as_deref(), projection)
}

/// A problem in a store file that does not stop it from loading but probably
/// hides annotations: a header that almost parses is silently read as comment
/// text, so the annotation it was meant to start disappears.
#[derive(Debug, PartialEq, Eq)]
pub struct StoreWarning {
    /// The 1-based line in the store file, when the problem is one line.
    pub line: Option<usize>,
    pub message: String,
}

pub fn store_warnings(location: &Location, bytes: &[u8]) -> Vec<StoreWarning> {
    let Ok(source) = std::str::from_utf8(bytes) else {
        return Vec::new();
    };
    let mut warnings: Vec<_> = Store::near_headers(source)
        .map(|(line, text)| StoreWarning {
            line: Some(line),
            message: format!(
                "{text:?} is not an annotation header; write it exactly as \"## a<N> [open|resolved] <anchor>\""
            ),
        })
        .collect();
    if warnings.is_empty()
        && Store::parse(source).annotations.is_empty()
        && !source.trim().is_empty()
        && source != Store::generated_preamble(&location.relative)
    {
        warnings.push(StoreWarning {
            line: None,
            message: "the store has content but no annotation sections; a section starts with a line like \"## a1 [open] document\"".to_owned(),
        });
    }
    warnings
}

pub fn snapshot(location: &Location) -> io::Result<Option<Vec<u8>>> {
    location
        .parent(false)?
        .map_or(Ok(None), |parent| parent.read())
}

#[derive(Debug)]
pub struct Request {
    pub doc: Option<String>,
    pub operation: Operation,
}

#[derive(Debug)]
pub struct Selection {
    quote: String,
    range: Range<usize>,
    revision: String,
}

impl Selection {
    pub fn revision(&self) -> &str {
        &self.revision
    }
}

#[derive(Debug)]
pub enum Operation {
    Add {
        body: String,
        selection: Option<Selection>,
        document_revision: Option<String>,
    },
    Change {
        id: u64,
        revision: String,
        change: Change,
    },
}

#[derive(Debug)]
pub enum Change {
    Edit(String),
    Status(Status),
    Delete,
}

fn decode_form(value: &[u8]) -> Result<String, Error> {
    let mut bytes = Vec::with_capacity(value.len());
    let mut input = value.iter().copied();
    while let Some(byte) = input.next() {
        bytes.push(match byte {
            b'+' => b' ',
            b'%' => {
                let hi = input.next().and_then(|ch| (ch as char).to_digit(16));
                let lo = input.next().and_then(|ch| (ch as char).to_digit(16));
                match (hi, lo) {
                    (Some(hi), Some(lo)) => (hi * 16 + lo) as u8,
                    _ => return Err(Error::Invalid("invalid form encoding")),
                }
            }
            _ => byte,
        });
    }
    let value = String::from_utf8(bytes).map_err(|_| Error::Invalid("invalid form UTF-8"))?;
    if value.contains('\0') {
        return Err(Error::Invalid("null byte in form"));
    }
    Ok(value)
}

pub fn parse_form(bytes: &[u8]) -> Result<HashMap<String, String>, Error> {
    if bytes.len() > FORM_CAP {
        return Err(Error::Invalid("annotation form too large"));
    }
    let mut overhead = bytes.len();
    let mut fields = HashMap::new();
    for field in bytes.split(|byte| *byte == b'&') {
        let split = field
            .iter()
            .position(|byte| *byte == b'=')
            .ok_or(Error::Invalid("invalid form field"))?;
        let key = decode_form(&field[..split])?;
        let value = &field[split + 1..];
        let limit = match key.as_str() {
            "quote" => 1_000,
            "body" => 10_000,
            _ => 16 * 1024,
        };
        if matches!(key.as_str(), "quote" | "body") {
            overhead -= value.len();
        }
        let value = decode_form(value)?;
        if value.chars().count() > limit || fields.insert(key, value).is_some() {
            return Err(Error::Invalid("duplicate or oversized annotation field"));
        }
    }
    if overhead > 16 * 1024 {
        return Err(Error::Invalid("annotation form overhead too large"));
    }
    Ok(fields)
}

impl Request {
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        let mut fields = parse_form(bytes)?;
        let invalid = Error::Invalid("missing or invalid annotation fields");
        let doc = fields.remove("doc");
        let operation = match fields.remove("op").as_deref() {
            Some("add") => {
                let body = take_body(&mut fields)?;
                let (selection, document_revision) = match (
                    fields.remove("quote"),
                    fields.remove("start"),
                    fields.remove("end"),
                    fields.remove("source_revision"),
                ) {
                    (None, None, None, revision) => (None, revision),
                    (Some(quote), Some(start), Some(end), Some(revision)) => {
                        let start =
                            decimal(&start).ok_or(Error::Invalid("invalid selection start"))?;
                        let end = decimal(&end).ok_or(Error::Invalid("invalid selection end"))?;
                        (
                            Some(Selection {
                                quote,
                                range: start..end,
                                revision,
                            }),
                            None,
                        )
                    }
                    _ => return Err(invalid),
                };
                Operation::Add {
                    body,
                    selection,
                    document_revision,
                }
            }
            Some(op @ ("edit" | "status" | "delete")) => {
                let id = fields
                    .remove("id")
                    .and_then(|value| decimal(value.strip_prefix('a')?))
                    .ok_or(Error::Invalid("invalid annotation ID"))?;
                let revision = fields
                    .remove("store_revision")
                    .ok_or(Error::Invalid("store revision required"))?;
                let change = match op {
                    "edit" => Change::Edit(take_body(&mut fields)?),
                    "status" => Change::Status(
                        fields
                            .remove("status")
                            .and_then(|value| Status::parse(&value))
                            .ok_or(Error::Invalid("invalid annotation status"))?,
                    ),
                    _ => Change::Delete,
                };
                Operation::Change {
                    id,
                    revision,
                    change,
                }
            }
            _ => return Err(Error::Invalid("invalid annotation operation")),
        };
        if !fields.is_empty() {
            return Err(invalid);
        }
        Ok(Self { doc, operation })
    }
}

fn decimal<T: std::str::FromStr>(value: &str) -> Option<T> {
    if value.bytes().all(|byte| byte.is_ascii_digit()) {
        value.parse().ok()
    } else {
        None
    }
}

fn take_body(fields: &mut HashMap<String, String>) -> Result<String, Error> {
    fields
        .remove("body")
        .map(|body| store::trim_body(&body).to_owned())
        .filter(|body| !body.is_empty())
        .ok_or(Error::Invalid("comment body is required"))
}

fn refresh(record: &mut Annotation, projection: &TextProjection, range: Range<usize>) {
    record.lines = Some(projection.lines(range.clone()));
    record.hint("section", projection.section(range.start).to_owned());
    record.hint("source_revision", projection.revision.clone());
    record.hint("start", range.start.to_string());
}

fn validate_selection(projection: &TextProjection, selection: &Selection) -> Result<String, Error> {
    if selection.revision != projection.revision {
        return Err(Error::Conflict("document changed, select the text again"));
    }
    if !projection.matches(
        selection.range.clone(),
        &projection::normalized(&selection.quote),
    ) {
        return Err(Error::Invalid("selected quote does not match the range"));
    }
    let quote = projection
        .quote(selection.range.clone())
        .expect("validated range");
    if quote.chars().count() > 1_000 {
        return Err(Error::Invalid("selected quote exceeds 1,000 characters"));
    }
    Ok(quote)
}

fn validate_document_revision(projection: &TextProjection, revision: &str) -> Result<(), Error> {
    if revision != projection.revision {
        return Err(Error::Conflict(
            "document changed, fetch the current revision",
        ));
    }
    Ok(())
}

pub fn mutate(location: &Location, operation: Operation) -> Result<Review, Error> {
    ReviewCache::default().mutate(location, operation)
}

// Runs once, between a mutation reading the store under its lock and
// committing: where an editor that takes no lock can still slip a write in.
#[cfg(test)]
thread_local! {
    static BEFORE_COMMIT: std::cell::Cell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::Cell::new(None) };
}

type Project<'a> = &'a mut dyn FnMut(&str) -> Arc<TextProjection>;

fn mutate_with(
    location: &Location,
    operation: Operation,
    project: Project<'_>,
) -> Result<(Arc<TextProjection>, Review), Error> {
    // Reject invalid or stale adds before creating directories, then validate
    // again against the source read under the lock.
    if let Operation::Add {
        selection,
        document_revision,
        ..
    } = &operation
    {
        if let Some(selection) = selection {
            validate_selection(&project(&read_source(location)?), selection)?;
        } else if let Some(revision) = document_revision {
            validate_document_revision(&project(&read_source(location)?), revision)?;
        }
    }
    let existing = snapshot(location)?;
    if matches!(&operation, Operation::Change { .. }) && existing.is_none() {
        return Err(Error::Conflict("annotation store changed on disk"));
    }
    let parent = location
        .parent(matches!(&operation, Operation::Add { .. }))?
        .ok_or(Error::Conflict("annotation store changed on disk"))?;
    let lock = parent.lock().inspect_err(|_| parent.discard(None))?;
    let result = mutate_locked(location, &parent, operation, project);
    if result.is_err() {
        parent.discard(Some(&lock));
    }
    result
}

fn mutate_locked(
    location: &Location,
    parent: &paths::Parent,
    operation: Operation,
    project: Project<'_>,
) -> Result<(Arc<TextProjection>, Review), Error> {
    if resolve(&location.document)?.path != location.path {
        return Err(Error::Conflict("annotation store moved"));
    }
    let bytes = parent.read()?;
    let projection = project(&read_source(location)?);
    let mut store = parse_snapshot(location, bytes.as_deref())?;
    store.check_ids()?;
    match operation {
        Operation::Add {
            body,
            selection,
            document_revision,
        } => {
            if let Some(revision) = document_revision {
                validate_document_revision(&projection, &revision)?;
            }
            let id = store
                .annotations
                .iter()
                .map(|record| record.id)
                .max()
                .unwrap_or(0)
                .checked_add(1)
                .ok_or(Error::Invalid("annotation IDs exhausted"))?;
            let mut record = Annotation::new(id, body);
            record.metadata.insert("created".to_owned(), utc_now());
            if let Some(selection) = selection {
                record.quote = Some(validate_selection(&projection, &selection)?);
                let (before, after) = projection.context(selection.range.clone());
                record.metadata.insert("before".to_owned(), before);
                record.metadata.insert("after".to_owned(), after);
                refresh(&mut record, &projection, selection.range);
            }
            store.annotations.push(record);
        }
        Operation::Change {
            id,
            revision,
            change,
        } => {
            if revision != store_revision(&location.path, bytes.as_deref()) {
                return Err(Error::Conflict("annotation store changed on disk"));
            }
            let index = store
                .annotations
                .iter()
                .position(|record| record.id == id)
                .ok_or(Error::Conflict("annotation no longer exists"))?;
            match change {
                Change::Edit(body) => store.annotations[index].body = body,
                Change::Status(status) => store.annotations[index].status = status,
                Change::Delete => {
                    store.annotations.remove(index);
                }
            }
        }
    }
    // One search per annotation: it both refreshes the stored hints and
    // places the annotation in the review that is returned.
    let ranges = store
        .annotations
        .iter_mut()
        .map(|record| {
            let range = projection.locate(record);
            if let Some(range) = &range {
                refresh(record, &projection, range.clone());
            }
            range
        })
        .collect();
    let remove = store.annotations.is_empty()
        && store.preamble == Store::generated_preamble(&location.relative);
    let content = (!remove).then(|| store.encode_checked()).transpose()?;
    let review = Review::resolved(
        &store.annotations,
        ranges,
        &projection,
        store_revision(&location.path, content.as_deref().map(str::as_bytes)),
    );
    #[cfg(test)]
    if let Some(interrupt) = BEFORE_COMMIT.take() {
        interrupt();
    }
    parent.commit(bytes.as_deref(), content.as_deref())?;
    Ok((projection, review))
}

fn utc_now() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // Convert epoch days to Gregorian civil dates using a March-based year.
    let days = (seconds / 86_400) as i64 + 719_468;
    let era = days / 146_097;
    let day_of_era = days % 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let march_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * march_month + 2) / 5 + 1;
    let month = if march_month < 10 {
        march_month + 3
    } else {
        march_month - 9
    };
    let year = era * 400 + year_of_era + i64::from(month <= 2);
    let (hour, minute, second) = (seconds / 3600 % 24, seconds / 60 % 60, seconds % 60);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

#[cfg(test)]
mod parity;
#[cfg(all(test, unix))]
mod tests;
