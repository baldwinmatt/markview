use std::collections::{HashMap, HashSet};
use std::io::{self, BufRead, BufReader};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::Instant;

use markview::annotations::{self, Error, Request, TextProjection, FORM_CAP};
use markview::DestinationKind;

use super::{
    broadcast_reload, decode_repeated, validated_served_path, write_response_headers, Clients,
    ReloadKind, ServeConfig, SharedConfig, WATCH_DEBOUNCE,
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

pub fn handle(
    stream: &mut TcpStream,
    reader: &mut BufReader<TcpStream>,
    config: &ServeConfig,
    request: &HttpRequest,
    deadline: Instant,
) -> io::Result<()> {
    let head = request.method == "HEAD";
    let fail = |stream: &mut TcpStream, status, message| {
        annotation_response(
            stream,
            status,
            &format!("{{\"error\":{}}}", annotations::json_string(message)),
            head,
        )
    };
    if !request
        .header("host")
        .is_some_and(|host| loopback_host(host, config.port))
    {
        return fail(stream, "403 Forbidden", "invalid Host");
    }
    let (path, query) = request
        .target
        .split_once('?')
        .unwrap_or((&request.target, ""));
    if path != ENDPOINT {
        return if matches!(request.method.as_str(), "GET" | "HEAD") {
            fail(stream, "404 Not Found", "unknown endpoint")
        } else {
            fail(stream, "405 Method Not Allowed", "method not allowed")
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
            let Some(location) = document(config, &route) else {
                return fail(
                    stream,
                    "404 Not Found",
                    "document route or store is unavailable",
                );
            };
            annotations::read_source(&location)
                .map_err(Error::from)
                .and_then(|source| annotations::read(&location, &TextProjection::new(&source)))
        }
        "POST" if config.review_token.is_some() => {
            let expected = config.review_token.as_deref().expect("checked token");
            if !token_matches(request.header("x-markview-token"), expected) {
                return fail(stream, "403 Forbidden", "invalid annotation token");
            }
            let length = request
                .header("content-length")
                .filter(|value| value.bytes().all(|byte| byte.is_ascii_digit()))
                .and_then(|value| value.parse::<usize>().ok());
            if !query.is_empty()
                || !matches!(length, Some(1..=FORM_CAP))
                || request.header("transfer-encoding").is_some()
                || !request.header("content-type").is_some_and(|value| {
                    value.split(';').next().is_some_and(|mime| {
                        mime.trim()
                            .eq_ignore_ascii_case("application/x-www-form-urlencoded")
                    })
                })
            {
                return fail(
                    stream,
                    "400 Bad Request",
                    "invalid annotation request framing",
                );
            }
            let Ok(bytes) = bounded_bytes(reader, deadline, length.expect("checked size"), false)
            else {
                return fail(stream, "400 Bad Request", "incomplete annotation request");
            };
            match Request::parse(&bytes) {
                Ok(form) => {
                    let Some(route) = form.doc else {
                        return fail(stream, "400 Bad Request", "expected a document route");
                    };
                    let Some(location) = document(config, &route) else {
                        return fail(
                            stream,
                            "404 Not Found",
                            "document route or store is unavailable",
                        );
                    };
                    annotations::mutate(&location, form.operation)
                }
                Err(error) => Err(error),
            }
        }
        _ => return fail(stream, "405 Method Not Allowed", "method not allowed"),
    };
    match result {
        Ok(review) => annotation_response(stream, "200 OK", &review.json(), head),
        Err(error) => {
            let status = match error {
                Error::Invalid(_) => "400 Bad Request",
                Error::Conflict(_) | Error::Section(..) => "409 Conflict",
                Error::Io(_) => "500 Internal Server Error",
            };
            let message = error.to_string();
            if status == "500 Internal Server Error" {
                eprintln!("markview: {message}");
                fail(stream, status, "annotation store is unavailable")
            } else {
                fail(stream, status, &message)
            }
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
        _ => None,
    }
}

/// The URLs carry a digest of the content, so a response can be cached for
/// good and a new markview build is still picked up at once.
pub const STATIC_CACHE: &str = "Cache-Control: public, max-age=31536000, immutable\r\n";

fn static_version() -> &'static str {
    static VERSION: OnceLock<String> = OnceLock::new();
    VERSION.get_or_init(|| {
        annotations::source_revision(
            &[annotations::REVIEW_SCRIPT, annotations::REVIEW_STYLE].concat(),
        )
    })
}

/// What a page needs to show annotations: the cached stylesheet and script,
/// and the call that connects them to this route. `nonce_attr` is the
/// ` nonce="..."` attribute a token-bearing page's policy requires.
pub fn hooks(route: &str, token: Option<&str>, port: u16, nonce_attr: &str) -> String {
    let version = static_version();
    format!(
        "<link rel=\"stylesheet\" href=\"/_markview/review.css?v={version}\">
<script{nonce_attr} src=\"/_markview/review.js?v={version}\"></script>
<script{nonce_attr}>(() => {{
  const route = {};
  const token = {};
  markviewServeReview(route, token, {port});
}})();</script>",
        annotations::json_string(route),
        token.map_or("null".to_owned(), annotations::json_string),
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
        "<div>\nliteral &amp;\n</div>\n\n",
        "- [x] task\n\n",
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

        // Pages link the cached script. The standalone fixture has no server
        // to fetch it from, so it carries the script inline instead.
        let external = format!(
            r#"<script nonce="{nonce}" src="/_markview/review.js?v={}"></script>"#,
            static_version()
        );
        assert!(html.contains(&external));
        let html = html.replacen(
            &external,
            &format!(
                r#"<script nonce="{nonce}">{}</script>"#,
                annotations::REVIEW_SCRIPT
            ),
            1,
        );

        if let Some(path) = std::env::var_os("MARKVIEW_REVIEW_BROWSER_FIXTURE") {
            let projection = TextProjection::new(SOURCE);
            let policy = format!("script-src 'nonce-{nonce}'; object-src 'none'; base-uri 'none'");
            let meta =
                format!("<meta http-equiv=\"Content-Security-Policy\" content=\"{policy}\">");
            let expected = annotations::json_string(&projection.text());
            let harness = include_str!("../tests/review.browser.js");
            let setup = format!(
                r#"<script nonce="{nonce}">const expectedText = {expected};
window.fetch = () => new Promise(() => {{}});
window.EventSource = class {{ addEventListener() {{}} }};
</script>
<script>window.untrustedScriptRan = true;</script>"#
            );
            let html = html
                .replacen("<head>", &format!("<head>{meta}{setup}"), 1)
                .replacen(
                    "</body>",
                    &format!(r#"<script nonce="{nonce}">{harness}</script></body>"#),
                    1,
                );
            std::fs::write(path, html).unwrap();
        }
    }
}
