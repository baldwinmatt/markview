use std::collections::{HashMap, HashSet};
use std::io::{self, BufRead, BufReader};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use markview::annotations::{self, Error, Request, FORM_CAP};
use markview::DestinationKind;

use super::{
    broadcast_reload, decode_repeated, serve_review, write_response_headers, Clients, ReloadKind,
    ServeConfig, SharedConfig, WATCH_DEBOUNCE,
};

const ENDPOINT: &str = "/_markview/annotations";
const HEAD_CAP: usize = 32 * 1024;
const HOST_SCAN_CAP: usize = 256 * 1024;

pub fn reserved(target: &str) -> bool {
    let path = target.split(['?', '#']).next().unwrap_or(target);
    path.starts_with("/_markview/")
        || decode_repeated(path).is_some_and(|decoded| {
            let mut components = Vec::new();
            for component in decoded.split('/') {
                match component {
                    "" | "." => {}
                    ".." => {
                        components.pop();
                    }
                    part => components.push(part),
                }
            }
            components.first() == Some(&"_markview")
        })
}

/// Whether a Markdown destination must not reach the page.
///
/// `javascript:` and `vbscript:` run script wherever they appear. `data:` can
/// navigate to an attacker-written document, so it is blocked for links, but
/// an `<img>` only ever decodes it as an image: `data:image/*` (SVG included,
/// which cannot run script inside `<img>`) is kept, any other type is not.
pub fn blocked_destination(kind: DestinationKind, destination: &str) -> bool {
    // The Markdown parser has already decoded HTML character references.
    // The browser trims leading C0/space and strips ASCII tabs and line breaks.
    let normalized: String = destination
        .trim_start_matches(|ch: char| ch <= ' ')
        .chars()
        .filter(|ch| !matches!(ch, '\t' | '\n' | '\r'))
        .collect();
    let Some((scheme, rest)) = normalized.split_once(':') else {
        return false;
    };
    if scheme.eq_ignore_ascii_case("data") {
        // `data:[<media type>][;base64],<data>`; no media type is text/plain.
        let media_type = rest.split([';', ',']).next().unwrap_or("").trim();
        return kind == DestinationKind::Link
            || !media_type
                .get(..6)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("image/"));
    }
    ["javascript", "vbscript"]
        .iter()
        .any(|blocked| scheme.eq_ignore_ascii_case(blocked))
}

/// Whether a `Host` header value names this server on loopback. DNS resolves
/// names case-insensitively and accepts one trailing dot, so browsers send
/// `LOCALHOST:7878` or `localhost.:7878` for the same origin. Any other name
/// or port may be a DNS-rebinding page and is refused.
pub fn loopback_host(host: &str, port: u16) -> bool {
    let Some((name, host_port)) = host.rsplit_once(':') else {
        return false;
    };
    host_port == port.to_string()
        && (name == "127.0.0.1"
            || name
                .strip_suffix('.')
                .unwrap_or(name)
                .eq_ignore_ascii_case("localhost"))
}

/// Finds the `Host` header of an ordinary request without the strict
/// parser's rules: any line ending, any bytes. `None` means the request has
/// no single well-formed Host within the cap and deadline, so the caller
/// treats the client as untrusted and still serves the page.
pub fn lenient_host(reader: &mut BufReader<TcpStream>, deadline: Instant) -> Option<String> {
    let mut budget = HOST_SCAN_CAP;
    let mut host = None;
    loop {
        let line = bounded_bytes(reader, deadline, budget, true).ok()?;
        budget -= line.len();
        let line = line.strip_suffix(b"\n")?;
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() {
            return host;
        }
        if line.len() >= 5 && line[..5].eq_ignore_ascii_case(b"host:") {
            let value = String::from_utf8_lossy(&line[5..]);
            if host
                .replace(value.trim_matches([' ', '\t']).to_owned())
                .is_some()
            {
                return None;
            }
        }
    }
}

/// Compares a presented token with the real one without stopping at the
/// first difference: every byte of the real token is always examined, so the
/// time taken says nothing about how much of a guess was right. Returns the
/// accumulated difference (zero only for an exact match) and the number of
/// bytes compared.
fn token_difference(presented: &[u8], expected: &[u8]) -> (usize, usize) {
    let mut difference = presented.len() ^ expected.len();
    let mut compared = 0;
    for (index, byte) in expected.iter().enumerate() {
        difference |= usize::from(byte ^ presented.get(index).copied().unwrap_or(0));
        compared += 1;
    }
    (difference, compared)
}

fn token_matches(presented: Option<&str>, expected: &str) -> bool {
    presented
        .is_some_and(|presented| token_difference(presented.as_bytes(), expected.as_bytes()).0 == 0)
}

pub struct HttpRequest {
    pub method: String,
    pub target: String,
    headers: HashMap<String, String>,
}

fn bad_request() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "malformed HTTP request")
}

fn bounded_bytes(
    reader: &mut BufReader<TcpStream>,
    deadline: Instant,
    cap: usize,
    line: bool,
) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    while bytes.len() < cap {
        let timeout = deadline
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "request deadline"))?;
        reader.get_ref().set_read_timeout(Some(timeout))?;
        let buf = reader.fill_buf()?;
        if buf.is_empty() {
            return Err(bad_request());
        }
        let buf = &buf[..buf.len().min(cap - bytes.len())];
        let end = if line {
            buf.iter().position(|byte| *byte == b'\n')
        } else {
            None
        };
        let count = end.map_or(buf.len(), |index| index + 1);
        bytes.extend_from_slice(&buf[..count]);
        reader.consume(count);
        if end.is_some() {
            return Ok(bytes);
        }
    }
    if line {
        Err(bad_request())
    } else {
        Ok(bytes)
    }
}

fn read_line(
    reader: &mut BufReader<TcpStream>,
    deadline: Instant,
    budget: &mut usize,
) -> io::Result<String> {
    let line = bounded_bytes(reader, deadline, *budget, true)?;
    *budget -= line.len();
    let line = line.strip_suffix(b"\r\n").ok_or_else(bad_request)?;
    std::str::from_utf8(line)
        .map(str::to_owned)
        .map_err(|_| bad_request())
}

