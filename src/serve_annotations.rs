use std::collections::HashMap;
use std::io::{self, BufRead, BufReader};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use markview::annotations::{self, Error, Request, TextProjection, FORM_CAP};
use markview::DestinationKind;

use super::{
    broadcast_reload, decode_repeated, validated_served_path, write_response_headers, Clients,
    ReloadKind, ServeConfig, SharedConfig, WATCH_DEBOUNCE,
};

const ENDPOINT: &str = "/_markview/annotations";
const HEAD_CAP: usize = 32 * 1024;

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
    pub fn read(reader: &mut BufReader<TcpStream>, deadline: Instant) -> io::Result<Self> {
        let mut budget = HEAD_CAP;
        let line = read_line(reader, deadline, &mut budget)?;
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
    if !request.header("host").is_some_and(|host| {
        host == format!("localhost:{}", config.port) || host == format!("127.0.0.1:{}", config.port)
    }) {
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
            if request.header("x-markview-token") != config.review_token.as_deref() {
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

pub fn script(route: &str, token: Option<&str>) -> String {
    format!(
        "{}\n(() => {{
          const route = {};
          const token = {};
          const review = createMarkviewReview(document.querySelector('main'), document.body, async (form) => {{
            form.set('doc', route);
            const response = await fetch('{}', {{ method: 'POST',
              headers: {{'X-Markview-Token': token}}, body: form }});
            const data = await response.json();
            if (!response.ok) {{
              if (response.status === 409) window.markviewReviewRefresh();
              throw new Error(data.error);
            }}
            window.markviewReviewRefresh();
            return data;
          }});
          let requested = 0;
          window.markviewReviewRefresh = async () => {{
            const sequence = ++requested;
            const revision = document.querySelector('main').dataset.sourceRevision;
            review.show(route, revision, Boolean(token), Boolean(token));
            try {{
              const response = await fetch('{}?doc=' + encodeURIComponent(route), {{cache: 'no-store'}});
              const data = await response.json();
              if (sequence === requested && response.ok && revision === data.source_revision) review.update(data);
            }} catch {{ /* Live reload will retry on the next change. */ }}
          }};
          window.markviewReviewRoute = route;
          window.markviewReviewRefresh();
        }})();",
        annotations::REVIEW_SCRIPT,
        annotations::json_string(route),
        token.map_or("null".to_owned(), annotations::json_string),
        ENDPOINT,
        ENDPOINT,
    )
}

pub fn watch_stores(shared: SharedConfig, clients: Clients) -> notify::Result<()> {
    let (tx, rx) = mpsc::channel();
    let mut watcher = annotations::StoreWatcher::new(move |event| {
        if let Ok(event) = event {
            let _ = tx.send(event);
        }
    })?;
    let documents = || -> Vec<PathBuf> {
        shared
            .read()
            .expect("config lock")
            .documents
            .iter()
            .map(|doc| doc.source_path.clone())
            .collect()
    };
    watcher.sync(documents());
    std::thread::spawn(move || loop {
        let mut events = Vec::new();
        match rx.recv_timeout(Duration::from_millis(300)) {
            Ok(event) => {
                events.push(event);
                while let Ok(event) = rx.recv_timeout(WATCH_DEBOUNCE) {
                    events.push(event);
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        let config = shared.read().expect("config lock").clone();
        watcher.sync(config.documents.iter().map(|doc| doc.source_path.clone()));
        for changed in watcher.changed(&events) {
            if let Some(route) = config.document_route_for_path(&changed) {
                broadcast_reload(&clients, ReloadKind::Annotations(route.to_owned()));
            }
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let html =
            super::super::render_file(&config, config.default_document().unwrap(), Some(&nonce))
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
