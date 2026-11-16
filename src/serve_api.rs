use std::fmt::Write as _;
use std::io::{self, BufReader};
use std::net::TcpStream;
use std::time::Instant;

use markview::annotations::{self, json_string, push_json_string};

use super::serve_annotations::{
    annotations_endpoint, json_response, post_body, query_document, read_served_source,
    respond_error, review_endpoint, token_matches, Exchange, HttpRequest, Writes,
};
use super::{write_response_headers, ServeConfig, ServedDocument};

/// The version of this API, as the catalog reports it and the path spells it.
pub const API_VERSION: u32 = 1;
pub const PREFIX: &str = "/_markview/api/v1/";
/// The largest document `GET /document` will read, in bytes.
pub const DOCUMENT_CAP: u64 = 16 * 1024 * 1024;
/// How long a write to an API client may stall before the response is dropped.
pub const WRITE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
pub const TOKEN_ENV: &str = "MARKVIEW_API_TOKEN";

/// The variable's value, with an empty one counting as unset.
fn token_variable() -> Option<std::ffi::OsString> {
    std::env::var_os(TOKEN_ENV).filter(|value| !value.is_empty())
}

/// Whether the environment names a token at all, valid or not.
pub fn token_variable_set() -> bool {
    token_variable().is_some()
}

/// Fewer distinct digits than this and a 64-digit token is not random: a
/// repeated character or a short pattern. Random tokens fall below it with
/// odds of about one in a hundred million.
const MIN_DISTINCT_DIGITS: usize = 8;

fn token_problem(value: &str) -> Option<&'static str> {
    let valid = value.len() == 64
        && value
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'));
    if !valid {
        return Some("MARKVIEW_API_TOKEN must contain 64 lowercase hex digits");
    }
    let mut seen = [false; 128];
    value
        .bytes()
        .for_each(|byte| seen[usize::from(byte)] = true);
    (seen.iter().filter(|seen| **seen).count() < MIN_DISTINCT_DIGITS).then_some(
        "MARKVIEW_API_TOKEN is too predictable: use 64 random hex digits, for example `openssl rand -hex 32`",
    )
}

pub fn configured_token() -> io::Result<Option<String>> {
    let Some(value) = token_variable() else {
        return Ok(None);
    };
    let invalid = |message| io::Error::new(io::ErrorKind::InvalidInput, message);
    let value = value
        .into_string()
        .map_err(|_| invalid("MARKVIEW_API_TOKEN must contain 64 lowercase hex digits"))?;
    match token_problem(&value) {
        Some(message) => Err(invalid(message)),
        None => Ok(Some(value)),
    }
}