impl HttpRequest {
    /// Strictly parses a request to the reserved prefix: `request_line` as
    /// already read from the connection, then the headers from `reader`.
    /// Ordinary routes never come here; they only need the request line.
    pub fn parse(
        request_line: &str,
        reader: &mut BufReader<TcpStream>,
        deadline: Instant,
    ) -> io::Result<Self> {
        let mut budget = HEAD_CAP
            .checked_sub(request_line.len())
            .ok_or_else(bad_request)?;
        let line = request_line.strip_suffix("\r\n").ok_or_else(bad_request)?;
        let mut parts = line.split(' ');
        let (Some(method), Some(target), Some("HTTP/1.1" | "HTTP/1.0"), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(bad_request());
        };
        if !target.starts_with('/')
            || target.contains('#')
            || target.bytes().any(|byte| byte.is_ascii_control())
        {
            return Err(bad_request());
        }
        let mut headers = HashMap::new();
        loop {
            let header = read_line(reader, deadline, &mut budget)?;
            if header.is_empty() {
                break;
            }
            let (key, value) = header.split_once(':').ok_or_else(bad_request)?;
            if key.is_empty()
                || !key
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^`_|~".contains(&byte))
                || value
                    .bytes()
                    .any(|byte| byte.is_ascii_control() && byte != b'\t')
            {
                return Err(bad_request());
            }
            let key = key.to_ascii_lowercase();
            if matches!(
                key.as_str(),
                "host"
                    | "content-length"
                    | "content-type"
                    | "transfer-encoding"
                    | "x-markview-token"
            ) && headers
                .insert(key, value.trim_matches([' ', '\t']).to_owned())
                .is_some()
            {
                return Err(bad_request());
            }
        }
        Ok(Self {
            method: method.to_owned(),
            target: target.to_owned(),
            headers,
        })
    }

    fn header(&self, key: &str) -> Option<&str> {
        self.headers.get(key).map(String::as_str)
    }
}

fn document(config: &ServeConfig, route: &str) -> Option<annotations::Location> {
    config
        .served_location(config.document_for_route(route)?)
        .ok()
}

/// The review a page asks for when it loads. A failure names the document
/// and what is wrong with it, as `markview --annotations` does, so a reader
/// facing a page without comments knows which file to look at.
fn page_review(
    config: &ServeConfig,
    document: &super::ServedDocument,
) -> Result<annotations::Review, Error> {
    use serve_review::DocumentProblem;
    let location = config.served_location(document).map_err(|error| {
        DocumentProblem::wrap(
            config,
            document,
            "document",
            &document.source_path,
            error.into(),
            "404 Not Found",
        )
    })?;
    let source = annotations::read_source(&location).map_err(|error| {
        DocumentProblem::wrap(
            config,
            document,
            "document",
            &location.document,
            error.into(),
            "500 Internal Server Error",
        )
    })?;
    config
        .review_cache
        .read(&location, &source)
        .map_err(|error| {
            let status = status(&error);
            DocumentProblem::wrap(
                config,
                document,
                "annotation store",
                &location.path,
                error,
                status,
            )
        })
}

fn status(error: &Error) -> &'static str {
    match error {
        Error::Invalid(_) => "400 Bad Request",
        Error::Conflict(_) | Error::Section(..) => "409 Conflict",
        Error::Io(_) => "500 Internal Server Error",
    }
}

fn mutate_review(
    config: &ServeConfig,
    location: &annotations::Location,
    operation: annotations::Operation,
) -> Result<annotations::Review, Error> {
    config.review_cache.mutate(location, operation)
}

fn annotation_response(
    stream: &mut TcpStream,
    status: &str,
    body: &str,
    head: bool,
) -> io::Result<()> {
    write_response_headers(
        stream,
        status,
        "application/json; charset=utf-8",
        body.as_bytes(),
        head,
        "Cache-Control: no-store\r\n",
    )
}

fn post_body(
    reader: &mut BufReader<TcpStream>,
    request: &HttpRequest,
    query: &str,
    deadline: Instant,
) -> Result<Vec<u8>, &'static str> {
    let length = request
        .header("content-length")
        .filter(|value| value.bytes().all(|byte| byte.is_ascii_digit()))
        .and_then(|value| value.parse::<usize>().ok());
    let Some(length @ 1..=FORM_CAP) = length else {
        return Err("invalid annotation request framing");
    };
    if !query.is_empty()
        || request.header("transfer-encoding").is_some()
        || !request.header("content-type").is_some_and(|value| {
            value.split(';').next().is_some_and(|mime| {
                mime.trim()
                    .eq_ignore_ascii_case("application/x-www-form-urlencoded")
            })
        })
    {
        return Err("invalid annotation request framing");
    }
    bounded_bytes(reader, deadline, length, false).map_err(|_| "incomplete annotation request")
}

pub fn handle(
    stream: &mut TcpStream,
    reader: &mut BufReader<TcpStream>,
    shared: &SharedConfig,
    request: &HttpRequest,
    deadline: Instant,
    session: Option<&serve_review::Session>,
    clients: &Clients,
) -> io::Result<()> {
    let head = request.method == "HEAD";
    let fail = |stream: &mut TcpStream, status: &str, message: &str| {
        annotation_response(
            stream,
            status,
            &format!("{{\"error\":{}}}", annotations::json_string(message)),
            head,
        )
    };
    let initial = shared.read().expect("config lock").clone();
    if !request
        .header("host")
        .is_some_and(|host| loopback_host(host, initial.port))
    {
        return fail(stream, "403 Forbidden", "invalid Host");
    }
    let (path, query) = request
        .target
        .split_once('?')
        .unwrap_or((&request.target, ""));
    let review_endpoint = session.filter(|_| path == serve_review::ENDPOINT);
    if path != ENDPOINT && review_endpoint.is_none() {
        return if matches!(request.method.as_str(), "GET" | "HEAD") {
            fail(stream, "404 Not Found", "unknown endpoint")
        } else {
            fail(stream, "405 Method Not Allowed", "method not allowed")
        };
    }
    let body = if request.method == "POST" && initial.session.review_token.is_some() {
        if !token_matches(
            request.header("x-markview-token"),
            initial
                .session
                .review_token
                .as_deref()
                .expect("checked token"),
        ) {
            return fail(stream, "403 Forbidden", "invalid annotation token");
        }
        match post_body(reader, request, query, deadline) {
            Ok(body) => Some(body),
            Err(message) => return fail(stream, "400 Bad Request", message),
        }
    } else {
        None
    };
    if let Some(session) = review_endpoint {
        if !query.is_empty() {
            return fail(stream, "400 Bad Request", "unexpected review query");
        }
        // Both read the whole collection, so neither holds the config lock:
        // a rescan waiting to publish would otherwise stall every request.
        let config = shared.read().expect("config lock").clone();
        let result = match request.method.as_str() {
            "GET" | "HEAD" => session.summary(&config).map(|json| (json, None)),
            "POST" if body.is_some() => session
                .finish(&config, body.as_deref().expect("validated POST"), || {
                    // `try_read`: this runs under the session lock, which a
                    // save takes while holding the config lock. A rescan
                    // that is publishing right now is a conflict too.
                    shared
                        .try_read()
                        .is_ok_and(|published| Arc::ptr_eq(&published, &config))
                })
                .map(|result| ("{\"finished\":true}".to_owned(), Some(result))),
            _ => return fail(stream, "405 Method Not Allowed", "method not allowed"),
        };
        return match result {
            Ok((json, Some(result))) => {
                broadcast_reload(clients, ReloadKind::ReviewFinished);
                let response = stream
                    .set_write_timeout(Some(Duration::from_millis(250)))
                    .and_then(|()| annotation_response(stream, "200 OK", &json, false));
                session.publish(result);
                response
            }
            Ok((json, None)) => annotation_response(stream, "200 OK", &json, head),
            Err(error) => respond_error(stream, error, "review summary is unavailable", fail),
        };
    }
    let result = match request.method.as_str() {
        "GET" | "HEAD" => {
            let Some(route) = query
                .strip_prefix("doc=")
                .filter(|route| !route.contains('&'))
                .and_then(|route| super::percent_decode(&route.replace('+', " ")))
            else {
                return fail(stream, "400 Bad Request", "expected a document route");
            };
            let Some(document) = initial.document_for_route(&route) else {
                return fail(
                    stream,
                    "404 Not Found",
                    "document route or store is unavailable",
                );
            };
            page_review(&initial, document).map(Some)
        }
        "POST" if body.is_some() => {
            // Review mutations use the current allowlist and hold it stable
            // through validation and writing. Ordinary serve uses its
            // immutable request snapshot.
            let published = session.map(|_| shared.read().expect("config lock"));
            let config = published.as_deref().unwrap_or(&initial);
            let mutation = || {
                let form = Request::parse(body.as_deref().expect("validated POST"))?;
                let route = form
                    .doc
                    .ok_or(Error::Invalid("expected a document route"))?;
                let Some(location) = document(config, &route) else {
                    return Ok(None);
                };
                let result = mutate_review(config, &location, form.operation)?;
                if session.is_some() {
                    if let Some(route) = config.document_route_for_path(&location.document) {
                        broadcast_reload(clients, ReloadKind::Annotations(route.to_owned()));
                    }
                }
                Ok(Some(result))
            };
            match session {
                Some(session) => session.mutate(mutation),
                None => mutation(),
            }
        }
        _ => return fail(stream, "405 Method Not Allowed", "method not allowed"),
    };
    match result {
        Ok(Some(review)) => annotation_response(stream, "200 OK", &review.json(), head),
        Ok(None) => fail(
            stream,
            "404 Not Found",
            "document route or store is unavailable",
        ),
        Err(error) => respond_error(stream, error, "annotation store is unavailable", fail),
    }
}

fn respond_error(
    stream: &mut TcpStream,
    error: Error,
    unavailable: &str,
    fail: impl FnOnce(&mut TcpStream, &str, &str) -> io::Result<()>,
) -> io::Result<()> {
    if let Some(problem) = serve_review::DocumentProblem::of(&error) {
        eprintln!("markview: {problem}");
        return fail(stream, problem.status, &problem.public);
    }
    let message = error.to_string();
    match error {
        Error::Invalid(_) => fail(stream, "400 Bad Request", &message),
        Error::Conflict(_) if serve_review::is_finished(&error) => annotation_response(
            stream,
            "409 Conflict",
            &format!(
                "{{\"finished\":true,\"error\":{}}}",
                annotations::json_string(&message)
            ),
            false,
        ),
        Error::Conflict(_) | Error::Section(..) => fail(stream, "409 Conflict", &message),
        Error::Io(_) => {
            eprintln!("markview: {message}");
            fail(stream, "500 Internal Server Error", unavailable)
        }
    }
}

const SCRIPT: &str = "text/javascript; charset=utf-8";
const STYLE: &str = "text/css; charset=utf-8";

/// Every static asset under the reserved prefix: its route, media type and
/// body. This table is the only list of them. It decides what is served,
/// what each `?v=` digest covers, and what the browser harness inlines, so
/// an asset added here is served and versioned without another edit.
const ASSETS: &[Asset] = &[
    ("/_markview/review.js", SCRIPT, annotations::REVIEW_SCRIPT),
    ("/_markview/review.css", STYLE, annotations::REVIEW_STYLE),
    ("/_markview/verdict.js", SCRIPT, annotations::VERDICT_SCRIPT),
    ("/_markview/copy.js", SCRIPT, annotations::COPY_SCRIPT),
    ("/_markview/copy.css", STYLE, annotations::COPY_STYLE),
];

type Asset = (&'static str, &'static str, &'static str);

/// Served from the reserved prefix so the browser caches each asset instead
/// of every page carrying it inline.
pub fn static_asset(target: &str) -> Option<(&'static str, &'static str)> {
    let path = target.split('?').next()?;
    ASSETS
        .iter()
        .find(|(route, ..)| *route == path)
        .map(|(_, mime, body)| (*mime, *body))
}

/// The URLs carry a digest of the content, so a response can be cached for
/// good and a new markview build is still picked up at once.
pub const STATIC_CACHE: &str = "Cache-Control: public, max-age=31536000, immutable\r\n";

/// Whether an asset belongs to the copy controls. Every page links those,
/// and only pages with review hooks link the rest.
fn copy_asset(route: &str) -> bool {
    route.starts_with("/_markview/copy.")
}

/// The digest shared by the copy assets (`copy`) or by the review assets.
/// The two are versioned apart: a digest that covered both would change the
/// bytes of a plain page whenever review.js, review.css or verdict.js is
/// edited.
fn digest(assets: &[Asset], copy: bool) -> String {
    let bodies: Vec<&str> = assets
        .iter()
        .filter(|(route, ..)| copy_asset(route) == copy)
        .map(|(.., body)| *body)
        .collect();
    annotations::source_revision(&bodies.concat())
}

/// The versioned URL a page links `route` with. Only assets in the table
/// can be linked.
fn asset_url(route: &str) -> String {
    static VERSIONS: OnceLock<[String; 2]> = OnceLock::new();
    assert!(
        ASSETS.iter().any(|(known, ..)| *known == route),
        "{route} is not a static asset"
    );
    let versions = VERSIONS.get_or_init(|| [digest(ASSETS, false), digest(ASSETS, true)]);
    format!("{route}?v={}", versions[usize::from(copy_asset(route))])
}

/// What a page needs to show annotations: the cached stylesheet and script,
/// and the call that connects them to this route. `nonce_attr` is the
/// ` nonce="..."` attribute a token-bearing page's policy requires.
pub fn hooks(
    route: &str,
    token: Option<&str>,
    port: u16,
    nonce_attr: &str,
    blocking: bool,
) -> String {
    let verdict = if blocking {
        format!(
            r#"<script{nonce_attr} src="{}"></script>"#,
            asset_url("/_markview/verdict.js")
        )
    } else {
        String::new()
    };
    format!(
        "<link rel=\"stylesheet\" href=\"{}\">
<script{nonce_attr} src=\"{}\"></script>
{verdict}
<script{nonce_attr}>(() => {{
  const route = {};
  const token = {};
  markviewServeReview(route, token, {port}, {blocking});
}})();</script>",
        asset_url("/_markview/review.css"),
        asset_url("/_markview/review.js"),
        annotations::json_string(route),
        token.map_or("null".to_owned(), annotations::json_string),
    )
}

pub fn copy_hooks(nonce_attr: &str) -> String {
    format!(
        r#"<link rel="stylesheet" href="{}">
<script{nonce_attr} src="{}"></script>"#,
        asset_url("/_markview/copy.css"),
        asset_url("/_markview/copy.js"),
    )
}

/// The policy sent with a token-bearing page. Only scripts carrying the
/// response's fresh nonce run: Markview's own. An injected or inline script,
/// an inline event handler, a `javascript:` URL, a plugin, a frame and a form
/// post are all refused, so nothing in a document can use the write token.
pub fn content_security_policy(nonce: &str) -> String {
    format!(
        "default-src 'self'; script-src 'nonce-{nonce}'; style-src 'self' 'unsafe-inline'; img-src 'self' https: http: data:; object-src 'none'; base-uri 'none'; frame-src 'none'; frame-ancestors 'none'; form-action 'none'"
    )
}

/// Whether `document` has an annotation store file right now.
pub fn has_store(document: &Path) -> bool {
    annotations::resolve(document)
        .and_then(|location| annotations::snapshot(&location))
        .is_ok_and(|store| store.is_some())
}

/// The annotation store watches for served documents, shared between the
/// connection threads and the thread that turns store events into reloads.
pub struct Stores {
    watcher: Mutex<annotations::StoreWatcher>,
    /// Documents whose page went out without review hooks. Such a page does
    /// not listen for `annotations` events, so when its first store appears
    /// only a full reload can show the comments.
    plain: Mutex<HashSet<PathBuf>>,
    /// How often the event thread has woken up, for the idle test.
    #[cfg(test)]
    pub wakeups: std::sync::atomic::AtomicUsize,
    /// Called whenever the watches are brought in line with a collection.
    #[cfg(test)]
    on_sync: Mutex<Option<Box<dyn Fn() + Send>>>,
}

impl Stores {
    /// Notes that a page for `document` was served. Stores are watched from
    /// the first time their document is served, not from startup: nobody can
    /// be looking at annotations for a page that was never requested.
    pub fn served(&self, document: &Path, hooks: bool) {
        if let Ok(mut watcher) = self.watcher.lock() {
            watcher.watch(document);
        }
        if !hooks {
            if let Ok(mut plain) = self.plain.lock() {
                plain.insert(document.to_owned());
            }
        }
    }

    fn note_sync(&self) {
        #[cfg(test)]
        if let Some(hook) = self.on_sync.lock().expect("hook lock").as_ref() {
            hook();
        }
    }

    #[cfg(test)]
    pub fn on_sync(&self, hook: impl Fn() + Send + 'static) {
        *self.on_sync.lock().expect("hook lock") = Some(Box::new(hook));
    }

    /// Whether the store of `document` is being watched.
    #[cfg(test)]
    pub fn watches(&self, document: &Path) -> bool {
        self.watcher.lock().expect("watcher lock").watches(document)
    }

    /// Brings the watches in line with the published collection: forgets
    /// documents a rescan dropped and, under a blocking review, watches every
    /// document, because its bar counts comments on pages nobody has opened.
    ///
    /// The collection is read here, under the watcher lock, instead of being
    /// passed in: two rescans can finish close together, and whichever syncs
    /// last must leave the watches matching the newest collection.
    pub fn sync(&self, shared: &SharedConfig) {
        self.note_sync();
        let Ok(mut watcher) = self.watcher.lock() else {
            return;
        };
        let config = shared.read().expect("config lock").clone();
        let served: HashSet<&Path> = config
            .documents
            .iter()
            .map(|document| document.source_path.as_path())
            .collect();
        watcher.retain(|path| served.contains(path));
        if config.session.blocking_review {
            for document in &config.documents {
                watcher.watch(&document.source_path);
            }
        }
        drop(watcher);
        if let Ok(mut plain) = self.plain.lock() {
            plain.retain(|path| served.contains(path.as_path()));
        }
    }
}

pub fn watch_stores(shared: SharedConfig, clients: Clients) -> notify::Result<Arc<Stores>> {
    let (tx, rx) = mpsc::channel();
    let watcher = annotations::StoreWatcher::new(move |event| {
        if let Ok(event) = event {
            let _ = tx.send(event);
        }
    })?;
    let stores = Arc::new(Stores {
        watcher: Mutex::new(watcher),
        plain: Mutex::new(HashSet::new()),
        #[cfg(test)]
        wakeups: std::sync::atomic::AtomicUsize::new(0),
        #[cfg(test)]
        on_sync: Mutex::new(None),
    });
    let thread_stores = stores.clone();
    // No timer: the thread sleeps until a watched directory changes.
    std::thread::spawn(move || {
        while let Ok(event) = rx.recv() {
            let mut events = vec![event];
            while let Ok(event) = rx.recv_timeout(WATCH_DEBOUNCE) {
                events.push(event);
            }
            #[cfg(test)]
            thread_stores
                .wakeups
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let config = shared.read().expect("config lock").clone();
            let changed = match thread_stores.watcher.lock() {
                Ok(mut watcher) => watcher.changed(&events),
                Err(_) => break,
            };
            for document in changed {
                config.review_summary.invalidate(&document);
                let Some(route) = config.document_route_for_path(&document) else {
                    continue;
                };
                let first_store = has_store(&document)
                    && thread_stores
                        .plain
                        .lock()
                        .is_ok_and(|mut plain| plain.remove(&document));
                let reload = if first_store {
                    ReloadKind::Structure
                } else {
                    ReloadKind::Annotations(route.to_owned())
                };
                broadcast_reload(&clients, reload);
            }
        }
    });
    Ok(stores)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const SOURCE: &str = concat!(
        "# Review fixture\n\n",
        "Before **two** &amp; [a link](target.md) 😀![alt](image.png).[^n]\n\n",
        "[unsafe](java&#x09;script:alert%281%29) [fragment](#review-fixture)\n\n",
        "```diff\n-old\n+new\n```\n\n",
        "> ```rust\r\n> \tlet ready = true;\r\n> ```\r\n\r\n",
        "    indented & text\n\n",
        "<div>\nliteral &amp;\n</div>\n\n",
        "- [x] task\n\n",
        "> [!NOTE]\n> Alert **prose**.\n\n",
        "| first | second |\n| - | - |\n| a | b |\n\n",
        "[^n]: Footnote *prose*.\n\n",
        "â€™ `two\nlines` with NEL\u{85}space\n",
    );

    fn many_documents(count: usize) -> (tempfile::TempDir, Vec<PathBuf>) {
        let dir = tempfile::tempdir().unwrap();
        for index in 0..count {
            let folder = dir.path().join(format!("section-{:02}", index % 20));
            std::fs::create_dir_all(&folder).unwrap();
            std::fs::write(
                folder.join(format!("doc-{index}.md")),
                format!("# Doc {index}\n"),
            )
            .unwrap();
        }
        let root = dir.path().to_path_buf();
        (dir, vec![root])
    }

    #[test]
    fn store_watching_adds_little_to_startup_for_many_documents() {
        let (_dir, inputs) = many_documents(2_000);
        let fastest = |work: &dyn Fn()| {
            (0..3)
                .map(|_| {
                    let start = Instant::now();
                    work();
                    start.elapsed()
                })
                .min()
                .unwrap()
        };
        // Everything `--serve` did at startup before annotations existed.
        let baseline = fastest(&|| {
            ServeConfig::from_inputs(inputs.clone(), 0, true).unwrap();
        });
        let watched = fastest(&|| {
            let config = ServeConfig::from_inputs(inputs.clone(), 0, true).unwrap();
            let shared: SharedConfig = Arc::new(std::sync::RwLock::new(Arc::new(config)));
            watch_stores(shared, Arc::new(Mutex::new(Vec::new()))).unwrap();
        });
        assert!(
            watched < baseline * 3,
            "startup with store watching took {watched:?}, {baseline:?} without"
        );
    }

    #[test]
    fn idle_store_watcher_never_wakes_up() {
        use std::sync::atomic::Ordering;
        let (_dir, inputs) = many_documents(3);
        let config = ServeConfig::from_inputs(inputs, 0, true).unwrap();
        let shared: SharedConfig = Arc::new(std::sync::RwLock::new(Arc::new(config)));
        let stores = watch_stores(shared, Arc::new(Mutex::new(Vec::new()))).unwrap();
        // More than three of the old 300 ms polling intervals.
        std::thread::sleep(Duration::from_millis(1_000));
        assert_eq!(stores.wakeups.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn annotation_reads_and_writes_share_the_serve_cache() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.md");
        std::fs::write(&path, "# Doc\n\nSome text.\n").unwrap();
        let config = ServeConfig::from_inputs(vec![path], 7878, false).unwrap();
        let location = document(&config, "/").unwrap();
        let form = Request::parse(b"op=add&body=First").unwrap();
        let added = mutate_review(&config, &location, form.operation).unwrap();
        // Every page load and every `annotations` event asks again.
        for _ in 0..5 {
            let page = page_review(&config, config.default_document().unwrap());
            assert_eq!(page.unwrap(), added);
        }
        // One projection, and one resolution: the write's own.
        assert_eq!(config.review_cache.work(), (1, 1));
    }

    /// A `--review` server for one document, run by `handle_connection` in
    /// this process, so a test can reach its config, session and hooks.
    struct LocalReview {
        _dir: tempfile::TempDir,
        port: u16,
        shared: SharedConfig,
    }

    impl LocalReview {
        /// Serves `connections` requests, then stops accepting.
        fn start(connections: usize, prepare: impl FnOnce(&ServeConfig)) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("doc.md");
            std::fs::write(&path, "# Doc\n\nSome text.\n").unwrap();
            let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
            let port = listener.local_addr().unwrap().port();
            let mut config = ServeConfig::from_inputs(vec![path], port, false).unwrap();
            config.session.review_token = Some("token".to_owned());
            config.session.blocking_review = true;
            prepare(&config);
            let shared: SharedConfig = Arc::new(std::sync::RwLock::new(Arc::new(config)));
            let clients: Clients = Arc::new(Mutex::new(Vec::new()));
            let stores = watch_stores(shared.clone(), clients.clone()).unwrap();
            let (session, verdict) = serve_review::Session::new();
            let server = shared.clone();
            std::thread::spawn(move || {
                // Kept so that publishing a verdict has somewhere to go.
                let _verdict = verdict;
                for stream in listener.incoming().take(connections) {
                    let (shared, clients) = (server.clone(), clients.clone());
                    let (stores, session) = (stores.clone(), session.clone());
                    std::thread::spawn(move || {
                        super::super::handle_connection(
                            stream.unwrap(),
                            &shared,
                            clients,
                            &stores,
                            Some(session),
                        )
                    });
                }
            });
            Self {
                _dir: dir,
                port,
                shared,
            }
        }

        /// Sends one request. The response arrives on the returned channel.
        fn request(&self, request_line: &str, form: Option<&str>) -> mpsc::Receiver<String> {
            use std::io::{Read, Write};
            let port = self.port;
            let mut request = format!("{request_line} HTTP/1.1\r\nHost: localhost:{port}\r\n");
            if let Some(form) = form {
                request.push_str(&format!(
                    "Content-Type: application/x-www-form-urlencoded\r\nX-Markview-Token: token\r\nContent-Length: {}\r\n\r\n{form}",
                    form.len()
                ));
            } else {
                request.push_str("\r\n");
            }
            let (done, response) = mpsc::channel();
            std::thread::spawn(move || {
                let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
                stream.write_all(request.as_bytes()).unwrap();
                let mut response = String::new();
                let _ = stream.read_to_string(&mut response);
                let _ = done.send(response);
            });
            response
        }

        /// The status line and body of a response, waited for.
        fn response(&self, request_line: &str, form: Option<&str>) -> (String, String) {
            let response = self
                .request(request_line, form)
                .recv_timeout(Duration::from_secs(5))
                .expect("a response");
            let (head, body) = response
                .split_once("\r\n\r\n")
                .expect("a complete response");
            (head.lines().next().unwrap().to_owned(), body.to_owned())
        }
    }

    #[test]
    fn a_summary_in_progress_blocks_neither_a_rescan_nor_a_save() {
        let (entered, held) = mpsc::channel();
        let (release, proceed) = mpsc::channel::<()>();
        let server = LocalReview::start(2, |config| {
            config.review_summary.hold_next(move || {
                entered.send(()).unwrap();
                let _ = proceed.recv();
            })
        });
        let summary = server.request("GET /_markview/review", None);
        held.recv_timeout(Duration::from_secs(5)).unwrap();
        // The summary is now mid-computation. A rescan can still publish...
        let rescan_can_publish = server.shared.try_write().is_ok();
        // ...and a save is answered instead of queueing behind the summary.
        let saved = server
            .request(
                "POST /_markview/annotations",
                Some("op=add&doc=%2F&body=Saved"),
            )
            .recv_timeout(Duration::from_secs(5));
        release.send(()).unwrap();
        let summary = summary.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(rescan_can_publish, "the summary held the config lock");
        let saved = saved.expect("the save queued behind the summary");
        assert!(saved.starts_with("HTTP/1.1 200"), "{saved}");
        assert!(summary.starts_with("HTTP/1.1 200"), "{summary}");
        assert!(summary.contains("\"review_revision\":"), "{summary}");
    }

    #[test]
    fn requests_after_the_verdict_say_the_review_is_finished() {
        let server = LocalReview::start(5, |_| {});
        let (_, summary) = server.response("GET /_markview/review", None);
        let revision = summary
            .split("\"review_revision\":\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .expect("review revision");
        let finish = format!(
            "op=finish&verdict=approved&review_revision={revision}&doc=/&source_revision={}",
            annotations::source_revision("# Doc\n\nSome text.\n")
        );
        assert_eq!(
            server.response("POST /_markview/review", Some(&finish)),
            (
                "HTTP/1.1 200 OK".to_owned(),
                r#"{"finished":true}"#.to_owned()
            )
        );
        // The page tells "already finished" from any other conflict by the
        // `finished` field, not by the wording of the message.
        let finished = (
            "HTTP/1.1 409 Conflict".to_owned(),
            r#"{"finished":true,"error":"Review finished. You can close this tab."}"#.to_owned(),
        );
        assert_eq!(
            server.response("POST /_markview/review", Some(&finish)),
            finished
        );
        assert_eq!(
            server.response(
                "POST /_markview/annotations",
                Some("op=add&doc=%2F&body=Late")
            ),
            finished
        );
        assert_eq!(
            server.response("GET /_markview/review", None),
            (
                "HTTP/1.1 200 OK".to_owned(),
                r#"{"finished":true}"#.to_owned()
            )
        );
    }

    #[cfg(unix)]
    #[test]
    fn finish_and_annotations_refuse_the_same_document_routes() {
        let server = LocalReview::start(64, |_| {});
        let finish = |doc: &str| {
            let form = format!(
                "op=finish&verdict=approved&review_revision=unconfirmed&doc={doc}&source_revision=none"
            );
            server.response("POST /_markview/review", Some(&form)).0
        };
        let list = |doc: &str| {
            server
                .response(&format!("GET /_markview/annotations?doc={doc}"), None)
                .0
        };
        let add = |doc: &str| {
            let form = format!("op=edit&id=a1&store_revision=none&body=x&doc={doc}");
            server
                .response("POST /_markview/annotations", Some(&form))
                .0
        };
        // Both endpoints take a route as an allowlist key. A route that is
        // spelled another way is not the key: neither endpoint resolves it.
        // What differs is only how each one says no: the annotations of an
        // unknown document are not found, and a finish for one is a conflict
        // that sends the page back to the collection.
        for alias in [
            "/./doc.md",
            "/doc.md/",
            "//doc.md",
            "doc.md",
            "/DOC.md",
            "/sub/../doc.md",
            "/doc.md%3Fx",
            "/other.md",
        ] {
            assert_eq!(list(alias), "HTTP/1.1 404 Not Found", "{alias}");
            assert_eq!(add(alias), "HTTP/1.1 404 Not Found", "{alias}");
            assert_eq!(finish(alias), "HTTP/1.1 409 Conflict", "{alias}");
        }
        // The real routes are known to both. (The revisions above are not
        // the confirmed ones, so the finish is still a conflict; the edit
        // finds no store to change.)
        for route in ["/", "/doc.md"] {
            assert_eq!(list(route), "HTTP/1.1 200 OK", "{route}");
            assert_eq!(add(route), "HTTP/1.1 409 Conflict", "{route}");
        }

        // The served file becomes a symlink to a file outside the collection.
        // Its route is still on the allowlist, but it no longer names the
        // file that was allowlisted, and both endpoints say what they say
        // for a route they do not know.
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("elsewhere.md");
        std::fs::write(&target, "# Elsewhere\n").unwrap();
        let served = server.shared.read().unwrap().documents[0]
            .source_path
            .clone();
        std::fs::remove_file(&served).unwrap();
        std::os::unix::fs::symlink(&target, &served).unwrap();
        for route in ["/", "/doc.md"] {
            assert_eq!(list(route), "HTTP/1.1 404 Not Found", "{route}");
            assert_eq!(add(route), "HTTP/1.1 404 Not Found", "{route}");
            assert_eq!(finish(route), "HTTP/1.1 409 Conflict", "{route}");
        }
    }

    #[test]
    fn every_static_asset_is_served_linked_and_part_of_its_own_digest() {
        let (page, _) = review_page();
        let plain = copy_hooks("");
        for asset @ (route, mime, body) in ASSETS {
            // Served, whatever version the URL carries.
            assert_eq!(static_asset(route), Some((*mime, *body)));
            assert_eq!(
                static_asset(&format!("{route}?v=0123456789abcdef")),
                Some((*mime, *body))
            );
            // Linked from a review page with its digest, and from a plain
            // page only if it is one of the copy assets.
            let copy = copy_asset(route);
            let url = asset_url(route);
            assert!(page.contains(&format!("\"{url}\"")), "{url}");
            assert_eq!(plain.contains(&format!("\"{url}\"")), copy, "{url}");
            assert!(url.ends_with(&format!("?v={}", digest(ASSETS, copy))));
            // Part of that digest: without it, or with one more byte in it,
            // the digest differs. The other group's digest never notices.
            let without: Vec<Asset> = ASSETS
                .iter()
                .filter(|other| other != &asset)
                .copied()
                .collect();
            let longer: &'static str = String::leak(format!("{body}\n"));
            let edited: Vec<Asset> = ASSETS
                .iter()
                .map(|other| {
                    if other == asset {
                        (*route, *mime, longer)
                    } else {
                        *other
                    }
                })
                .collect();
            for changed in [&without, &edited] {
                assert_ne!(digest(changed, copy), digest(ASSETS, copy), "{route}");
                assert_eq!(digest(changed, !copy), digest(ASSETS, !copy), "{route}");
            }
        }
        // A new review asset is versioned by being in the table, and leaves
        // the copy digest, and so every plain page, as it was.
        let mut more = ASSETS.to_vec();
        more.push(("/_markview/review-extra.js", SCRIPT, "// new"));
        assert_ne!(digest(&more, false), digest(ASSETS, false));
        assert_eq!(digest(&more, true), digest(ASSETS, true));
        assert_eq!(static_asset("/_markview/review-extra.js"), None);
        assert_eq!(static_asset("/_markview/"), None);
    }

    #[test]
    fn token_comparison_examines_every_byte() {
        let token = "0123456789abcdef".repeat(3);
        let expected = token.as_bytes();
        let mut first = expected.to_vec();
        first[0] ^= 1;
        let mut last = expected.to_vec();
        *last.last_mut().unwrap() ^= 1;
        let longer = [expected, b"0"].concat();
        // Wherever the guess goes wrong, or however long it is, the whole
        // real token is compared: there is no early exit to time.
        for (guess, matches) in [
            (expected, true),
            (first.as_slice(), false),
            (last.as_slice(), false),
            (&expected[..1], false),
            (longer.as_slice(), false),
            (b"".as_slice(), false),
        ] {
            let (difference, compared) = token_difference(guess, expected);
            assert_eq!(difference == 0, matches, "{guess:?}");
            assert_eq!(compared, expected.len(), "{guess:?}");
        }
        assert!(token_matches(Some(&token), &token));
        assert!(!token_matches(None, &token));
        assert!(!token_matches(Some("guessed"), &token));
    }

    /// The review fixture as a `--review` page with its token and nonce.
    fn review_page() -> (String, String) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("review.md");
        let target = dir.path().join("target.md");
        std::fs::write(&path, SOURCE).unwrap();
        std::fs::write(&target, "# Target").unwrap();
        let mut config = ServeConfig::from_inputs(vec![path, target], 7878, false).unwrap();
        config.session.review_token = Some("test-only".to_owned());
        let nonce = annotations::random_hex().unwrap();
        let html = super::super::render_file(
            &config,
            config.default_document().unwrap(),
            Some(super::super::ReviewHooks {
                nonce: Some(&nonce),
                token: config.session.review_token.as_deref(),
                blocking_review: true,
            }),
        )
        .unwrap();
        (html, nonce)
    }

    #[test]
    fn server_renders_normalized_safe_destinations_and_nonce_scripts() {
        let (html, nonce) = review_page();
        assert_eq!(
            html.matches(&format!("<script nonce=\"{nonce}\">")).count(),
            2
        );
        assert!(!html.contains("<script>"));
        assert!(html.contains("<a href=\"/target.md\">a link</a>"));
        assert!(html.contains("<a href=\"#\">unsafe</a>"));
        assert!(html.contains("<a href=\"#review-fixture\">fragment</a>"));
        assert!(html.contains(&format!(
            "data-source-revision=\"{}\"",
            annotations::source_revision(SOURCE)
        )));
        assert!(reserved("/%5fmarkview/anything"));
        assert!(reserved("/_markview/%25bad"));
        assert!(reserved("/foo/../_markview/anything"));
        assert!(!reserved("/nested/_markview/anything"));

        // The review script is linked, with the nonce, not inlined.
        assert!(html.contains(&format!(
            r#"<script nonce="{nonce}" src="{}"></script>"#,
            asset_url("/_markview/review.js")
        )));
        // For a look at the harness in a browser window.
        if let Some(path) = std::env::var_os("MARKVIEW_REVIEW_BROWSER_FIXTURE") {
            std::fs::write(path, browser_fixture()).unwrap();
        }
    }

    /// One self-contained page that runs `tests/review.browser.js` against
    /// the review fixture: the served page with its scripts inlined under
    /// the real policy, a stubbed network, and the harness appended. The
    /// harness writes `PASS — ...` or `FAIL — <why>` into an `<output>`.
    fn browser_fixture() -> String {
        let (html, nonce) = review_page();
        let policy = content_security_policy(&nonce);
        let projection = markview::annotations::TextProjection::new(SOURCE);
        let expected = annotations::json_string(&projection.text());
        let setup = format!(
            r#"<meta http-equiv="Content-Security-Policy" content="{policy}">
<script nonce="{nonce}">const expectedText = {expected};
window.fixtureFetch = () => new Promise(() => {{}});
window.fetch = (...args) => window.fixtureFetch(...args);
window.EventSource = class extends EventTarget {{
  constructor() {{ super(); window.fixtureEvents = this; }}
  close() {{}}
}};
</script>
<script>window.untrustedScriptRan = true;</script>"#
        );
        let mut html = html.replacen("<head>", &format!("<head>{setup}"), 1);
        // Every asset the page links, inlined: a file has no server to ask.
        // Stylesheets too, so the page is laid out as a served one is.
        for (route, mime, body) in ASSETS {
            let url = asset_url(route);
            let name = route.rsplit('/').next().unwrap();
            let (link, inline) = if *mime == SCRIPT {
                (
                    format!(r#"<script nonce="{nonce}" src="{url}"></script>"#),
                    format!(r#"<script nonce="{nonce}">{body}</script>"#),
                )
            } else {
                (
                    format!(r#"<link rel="stylesheet" href="{url}">"#),
                    format!(r#"<style data-markview-style="{name}">{body}</style>"#),
                )
            };
            assert!(html.contains(&link), "{route} is not linked");
            html = html.replace(&link, &inline);
        }
        let harness = include_str!("../tests/review.browser.js");
        html.replace(
            "</body>",
            &format!(r#"<script nonce="{nonce}">{harness}</script></body>"#),
        )
    }

    /// The Chrome or Chromium to run the harness in: `MARKVIEW_BROWSER`, or
    /// the usual install locations and names.
    fn headless_browser() -> Option<PathBuf> {
        if let Some(path) = std::env::var_os("MARKVIEW_BROWSER") {
            return Some(path.into());
        }
        let installed = [
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            "/Applications/Chromium.app/Contents/MacOS/Chromium",
        ];
        let named = [
            "google-chrome",
            "google-chrome-stable",
            "chromium",
            "chromium-browser",
        ];
        let on_path = std::env::var_os("PATH").into_iter().flat_map(|path| {
            std::env::split_paths(&path)
                .flat_map(|dir| named.map(|name| dir.join(name)))
                .collect::<Vec<_>>()
        });
        installed
            .into_iter()
            .map(PathBuf::from)
            .chain(on_path)
            .find(|path| path.is_file())
    }

    /// Runs `tests/review.browser.js` in a real, headless browser: focus,
    /// layout, selections, the nonce policy and `CSS.highlights` are only
    /// what they are in one. Skipped, with the reason, when no Chrome or
    /// Chromium is installed; `MARKVIEW_REQUIRE_BROWSER` makes that a failure.
    #[test]
    fn review_browser_harness_passes_in_headless_chrome() {
        let Some(browser) = headless_browser() else {
            assert!(
                std::env::var_os("MARKVIEW_REQUIRE_BROWSER").is_none(),
                "MARKVIEW_REQUIRE_BROWSER is set, but no Chrome or Chromium was found"
            );
            eprintln!(
                "skipped: no Chrome or Chromium found for tests/review.browser.js; set MARKVIEW_BROWSER to one"
            );
            return;
        };
        // A new profile each time: the page remembers a preference per origin.
        let dir = tempfile::tempdir().unwrap();
        let page = dir.path().join("harness.html");
        std::fs::write(&page, browser_fixture()).unwrap();
        let mut command = std::process::Command::new(&browser);
        command
            .arg("--headless=new")
            .args([
                "--disable-gpu",
                "--no-first-run",
                "--no-default-browser-check",
            ])
            .arg(format!(
                "--user-data-dir={}",
                dir.path().join("profile").display()
            ))
            // The harness waits on timers. Virtual time runs them at once.
            .args(["--virtual-time-budget=20000", "--dump-dom"])
            .arg(format!("file://{}", page.display()))
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        if std::env::var_os("CI").is_some() {
            // Hosted runners do not let Chrome build its sandbox. The only
            // page loaded is the fixture written above.
            command.arg("--no-sandbox");
        }
        let mut child = command.spawn().expect("start the browser");
        let mut stdout = child.stdout.take().unwrap();
        let (done, dumped) = mpsc::channel();
        std::thread::spawn(move || {
            let mut dom = String::new();
            let _ = std::io::Read::read_to_string(&mut stdout, &mut dom);
            let _ = done.send(dom);
        });
        let dom = dumped.recv_timeout(Duration::from_secs(120));
        let _ = child.kill();
        let _ = child.wait();
        let dom = dom.expect("the browser did not finish within two minutes");
        let verdict = dom
            .split("<output")
            .nth(1)
            .and_then(|rest| rest.split_once('>'))
            .and_then(|(_, rest)| rest.split("</output>").next())
            .unwrap_or_else(|| panic!("the harness did not run in {}", browser.display()));
        println!("{}: {verdict}", browser.display());
        assert!(verdict.starts_with("PASS"), "{verdict}");
    }
}
