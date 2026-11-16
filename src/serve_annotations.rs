use std::collections::{HashMap, HashSet};
use std::io::{self, BufRead, BufReader};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use markview::annotations::{self, Error, Request, FORM_CAP};
use markview::DestinationKind;

use super::{
    broadcast_reload, decode_repeated, serve_review, validated_served_path, write_response_headers,
    Clients, ReloadKind, ServeConfig, SharedConfig, WATCH_DEBOUNCE,
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
    // Routes are generated from the served allowlist, never joined to a path.
    let document = if route == "/" {
        config.default_document()?
    } else {
        config.document_by_route(route)?
    };
    let canonical = validated_served_path(&config.root, &document.source_path)?;
    if canonical != document.source_path {
        return None;
    }
    annotations::resolve(&canonical)
        .ok()
        .filter(|location| location.document == canonical)
}

fn read_review(
    config: &ServeConfig,
    location: &annotations::Location,
) -> Result<annotations::Review, Error> {
    let source = annotations::read_source(location)?;
    config.review_cache.read(location, &source)
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
    let body = if request.method == "POST" && initial.review_token.is_some() {
        if !token_matches(
            request.header("x-markview-token"),
            initial.review_token.as_deref().expect("checked token"),
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
            let Some(location) = document(&initial, &route) else {
                return fail(
                    stream,
                    "404 Not Found",
                    "document route or store is unavailable",
                );
            };
            read_review(&initial, &location).map(Some)
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

/// The review script and stylesheet, served from the reserved prefix so the
/// browser caches them instead of every page carrying ~18 KB inline.
pub fn static_asset(target: &str) -> Option<(&'static str, &'static str)> {
    match target.split('?').next() {
        Some("/_markview/review.js") => {
            Some(("text/javascript; charset=utf-8", annotations::REVIEW_SCRIPT))
        }
        Some("/_markview/review.css") => {
            Some(("text/css; charset=utf-8", annotations::REVIEW_STYLE))
        }
        Some("/_markview/copy.js") => {
            Some(("text/javascript; charset=utf-8", annotations::COPY_SCRIPT))
        }
        Some("/_markview/copy.css") => Some(("text/css; charset=utf-8", annotations::COPY_STYLE)),
        Some("/_markview/verdict.js") => Some(("text/javascript; charset=utf-8", VERDICT_SCRIPT)),
        _ => None,
    }
}

const VERDICT_SCRIPT: &str = include_str!("annotations/verdict.js");

/// The URLs carry a digest of the content, so a response can be cached for
/// good and a new markview build is still picked up at once.
pub const STATIC_CACHE: &str = "Cache-Control: public, max-age=31536000, immutable\r\n";

fn digest(bodies: &[&str]) -> String {
    annotations::source_revision(&bodies.concat())
}

/// Versions the review assets. Only pages that carry review hooks link them.
fn static_version() -> &'static str {
    static VERSION: OnceLock<String> = OnceLock::new();
    VERSION.get_or_init(|| {
        digest(&[
            annotations::REVIEW_SCRIPT,
            annotations::REVIEW_STYLE,
            VERDICT_SCRIPT,
        ])
    })
}

/// Versions the copy assets on their own. Every page links them, so a digest
/// that also covered the review scripts would change the bytes of a plain
/// page whenever review.js, review.css or verdict.js is edited.
fn copy_version() -> &'static str {
    static VERSION: OnceLock<String> = OnceLock::new();
    VERSION.get_or_init(|| digest(&[annotations::COPY_SCRIPT, annotations::COPY_STYLE]))
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
    let version = static_version();
    let verdict = if blocking {
        format!(r#"<script{nonce_attr} src="/_markview/verdict.js?v={version}"></script>"#)
    } else {
        String::new()
    };
    format!(
        "<link rel=\"stylesheet\" href=\"/_markview/review.css?v={version}\">
<script{nonce_attr} src=\"/_markview/review.js?v={version}\"></script>
{verdict}
<script{nonce_attr}>(() => {{
  const route = {};
  const token = {};
  markviewServeReview(route, token, {port}, {blocking});
}})();</script>",
        annotations::json_string(route),
        token.map_or("null".to_owned(), annotations::json_string),
    )
}

pub fn copy_hooks(nonce_attr: &str) -> String {
    let version = copy_version();
    format!(
        r#"<link rel="stylesheet" href="/_markview/copy.css?v={version}">
<script{nonce_attr} src="/_markview/copy.js?v={version}"></script>"#
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

    /// A blocking review shows counts for the whole collection, even when
    /// the reviewer has only opened one document.
    pub fn watch_collection(&self, config: &ServeConfig) {
        if let Ok(mut watcher) = self.watcher.lock() {
            for document in &config.documents {
                watcher.watch(&document.source_path);
            }
        }
    }

    /// Forgets documents that a rescan dropped from the served set.
    pub fn retain(&self, config: &ServeConfig) {
        let served: HashSet<&Path> = config
            .documents
            .iter()
            .map(|document| document.source_path.as_path())
            .collect();
        if let Ok(mut watcher) = self.watcher.lock() {
            watcher.retain(|path| served.contains(path));
        }
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
            assert_eq!(read_review(&config, &location).unwrap(), added);
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
            config.review_token = Some("token".to_owned());
            config.blocking_review = true;
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

    #[test]
    fn server_renders_normalized_safe_destinations_and_nonce_scripts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("review.md");
        let target = dir.path().join("target.md");
        std::fs::write(&path, SOURCE).unwrap();
        std::fs::write(&target, "# Target").unwrap();
        let mut config = ServeConfig::from_inputs(vec![path, target], 7878, false).unwrap();
        config.review_token = Some("test-only".to_owned());
        let nonce = annotations::random_hex().unwrap();
        let html = super::super::render_file(
            &config,
            config.default_document().unwrap(),
            Some(super::super::ReviewHooks {
                nonce: Some(&nonce),
                token: config.review_token.as_deref(),
                blocking_review: true,
            }),
        )
        .unwrap();
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
            r#"<script nonce="{nonce}" src="/_markview/review.js?v={}"></script>"#,
            static_version()
        )));
        if let Some(path) = std::env::var_os("MARKVIEW_REVIEW_BROWSER_FIXTURE") {
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
            for (name, source) in [
                ("copy.js", annotations::COPY_SCRIPT),
                ("review.js", annotations::REVIEW_SCRIPT),
                ("verdict.js", VERDICT_SCRIPT),
            ] {
                let version = if name == "copy.js" {
                    copy_version()
                } else {
                    static_version()
                };
                let link = format!(
                    r#"<script nonce="{nonce}" src="/_markview/{name}?v={version}"></script>"#
                );
                html = html.replace(
                    &link,
                    &format!(r#"<script nonce="{nonce}">{source}</script>"#),
                );
            }
            let harness = include_str!("../tests/review.browser.js");
            let html = html.replace(
                "</body>",
                &format!(r#"<script nonce="{nonce}">{harness}</script></body>"#),
            );
            std::fs::write(path, html).unwrap();
        }
    }
}