pub fn catalog(config: &ServeConfig) -> String {
    let default = config
        .default_document()
        .map_or_else(|| "null".to_owned(), |doc| json_string(&doc.route_path));
    let access = config.session.api_access();
    let (can_annotate, can_finish) = (access.is_some(), access.is_some_and(|a| a.finish));
    let mode = config.session.mode();
    let mut body = format!(
        r#"{{"api_version":{API_VERSION},"mode":"{mode}","default_document":{default},"can_annotate":{can_annotate},"can_finish":{can_finish},"documents":["#
    );
    for (index, doc) in config.documents.iter().enumerate() {
        if index > 0 {
            body.push(',');
        }
        let _ = write!(body, r#"{{"route":{}}}"#, json_string(&doc.route_path));
    }
    body.push_str("]}");
    body
}

/// What `GET /annotations` and every write answer: one review.
fn annotations_json(review: &annotations::Review) -> String {
    let mut output = String::with_capacity(128 + review.annotations.len() * 256);
    output.push_str(r#"{"source_revision":"#);
    push_json_string(&mut output, &review.source_revision);
    output.push_str(r#","store_revision":"#);
    push_json_string(&mut output, &review.store_revision);
    output.push_str(r#","annotations":["#);
    for (index, record) in review.annotations.iter().enumerate() {
        if index > 0 {
            output.push(',');
        }
        let _ = write!(
            output,
            r#"{{"id":"a{}","status":"{}","body":"#,
            record.id,
            record.status.label()
        );
        push_json_string(&mut output, &record.body);
        for (key, value) in [("quote", &record.quote), ("section", &record.section)] {
            let _ = write!(output, r#","{key}":"#);
            optional_json(&mut output, value.as_deref());
            if key == "quote" {
                output.push_str(r#","lines":"#);
                match record.lines {
                    Some((first, last)) => {
                        let _ = write!(output, "[{first},{last}]");
                    }
                    None => output.push_str("null"),
                }
            }
        }
        let _ = write!(output, r#","attached":{},"range":"#, record.attached);
        match &record.range {
            Some(range) => {
                let _ = write!(output, "[{},{}]", range.start, range.end);
            }
            None => output.push_str("null"),
        }
        output.push_str(r#","client_id":"#);
        optional_json(&mut output, record.client_id.as_deref());
        output.push('}');
    }
    output.push_str("]}");
    output
}

fn optional_json(output: &mut String, value: Option<&str>) {
    match value {
        Some(value) => push_json_string(output, value),
        None => output.push_str("null"),
    }
}

/// A document as `GET /document` serves it: read once, with its revision.
pub struct Loaded {
    pub location: annotations::Location,
    pub source: String,
    pub revision: String,
}

/// Reads the served document, refusing one larger than `DOCUMENT_CAP`.
/// Failures read as they do on the annotation endpoints.
pub fn load(config: &ServeConfig, doc: &ServedDocument) -> Result<Loaded, annotations::Error> {
    let (location, source) = read_served_source(config, doc, Some(DOCUMENT_CAP))?;
    let revision = annotations::source_revision(&source);
    Ok(Loaded {
        location,
        source,
        revision,
    })
}

/// The JSON body for a loaded document, with its projection taken from (or
/// added to) the cache the annotation endpoints share.
pub fn body(config: &ServeConfig, doc: &ServedDocument, loaded: &Loaded) -> String {
    let projection = config
        .review_cache
        .projection(&loaded.location, &loaded.source);
    let text = projection.text();
    let mut body = String::with_capacity(loaded.source.len() + text.len() + 256);
    body.push_str(r#"{"route":"#);
    push_json_string(&mut body, &doc.route_path);
    body.push_str(r#","title":"#);
    push_json_string(&mut body, &doc.title);
    body.push_str(r#","source_revision":"#);
    push_json_string(&mut body, &projection.revision);
    body.push_str(r#","markdown":"#);
    push_json_string(&mut body, &loaded.source);
    body.push_str(r#","projected_text":"#);
    push_json_string(&mut body, &text);
    body.push('}');
    body
}

/// Whether an `If-None-Match` header value names `revision`'s ETag.
pub fn etag_matches(header: &str, revision: &str) -> bool {
    header.split(',').any(|candidate| {
        let candidate = candidate.trim();
        let candidate = candidate.strip_prefix("W/").unwrap_or(candidate);
        candidate == "*"
            || candidate
                .strip_prefix('"')
                .and_then(|rest| rest.strip_suffix('"'))
                == Some(revision)
    })
}

/// The credential a write presents: `Authorization: Bearer <token>`, with
/// any number of spaces after the scheme.
fn bearer(request: &HttpRequest) -> Option<&str> {
    request
        .header("authorization")
        .and_then(|header| header.split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("Bearer"))
        .map(|(_, token)| token.trim_start_matches(' '))
}

/// Serves one request under `/_markview/api/v1/`. `route` is what follows
/// the prefix. Every route is answered here, from its credential to its
/// errors; the legacy endpoints share only the helpers.
pub(super) fn handle(
    stream: &mut TcpStream,
    reader: &mut BufReader<TcpStream>,
    exchange: &Exchange,
    route: &str,
    deadline: Instant,
) -> io::Result<()> {
    let Exchange {
        initial,
        request,
        session,
        query,
        ..
    } = exchange;
    // A client that stops reading must not hold a worker on a large body.
    stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
    // Any route this server does not offer is not there, whatever the
    // method; only a route that exists refuses a method.
    let offered = match route {
        "documents" | "document" | "annotations" => true,
        "review" => session.is_some(),
        _ => false,
    };
    if !offered {
        return exchange.fail(stream, "404 Not Found", "unknown endpoint");
    }
    if matches!(route, "documents" | "document") {
        if !matches!(request.method.as_str(), "GET" | "HEAD") {
            return exchange.fail(stream, "405 Method Not Allowed", "method not allowed");
        }
        return if route == "documents" {
            if !query.is_empty() {
                return exchange.fail(stream, "400 Bad Request", "unexpected documents query");
            }
            json_response(stream, "200 OK", &catalog(initial), exchange.head())
        } else {
            document(stream, exchange)
        };
    }
    let body = match request.method.as_str() {
        "POST" => {
            let access = initial
                .session
                .api_access()
                .filter(|access| route != "review" || access.finish);
            let Some(access) = access else {
                return exchange.fail(
                    stream,
                    "403 Forbidden",
                    "API writes are disabled: set MARKVIEW_API_TOKEN and use --annotate or --review",
                );
            };
            if !token_matches(bearer(request), access.token) {
                return exchange.fail(stream, "403 Forbidden", "invalid API token");
            }
            match post_body(reader, request, query, deadline) {
                Ok(body) => Some(body),
                Err(message) => return exchange.fail(stream, "400 Bad Request", message),
            }
        }
        _ => None,
    };
    match session {
        Some(session) if route == "review" => {
            review_endpoint(stream, exchange, session, body.as_deref())
        }
        _ => annotations_endpoint(
            stream,
            exchange,
            body.as_deref(),
            &Writes {
                revision: annotations::RevisionPolicy::Required,
                live_config: true,
                render: annotations_json,
            },
        ),
    }
}

/// `GET /document`: the source and its projection, or `304` when the
/// client's `If-None-Match` names the current revision.
fn document(stream: &mut TcpStream, exchange: &Exchange) -> io::Result<()> {
    let Exchange {
        initial,
        request,
        query,
        ..
    } = exchange;
    let Some(route) = query_document(query) else {
        return exchange.fail(stream, "400 Bad Request", "expected a document route");
    };
    let Some(document) = initial.document_for_route(&route) else {
        return exchange.fail(stream, "404 Not Found", "document is unavailable");
    };
    let loaded = match load(initial, document) {
        Ok(loaded) => loaded,
        Err(error) => return respond_error(stream, exchange, error, "document is unavailable"),
    };
    let headers = format!(
        "ETag: \"{}\"\r\nCache-Control: no-cache\r\n",
        loaded.revision
    );
    if request
        .header("if-none-match")
        .is_some_and(|value| etag_matches(value, &loaded.revision))
    {
        return write_response_headers(
            stream,
            "304 Not Modified",
            "application/json; charset=utf-8",
            b"",
            true,
            &headers,
        );
    }
    let body = body(initial, document, &loaded);
    write_response_headers(
        stream,
        "200 OK",
        "application/json; charset=utf-8",
        body.as_bytes(),
        exchange.head(),
        &headers,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{serve_annotations::watch_stores, Clients, SharedConfig};
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::sync::{Arc, Mutex};

    /// A plain `--serve` server run by `handle_connection` in this process,
    /// so a test can reach its config and the cache counters.
    struct LocalServe {
        _dir: tempfile::TempDir,
        port: u16,
        shared: SharedConfig,
    }

    impl LocalServe {
        fn start(connections: usize, files: &[(&str, &[u8])]) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let mut inputs = Vec::new();
            for (name, content) in files {
                let path = dir.path().join(name);
                std::fs::write(&path, content).unwrap();
                inputs.push(path);
            }
            let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
            let port = listener.local_addr().unwrap().port();
            let config = crate::ServeConfig::from_inputs(inputs, port, false).unwrap();
            let shared: SharedConfig = Arc::new(std::sync::RwLock::new(Arc::new(config)));
            let clients: Clients = Arc::new(Mutex::new(Vec::new()));
            let stores = watch_stores(shared.clone(), clients.clone()).unwrap();
            let server = shared.clone();
            std::thread::spawn(move || {
                for stream in listener.incoming().take(connections) {
                    let (shared, clients, stores) =
                        (server.clone(), clients.clone(), stores.clone());
                    std::thread::spawn(move || {
                        crate::handle_connection(stream.unwrap(), &shared, clients, &stores, None)
                    });
                }
            });
            Self {
                _dir: dir,
                port,
                shared,
            }
        }

        fn get(&self, target: &str, headers: &str) -> String {
            let port = self.port;
            let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
            write!(
                stream,
                "GET {target} HTTP/1.1\r\nHost: localhost:{port}\r\n{headers}\r\n"
            )
            .unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).unwrap();
            response
        }

        fn projections(&self) -> usize {
            self.shared.read().unwrap().review_cache.work().0
        }
    }

    fn revision_of(response: &str) -> String {
        response
            .split("\"source_revision\":\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .expect("source revision")
            .to_owned()
    }

    const DOC: &[u8] = b"# Title\n\nSome *text*.\n";

    #[test]
    fn document_and_annotation_reads_of_one_revision_build_one_projection() {
        let server = LocalServe::start(6, &[("doc.md", DOC)]);
        let document = "/_markview/api/v1/document?doc=%2Fdoc.md";
        assert!(server.get(document, "").starts_with("HTTP/1.1 200"));
        assert_eq!(
            server.projections(),
            1,
            "the document read bypassed the cache"
        );
        for _ in 0..2 {
            assert!(server.get(document, "").starts_with("HTTP/1.1 200"));
        }
        for _ in 0..3 {
            let annotations = server.get("/_markview/api/v1/annotations?doc=%2Fdoc.md", "");
            assert!(annotations.starts_with("HTTP/1.1 200"), "{annotations}");
        }
        assert_eq!(server.projections(), 1);
    }

    #[test]
    fn concurrent_cold_document_reads_build_the_projection_once() {
        let source = "# Big\n\n".to_owned() + &"words and *more* words\n\n".repeat(20_000);
        let server = LocalServe::start(8, &[("doc.md", source.as_bytes())]);
        let barrier = Arc::new(std::sync::Barrier::new(8));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let (port, barrier) = (server.port, barrier.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
                    write!(stream, "GET /_markview/api/v1/document?doc=%2Fdoc.md HTTP/1.1\r\nHost: localhost:{port}\r\n\r\n").unwrap();
                    let mut response = String::new();
                    stream.read_to_string(&mut response).unwrap();
                    response.starts_with("HTTP/1.1 200")
                })
            })
            .collect();
        assert!(threads.into_iter().all(|thread| thread.join().unwrap()));
        assert_eq!(server.projections(), 1);
    }

    #[test]
    fn a_document_over_the_cap_is_refused_before_it_is_read_or_projected() {
        let server = LocalServe::start(1, &[("doc.md", b"# Small\n")]);
        let path = server.shared.read().unwrap().documents[0]
            .source_path
            .clone();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(DOCUMENT_CAP + 1)
            .unwrap();
        let response = server.get("/_markview/api/v1/document?doc=%2Fdoc.md", "");
        assert!(response.starts_with("HTTP/1.1 413"), "{response}");
        assert!(response.contains(r#"{"error":"#), "{response}");
        assert_eq!(server.projections(), 0);
    }

    #[test]
    fn a_matching_if_none_match_is_answered_without_building_anything() {
        let server = LocalServe::start(3, &[("doc.md", DOC)]);
        let target = "/_markview/api/v1/document?doc=%2Fdoc.md";
        let first = server.get(target, "");
        let revision = revision_of(&first);
        assert!(first.contains(&format!("ETag: \"{revision}\"")), "{first}");
        assert!(first.contains("Cache-Control: no-cache"), "{first}");
        let builds = server.projections();
        let conditional = server.get(target, &format!("If-None-Match: \"{revision}\"\r\n"));
        assert!(conditional.starts_with("HTTP/1.1 304"), "{conditional}");
        assert!(
            conditional.contains(&format!("ETag: \"{revision}\"")),
            "{conditional}"
        );
        assert!(!conditional.contains("markdown"), "{conditional}");
        assert_eq!(server.projections(), builds);
        let other = server.get(target, "If-None-Match: \"0000000000000000\"\r\n");
        assert!(other.starts_with("HTTP/1.1 200"), "{other}");
    }
}
