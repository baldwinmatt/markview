use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;

use markview::{help, render, Cli, FrontendRenderer, HtmlRenderer, MarkdownDocument, OutputFormat};
use notify::{Config, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use pulldown_cmark::{Event, Options, Parser, Tag};

fn main() -> ExitCode {
    match run() {
        Ok(Some(output)) => {
            print!("{output}");
            ExitCode::SUCCESS
        }
        Ok(None) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("markview: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<Option<String>, Box<dyn std::error::Error>> {
    let cli = Cli::parse(std::env::args().skip(1))?;

    if cli.help {
        return Ok(Some(help().to_owned()));
    }

    if let Some(port) = cli.serve {
        if cli.inputs.is_empty() {
            return Err(markview::CliError::MissingServeInput.into());
        }
        let inputs = cli.inputs.iter().map(PathBuf::from).collect::<Vec<_>>();
        serve_markdown(inputs, port, cli.recurse)?;
        return Ok(None);
    }

    let markdown = match cli.input.as_deref() {
        Some(path) => fs::read_to_string(path)?,
        None => {
            let mut input = String::new();
            io::stdin().read_to_string(&mut input)?;
            input
        }
    };

    Ok(Some(match cli.output {
        OutputFormat::Terminal => render(&markdown, cli.options),
        OutputFormat::Html => {
            let document = cli
                .input
                .as_deref()
                .map(|path| MarkdownDocument::from_path(&markdown, path))
                .unwrap_or_else(|| MarkdownDocument::new(&markdown));
            HtmlRenderer.render_document(&document)
        }
    }))
}

#[derive(Debug, Clone)]
struct ServeConfig {
    root: PathBuf,
    documents: Vec<ServedDocument>,
    default_document: PathBuf,
    port: u16,
    mode: ServeMode,
}

#[derive(Debug, Clone)]
struct ServedDocument {
    source_path: PathBuf,
    route_path: String,
    title: String,
}

#[derive(Debug, Clone)]
struct ServedAsset {
    source_path: PathBuf,
    route_path: String,
    content_type: &'static str,
}

struct ServeBuild {
    root: PathBuf,
    documents: Vec<ServedDocument>,
    default_document: PathBuf,
    mode: ServeMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ServeMode {
    SingleFile,
    Explicit,
    Directory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NavLayout {
    Empty,
    Tabs,
    Sidebar,
}

fn serve_markdown(
    inputs: Vec<PathBuf>,
    port: u16,
    recurse: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut config = ServeConfig::from_inputs(inputs, port, recurse)?;
    let listener = TcpListener::bind(("127.0.0.1", port)).map_err(|error| {
        if error.kind() == io::ErrorKind::AddrInUse {
            format!("port {port} is already in use")
        } else {
            format!("failed to bind localhost:{port}: {error}")
        }
    })?;
    let address = listener.local_addr()?;
    config.port = address.port();
    let clients = Arc::new(Mutex::new(Vec::new()));
    let _watcher = watch_files(
        config
            .documents
            .iter()
            .map(|document| document.source_path.clone())
            .collect(),
        clients.clone(),
    )?;
    let config = Arc::new(config);

    println!(
        "Serving on http://localhost:{} — press Ctrl+C to stop",
        address.port()
    );
    io::stdout().flush()?;

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let config = config.clone();
                let clients = clients.clone();
                thread::spawn(move || {
                    if let Err(error) = handle_connection(stream, &config, clients) {
                        eprintln!("markview: serve error: {error}");
                    }
                });
            }
            Err(error) => eprintln!("markview: connection failed: {error}"),
        }
    }

    Ok(())
}

impl ServeConfig {
    fn from_inputs(
        inputs: Vec<PathBuf>,
        port: u16,
        recurse: bool,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        if inputs.is_empty() {
            return Err("serve requires an input file or directory".into());
        }
        let directory_inputs = inputs.iter().filter(|path| path.is_dir()).count();
        if directory_inputs > 0 && inputs.len() > 1 {
            return Err("cannot mix a served directory with explicit files".into());
        }
        if recurse && directory_inputs != 1 {
            return Err("--recurse requires a single served directory".into());
        }
        let build = if directory_inputs == 1 {
            Self::discover_directory(&inputs[0], recurse)?
        } else {
            Self::explicit_files(&inputs)?
        };
        let config = Self {
            root: build.root,
            documents: build.documents,
            default_document: build.default_document,
            port,
            mode: build.mode,
        };
        Ok(config)
    }

    fn explicit_files(inputs: &[PathBuf]) -> Result<ServeBuild, Box<dyn std::error::Error>> {
        let mut canonical_files = Vec::new();
        for input in inputs {
            let canonical = input.canonicalize()?;
            if !canonical.is_file() {
                return Err(format!("served input is not a file: {}", input.display()).into());
            }
            if !is_markdown_path(&canonical) {
                return Err(format!("served input is not Markdown: {}", input.display()).into());
            }
            if canonical_files.iter().any(|known| known == &canonical) {
                return Err("explicit served files contain duplicate or aliased paths".into());
            }
            canonical_files.push(canonical);
        }

        let root = if canonical_files.len() == 1 {
            canonical_files[0]
                .parent()
                .ok_or("served file has no parent directory")?
                .to_path_buf()
        } else {
            deepest_common_ancestor(&canonical_files)
                .ok_or("explicit served files have no common ancestor")?
        };
        if is_filesystem_root(&root) {
            return Err("explicit served files must share a non-root ancestor directory".into());
        }

        let mut documents = Vec::new();
        for canonical in canonical_files {
            if !canonical.starts_with(&root) {
                return Err("served file resolves outside the serve root".into());
            }
            documents.push(document_for_path(&root, canonical)?);
        }
        let default_document = documents
            .first()
            .ok_or("serve requires at least one Markdown file")?
            .source_path
            .clone();
        let mode = if documents.len() == 1 {
            ServeMode::SingleFile
        } else {
            ServeMode::Explicit
        };
        Ok(ServeBuild {
            root,
            documents,
            default_document,
            mode,
        })
    }

    fn discover_directory(input: &Path, recurse: bool) -> Result<ServeBuild, Box<dyn std::error::Error>> {
        let root = input.canonicalize()?;
        if is_filesystem_root(&root) {
            return Err("served directory cannot be a filesystem root".into());
        }
        let candidates = collect_markdown_paths(input, recurse)?;

        let mut collision_keys = Vec::new();
        let mut documents = Vec::new();
        for candidate in candidates {
            let relative = candidate.strip_prefix(input).unwrap_or(&candidate);
            let collision_key = relative.to_string_lossy().to_lowercase();
            if collision_keys.iter().any(|known| known == &collision_key) {
                return Err(format!(
                    "directory contains colliding Markdown filenames: {}",
                    relative.display()
                )
                .into());
            }
            collision_keys.push(collision_key);
            let canonical = candidate.canonicalize()?;
            if !canonical.starts_with(&root) {
                return Err(format!(
                    "served document resolves outside root: {}",
                    relative.display()
                )
                .into());
            }
            if documents
                .iter()
                .any(|document: &ServedDocument| document.source_path == canonical)
            {
                return Err(format!(
                    "directory contains duplicate or aliased Markdown file: {}",
                    relative.display()
                )
                .into());
            }
            documents.push(document_for_path(&root, canonical)?);
        }
        if documents.is_empty() {
            let scope = if recurse { "recursively" } else { "at its top level" };
            return Err(format!("served directory has no Markdown files {scope}").into());
        }

        let default_document = ["README.md", "index.md"]
            .iter()
            .find_map(|name| {
                documents
                    .iter()
                    .find(|document| {
                        document.source_path.parent() == Some(root.as_path())
                            && document
                                .source_path
                                .file_name()
                                .and_then(|file| file.to_str())
                                == Some(*name)
                    })
                    .map(|document| document.source_path.clone())
            })
            .unwrap_or_else(|| documents[0].source_path.clone());

        Ok(ServeBuild {
            root,
            documents,
            default_document,
            mode: ServeMode::Directory,
        })
    }

    fn default_document(&self) -> Option<&ServedDocument> {
        self.documents
            .iter()
            .find(|document| document.source_path == self.default_document)
    }

    fn document_by_route(&self, route: &str) -> Option<&ServedDocument> {
        self.documents
            .iter()
            .find(|document| document.route_path == route)
    }

    fn document_route_for_path(&self, path: &Path) -> Option<&str> {
        self.documents
            .iter()
            .find(|document| document.source_path == path)
            .map(|document| document.route_path.as_str())
    }

    fn asset_by_route(&self, route: &str) -> Option<ServedAsset> {
        self.scan_assets()
            .into_iter()
            .find(|asset| asset.route_path == route)
    }

    fn asset_for_path(&self, path: &Path) -> Option<ServedAsset> {
        self.scan_assets()
            .into_iter()
            .find(|asset| asset.source_path == path)
    }

    fn route_for_request(&self, request_path: &str) -> Option<String> {
        let path = request_path
            .split(['?', '#'])
            .next()
            .unwrap_or(request_path);
        if path == "/events" {
            return Some("/events".to_owned());
        }
        let decoded = percent_decode(path)?;
        if decoded.chars().any(|ch| ch == '\0' || ch.is_control()) {
            return None;
        }
        let repeated = decode_repeated(path)?;
        if repeated.chars().any(|ch| ch == '\0' || ch.is_control()) {
            return None;
        }
        if path_resolves_outside_root(&self.root, &repeated) {
            return None;
        }

        let decoded = decoded.trim_start_matches('/');
        if decoded.is_empty() {
            return Some("/".to_owned());
        }
        let candidate = self.root.join(decoded);
        let canonical = candidate.canonicalize().ok()?;
        if !canonical.starts_with(&self.root) {
            return None;
        }
        route_from_root(&self.root, &canonical)
    }

    fn scan_assets(&self) -> Vec<ServedAsset> {
        let mut assets = Vec::new();
        for document in &self.documents {
            if validated_served_path(&self.root, &document.source_path).is_none() {
                continue;
            }
            let Ok(source) = fs::read_to_string(&document.source_path) else {
                continue;
            };
            for reference in markdown_references(&source) {
                if is_external_or_absolute(&reference) || reference.starts_with('#') {
                    continue;
                }
                let Some(clean) = reference_without_suffix(&reference) else {
                    continue;
                };
                if clean.is_empty() || is_markdown_link(clean) {
                    continue;
                }
                let Some(decoded) = percent_decode(clean) else {
                    continue;
                };
                let base = document.source_path.parent().unwrap_or(&self.root);
                let candidate = base.join(decoded);
                let Ok(canonical) = candidate.canonicalize() else {
                    continue;
                };
                if !canonical.starts_with(&self.root) || !canonical.is_file() {
                    continue;
                }
                let Some(route_path) = route_from_root(&self.root, &canonical) else {
                    continue;
                };
                if is_markdown_path(&canonical)
                    && !self
                        .documents
                        .iter()
                        .any(|doc| doc.source_path == canonical)
                {
                    continue;
                }
                if assets
                    .iter()
                    .any(|asset: &ServedAsset| asset.source_path == canonical)
                {
                    continue;
                }
                assets.push(ServedAsset {
                    source_path: canonical,
                    route_path,
                    content_type: safe_content_type(clean),
                });
            }
        }
        assets
    }
}

fn document_for_path(root: &Path, canonical: PathBuf) -> io::Result<ServedDocument> {
    let route_path = route_from_root(root, &canonical).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "document does not resolve under serve root",
        )
    })?;
    let source = fs::read_to_string(&canonical)?;
    let title = first_heading_title(&source).unwrap_or_else(|| {
        canonical
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("Markdown")
            .to_owned()
    });
    Ok(ServedDocument {
        source_path: canonical,
        route_path,
        title,
    })
}

fn deepest_common_ancestor(paths: &[PathBuf]) -> Option<PathBuf> {
    let first_parent = paths.first()?.parent()?;
    let mut root = first_parent.to_path_buf();
    for path in &paths[1..] {
        let parent = path.parent()?;
        while !parent.starts_with(&root) {
            if !root.pop() {
                return None;
            }
        }
    }
    Some(root)
}

fn is_filesystem_root(path: &Path) -> bool {
    path.parent().is_none()
}

fn is_markdown_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "md" | "markdown" | "mdown"
            )
        })
}

fn is_markdown_link(path: &str) -> bool {
    is_markdown_path(Path::new(path))
}

fn collect_markdown_paths(start: &Path, recurse: bool) -> io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut pending = vec![start.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory)?.filter_map(Result::ok) {
            let hidden = entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with('.'));
            if hidden {
                continue;
            }
            let path = entry.path();
            if recurse && entry.file_type().is_ok_and(|file_type| file_type.is_dir()) {
                pending.push(path);
                continue;
            }
            if is_markdown_path(&path) {
                files.push(path);
            }
        }
    }
    files.sort_by_key(|path| relative_sort_key(start, path));
    Ok(files)
}

fn relative_sort_key(start: &Path, path: &Path) -> String {
    path.strip_prefix(start)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

fn route_from_root(root: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(root).ok()?;
    Some(relative_to_route(relative))
}

fn validated_served_path(root: &Path, path: &Path) -> Option<PathBuf> {
    let canonical = path.canonicalize().ok()?;
    if canonical == path && canonical.starts_with(root) {
        Some(canonical)
    } else {
        None
    }
}

fn path_resolves_outside_root(root: &Path, decoded_path: &str) -> bool {
    let decoded_path = decoded_path.trim_start_matches('/');
    if decoded_path.is_empty() {
        return false;
    }
    root.join(decoded_path)
        .canonicalize()
        .is_ok_and(|canonical| !canonical.starts_with(root))
}

fn relative_to_route(relative: &Path) -> String {
    let parts = relative
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(part) => {
                Some(percent_encode_route_segment(&part.to_string_lossy()))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    format!("/{}", parts.join("/"))
}

fn percent_encode_route_segment(segment: &str) -> String {
    let mut encoded = String::new();
    for byte in segment.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn decode_repeated(value: &str) -> Option<String> {
    let mut current = value.to_owned();
    for _ in 0..4 {
        let decoded = percent_decode(&current)?;
        if decoded == current {
            return Some(decoded);
        }
        current = decoded;
    }
    Some(current)
}

fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hi = *bytes.get(index + 1)?;
            let lo = *bytes.get(index + 2)?;
            output.push((hex_value(hi)? << 4) | hex_value(lo)?);
            index += 3;
        } else {
            output.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(output).ok()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn markdown_references(source: &str) -> Vec<String> {
    Parser::new_ext(source, Options::all())
        .filter_map(|event| match event {
            Event::Start(Tag::Link { dest_url, .. })
            | Event::Start(Tag::Image { dest_url, .. }) => Some(dest_url.to_string()),
            _ => None,
        })
        .collect()
}

fn first_heading_title(source: &str) -> Option<String> {
    let mut active = false;
    let mut title = String::new();
    for event in Parser::new_ext(source, Options::all()) {
        match event {
            Event::Start(Tag::Heading { .. }) => active = true,
            Event::End(pulldown_cmark::TagEnd::Heading(_)) if active => {
                let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
                return (!title.is_empty()).then_some(title);
            }
            Event::Text(text) | Event::Code(text) if active => {
                title.push_str(&text);
                title.push(' ');
            }
            Event::SoftBreak | Event::HardBreak if active => title.push(' '),
            _ => {}
        }
    }
    None
}

fn is_external_or_absolute(reference: &str) -> bool {
    let lower = reference.to_ascii_lowercase();
    lower.starts_with("http:")
        || lower.starts_with("https:")
        || lower.starts_with("mailto:")
        || lower.starts_with("file:")
        || reference.starts_with('/')
        || reference.contains(":\\")
}

fn reference_without_suffix(reference: &str) -> Option<&str> {
    let path = reference.split(['?', '#']).next().unwrap_or(reference);
    if path
        .bytes()
        .any(|byte| byte == b'\0' || byte.is_ascii_control())
    {
        None
    } else {
        Some(path)
    }
}

fn safe_content_type(path: &str) -> &'static str {
    match Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.to_ascii_lowercase())
        .as_deref()
    {
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("avif") => "image/avif",
        Some("ico") => "image/x-icon",
        Some("txt") | Some("log") => "text/plain; charset=utf-8",
        Some("pdf") => "application/pdf",
        Some("json") => "application/json",
        Some("csv") => "text/csv; charset=utf-8",
        Some("xml") => "application/xml",
        _ => "application/octet-stream",
    }
}

/// True for I/O errors that just mean the client went away (closed tab, page
/// reload, dropped tunnel), which happen routinely on a long-lived `/events`
/// stream and aren't worth logging as server errors.
fn is_client_disconnect(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::UnexpectedEof
    )
}

fn watch_files(
    paths: Vec<PathBuf>,
    clients: Arc<Mutex<Vec<mpsc::Sender<()>>>>,
) -> notify::Result<RecommendedWatcher> {
    let watch_paths = paths.clone();
    let (changes_tx, changes_rx) = mpsc::channel();
    let mut watcher = RecommendedWatcher::new(
        move |result: notify::Result<notify::Event>| {
            if let Ok(event) = result {
                if is_reload_event(&event.kind)
                    && event.paths.iter().any(|event_path| {
                        event_path
                            .canonicalize()
                            .map(|event_path| watch_paths.iter().any(|known| known == &event_path))
                            .unwrap_or(false)
                    })
                {
                    let _ = changes_tx.send(());
                }
            }
        },
        Config::default(),
    )?;

    let mut directories = Vec::new();
    for path in paths {
        let directory = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        if !directories.iter().any(|known| known == &directory) {
            watcher.watch(&directory, RecursiveMode::NonRecursive)?;
            directories.push(directory);
        }
    }
    thread::spawn(move || {
        while changes_rx.recv().is_ok() {
            if let Ok(mut clients) = clients.lock() {
                clients.retain(|client| client.send(()).is_ok());
            }
        }
    });

    Ok(watcher)
}

fn is_reload_event(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
    )
}

fn handle_connection(
    mut stream: TcpStream,
    config: &ServeConfig,
    clients: Arc<Mutex<Vec<mpsc::Sender<()>>>>,
) -> io::Result<()> {
    let mut request = String::new();
    {
        let mut reader = BufReader::new(stream.try_clone()?);
        reader.read_line(&mut request)?;
    }

    let mut parts = request.split_whitespace();
    let method = parts.next().unwrap_or("");
    let request_path = parts.next().unwrap_or("/");
    if !matches!(method, "GET" | "HEAD") {
        return write_response(
            &mut stream,
            "405 Method Not Allowed",
            "text/plain; charset=utf-8",
            "Method not allowed",
            method == "HEAD",
        );
    }
    let head_only = method == "HEAD";
    let route = config.route_for_request(request_path);
    let route = route.as_deref().unwrap_or("");
    match route {
        "/" => {
            if let Some(document) = config.default_document() {
                serve_document(&mut stream, config, document, head_only)
            } else {
                write_404(&mut stream, head_only)
            }
        }
        "/events" => {
            if head_only {
                write_bytes_response(&mut stream, "200 OK", "text/event-stream", &[], true)
            } else {
                match serve_events(stream, clients) {
                    Ok(()) => Ok(()),
                    Err(error) if is_client_disconnect(&error) => {
                        if std::env::var_os("MARKVIEW_LOG_EVENT_DISCONNECTS").is_some() {
                            eprintln!("markview: /events client disconnected");
                        }
                        Ok(())
                    }
                    Err(error) => Err(error),
                }
            }
        }
        _ => {
            if let Some(document) = config.document_by_route(route) {
                serve_document(&mut stream, config, document, head_only)
            } else if let Some(asset) = config.asset_by_route(route) {
                serve_asset(&mut stream, config, &asset, head_only)
            } else {
                write_404(&mut stream, head_only)
            }
        }
    }
}

fn serve_document(
    stream: &mut TcpStream,
    config: &ServeConfig,
    document: &ServedDocument,
    head_only: bool,
) -> io::Result<()> {
    match render_file(config, document) {
        Ok(html) => write_response(
            stream,
            "200 OK",
            "text/html; charset=utf-8",
            &html,
            head_only,
        ),
        Err(error) if error.kind() == io::ErrorKind::NotFound => write_404(stream, head_only),
        Err(error) => write_response(
            stream,
            "500 Internal Server Error",
            "text/plain; charset=utf-8",
            &format!("Failed to render document: {error}"),
            head_only,
        ),
    }
}

fn serve_asset(
    stream: &mut TcpStream,
    config: &ServeConfig,
    asset: &ServedAsset,
    head_only: bool,
) -> io::Result<()> {
    let Some(canonical) = validated_served_path(&config.root, &asset.source_path) else {
        return write_404(stream, head_only);
    };
    if canonical != asset.source_path {
        return write_404(stream, head_only);
    }
    match fs::read(&asset.source_path) {
        Ok(bytes) => write_bytes_response(stream, "200 OK", asset.content_type, &bytes, head_only),
        Err(_) => write_404(stream, head_only),
    }
}

fn serve_events(
    mut stream: TcpStream,
    clients: Arc<Mutex<Vec<mpsc::Sender<()>>>>,
) -> io::Result<()> {
    let (tx, rx) = mpsc::channel();
    clients
        .lock()
        .map_err(|_| io::Error::other("clients lock poisoned"))?
        .push(tx);
    stream.write_all(
        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nX-Content-Type-Options: nosniff\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\n\r\n",
    )?;
    stream.flush()?;

    let probe_disconnects = std::env::var_os("MARKVIEW_LOG_EVENT_DISCONNECTS").is_some();
    loop {
        if probe_disconnects {
            match rx.recv_timeout(std::time::Duration::from_millis(50)) {
                Ok(()) => stream.write_all(b"data: reload\n\n")?,
                Err(mpsc::RecvTimeoutError::Timeout) => stream.write_all(b": keepalive\n\n")?,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        } else if rx.recv().is_ok() {
            stream.write_all(b"data: reload\n\n")?;
        } else {
            break;
        }
        stream.flush()?;
    }

    Ok(())
}

fn render_file(config: &ServeConfig, served: &ServedDocument) -> io::Result<String> {
    if validated_served_path(&config.root, &served.source_path).is_none() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "served document no longer resolves under serve root",
        ));
    }
    let markdown = fs::read_to_string(&served.source_path)?;
    let title = title_for_source(&served.source_path, &markdown);
    let markdown = rewrite_markdown_links(config, served, &markdown);
    let document = MarkdownDocument::with_title(markdown, title);
    let modified_at = modified_timestamp_millis(&served.source_path);
    Ok(inject_serve_shell(
        config,
        &served.route_path,
        modified_at,
        &HtmlRenderer.render_document(&document),
    ))
}

fn rewrite_markdown_links(config: &ServeConfig, served: &ServedDocument, markdown: &str) -> String {
    let mut rewritten = String::new();
    let mut fence: Option<(char, usize)> = None;

    for line in markdown.split_inclusive('\n') {
        let line_without_newline = line.trim_end_matches(['\r', '\n']);
        let newline = &line[line_without_newline.len()..];
        let trimmed = line_without_newline.trim_start();

        if let Some(marker) = fence_marker(trimmed) {
            match fence {
                Some((open_ch, open_len)) if marker.0 == open_ch && marker.1 >= open_len => {
                    fence = None;
                }
                None => fence = Some(marker),
                _ => {}
            }
            rewritten.push_str(line);
            continue;
        }
        if fence.is_some() || line_without_newline.starts_with("    ") {
            rewritten.push_str(line);
            continue;
        }

        rewritten.push_str(&rewrite_reference_definition(
            config,
            served,
            &rewrite_inline_markdown_links(config, served, line_without_newline),
        ));
        rewritten.push_str(newline);
    }

    rewritten
}

/// Returns the fence character and run length for a code-fence marker line
/// (e.g. `(` `` ` ``, 4)` for ` ```` `). Per CommonMark, a fence is only closed
/// by a line using the *same* character with a run at least as long as the
/// opening fence, so callers must compare against the currently open fence
/// rather than treating every fence-looking line as a toggle.
fn fence_marker(trimmed: &str) -> Option<(char, usize)> {
    let ch = trimmed.chars().next()?;
    if ch != '`' && ch != '~' {
        return None;
    }
    let len = trimmed.chars().take_while(|&candidate| candidate == ch).count();
    (len >= 3).then_some((ch, len))
}

fn rewrite_inline_markdown_links(
    config: &ServeConfig,
    served: &ServedDocument,
    line: &str,
) -> String {
    let mut output = String::new();
    let mut index = 0;
    let mut in_code = false;

    while index < line.len() {
        let ch = line[index..].chars().next().expect("valid char boundary");
        let ch_len = ch.len_utf8();

        if ch == '`' {
            in_code = !in_code;
            output.push('`');
            index += ch_len;
            continue;
        }
        if in_code || ch != ']' || !line[index + ch_len..].starts_with('(') {
            output.push(ch);
            index += ch_len;
            continue;
        }

        let destination_start = index + ch_len + 1;
        let Some(destination_end) = find_link_destination_end(line, destination_start) else {
            output.push(ch);
            index += ch_len;
            continue;
        };
        let content = &line[destination_start..destination_end];
        let (destination, title) = split_destination_and_title(content);
        output.push_str("](");
        if let Some(target) = rewritten_reference(config, served, destination) {
            output.push_str(&target);
        } else {
            output.push_str(destination);
        }
        output.push_str(title);
        output.push(')');
        index = destination_end + 1;
    }

    output
}

fn rewrite_reference_definition(
    config: &ServeConfig,
    served: &ServedDocument,
    line: &str,
) -> String {
    let Some((label, rest)) = line.split_once("]:") else {
        return line.to_owned();
    };
    if !label.starts_with('[') || label.starts_with("[^") {
        return line.to_owned();
    }
    let leading = rest.len() - rest.trim_start().len();
    let rest_trimmed = rest.trim_start();
    let destination_end = rest_trimmed
        .find(char::is_whitespace)
        .unwrap_or(rest_trimmed.len());
    let destination = &rest_trimmed[..destination_end];
    let Some(target) = rewritten_reference(config, served, destination) else {
        return line.to_owned();
    };
    format!(
        "{}]:{}{}{}",
        label,
        " ".repeat(leading),
        target,
        &rest_trimmed[destination_end..]
    )
}

fn find_link_destination_end(line: &str, start: usize) -> Option<usize> {
    let mut escaped = false;
    let mut quote: Option<char> = None;
    for (offset, ch) in line[start..].char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if let Some(open) = quote {
            if ch == open {
                quote = None;
            }
            continue;
        }
        if ch == '"' || ch == '\'' {
            quote = Some(ch);
            continue;
        }
        if ch == ')' {
            return Some(start + offset);
        }
    }
    None
}

/// Splits a link's `(destination title)` content on the first unescaped
/// whitespace, so a title (e.g. `"See (details)"`) — including any parens it
/// contains — is kept intact and untouched rather than treated as part of the
/// destination path.
fn split_destination_and_title(content: &str) -> (&str, &str) {
    let mut escaped = false;
    for (offset, ch) in content.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if ch.is_whitespace() {
            return (&content[..offset], &content[offset..]);
        }
    }
    (content, "")
}

fn rewritten_reference(
    config: &ServeConfig,
    served: &ServedDocument,
    reference: &str,
) -> Option<String> {
    if is_external_or_absolute(reference) || reference.starts_with('#') || reference.contains('?') {
        return None;
    }
    let path_part = reference.split('#').next().unwrap_or(reference);
    let fragment = reference
        .split_once('#')
        .map(|(_, fragment)| format!("#{fragment}"))
        .unwrap_or_default();
    let decoded = percent_decode(path_part)?;
    let base = served.source_path.parent().unwrap_or(&config.root);
    let candidate = base.join(decoded);
    let canonical = candidate.canonicalize().ok()?;
    if !canonical.starts_with(&config.root) {
        return None;
    }
    if is_markdown_path(&canonical) {
        return config
            .document_route_for_path(&canonical)
            .map(|route| format!("{route}{fragment}"));
    }
    config
        .asset_for_path(&canonical)
        .map(|asset| format!("{}{}", asset.route_path, fragment))
}

fn render_nav(config: &ServeConfig, active_route: &str) -> String {
    let layout = nav_layout(config);
    if layout == NavLayout::Empty {
        return r#"<nav data-markview-nav></nav>"#.to_owned();
    }
    let class = match layout {
        NavLayout::Empty => unreachable!("empty nav returned above"),
        NavLayout::Tabs => "markview-tabs",
        NavLayout::Sidebar => "markview-sidebar",
    };
    let links = config
        .documents
        .iter()
        .map(|document| {
            let title = current_document_title(document);
            let active = if document.route_path == active_route {
                r#" aria-current="page""#
            } else {
                ""
            };
            format!(
                r#"<a href="{}" title="{}"{}>{}</a>"#,
                html_escape(&document.route_path),
                html_escape(&document.route_path),
                active,
                html_escape(&title)
            )
        })
        .collect::<Vec<_>>()
        .join("");
    format!(r#"<nav data-markview-nav class="{class}">{links}</nav>"#)
}

fn nav_layout(config: &ServeConfig) -> NavLayout {
    if config.documents.len() <= 1 {
        NavLayout::Empty
    } else if config.mode != ServeMode::Directory && config.documents.len() <= 6 {
        NavLayout::Tabs
    } else {
        NavLayout::Sidebar
    }
}

fn inject_serve_shell(
    config: &ServeConfig,
    active_route: &str,
    modified_at: Option<u128>,
    html: &str,
) -> String {
    let nav = render_nav(config, active_route);
    let modified_attr = modified_at
        .map(|millis| format!(r#" data-timestamp-ms="{millis}""#))
        .unwrap_or_default();
    let footer = format!(
        r#"<footer class="markview-footer">
<span>Served by <a href="https://github.com/baldwinmatt/markview">markview</a></span>
<span class="markview-footer-meta">
<span class="markview-modified">Doc modified <time data-markview-modified-at{modified_attr}></time></span>
<span class="markview-refreshed">Last refreshed <time data-markview-refreshed-at></time></span>
</span>
</footer>"#
    );
    let script = r#"<script>
(() => {
  const formatTime = (date) => date.toLocaleTimeString([], { hour: 'numeric', minute: '2-digit', second: '2-digit' });
  const updateModifiedTimes = () => {
    document.querySelectorAll('[data-markview-modified-at]').forEach((element) => {
      const millis = Number(element.dataset.timestampMs);
      if (!Number.isFinite(millis)) {
        element.textContent = 'unknown';
        return;
      }
      const modified = new Date(millis);
      element.dateTime = modified.toISOString();
      element.textContent = formatTime(modified);
    });
  };
  const updateRefreshTime = () => {
    const now = new Date();
    document.querySelectorAll('[data-markview-refreshed-at]').forEach((element) => {
      element.dateTime = now.toISOString();
      element.textContent = formatTime(now);
    });
  };
  updateModifiedTimes();
  updateRefreshTime();
  const events = new EventSource('/events');
  events.onmessage = async (event) => {
    if (event.data !== 'reload') return;
    const savedY = window.scrollY;
    const response = await fetch(location.pathname, { cache: 'no-store' });
    const text = await response.text();
    const next = new DOMParser().parseFromString(text, 'text/html');
    const currentMain = document.querySelector('main');
    const nextMain = next.querySelector('main');
    if (currentMain && nextMain) currentMain.innerHTML = nextMain.innerHTML;
    const currentNav = document.querySelector('[data-markview-nav]');
    const nextNav = next.querySelector('[data-markview-nav]');
    if (currentNav && nextNav) currentNav.innerHTML = nextNav.innerHTML;
    const currentFooter = document.querySelector('footer');
    const nextFooter = next.querySelector('footer');
    if (currentFooter && nextFooter) currentFooter.innerHTML = nextFooter.innerHTML;
    if (next.title) document.title = next.title;
    updateModifiedTimes();
    updateRefreshTime();
    window.scrollTo(0, savedY);
  };
})();
</script>"#;

    let style = r#"<style>
[data-markview-nav] {
  width: min(860px, calc(100vw - 48px));
  margin: 24px auto 0;
}
[data-markview-nav]:empty { display: none; }
.markview-shell {
  display: grid;
  grid-template-columns: 240px minmax(0, 860px);
  gap: 32px;
  width: min(1132px, calc(100vw - 48px));
  margin: 0 auto;
  align-items: start;
}
.markview-shell main {
  width: auto;
  margin: 0;
}
.markview-shell [data-markview-nav] {
  width: auto;
  margin: 40px 0 0;
}
.markview-footer {
  width: min(860px, calc(100vw - 48px));
  margin: -36px auto 40px;
  padding-top: 1rem;
  border-top: 1px solid var(--rule);
  color: var(--muted);
  font-size: 0.9rem;
  display: flex;
  justify-content: space-between;
  gap: 1rem;
  align-items: baseline;
}
.markview-refreshed {
  text-align: right;
  white-space: nowrap;
}
.markview-footer-meta {
  display: flex;
  flex-direction: column;
  gap: 0.2rem;
  text-align: right;
}
.markview-modified,
.markview-refreshed {
  white-space: nowrap;
}
.markview-tabs {
  display: flex;
  gap: 0.25rem;
  flex-wrap: wrap;
  border-bottom: 1px solid var(--rule);
}
.markview-tabs a {
  padding: 0.55rem 1rem;
  margin-bottom: -1px;
  border: 1px solid transparent;
  border-top-left-radius: 8px;
  border-top-right-radius: 8px;
  background: var(--code-bg);
  color: var(--muted);
  text-decoration: none;
  max-width: 12rem;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.markview-tabs a:hover {
  background: var(--quote-bg);
  color: var(--fg);
  border-color: var(--rule) var(--rule) transparent;
}
.markview-tabs a[aria-current="page"] {
  background: var(--bg);
  color: var(--fg);
  font-weight: 600;
  border-color: var(--rule) var(--rule) var(--bg);
}
.markview-sidebar {
  display: flex;
  flex-direction: column;
  gap: 2px;
  background: var(--code-bg);
  border-radius: 10px;
  padding: 14px 10px;
  position: sticky;
  top: 24px;
  align-self: start;
  max-height: calc(100vh - 48px);
  overflow-y: auto;
}
.markview-sidebar a {
  display: block;
  padding: 0.5rem 0.7rem;
  border-radius: 6px;
  color: var(--muted);
  text-decoration: none;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  transition: background-color 0.15s ease, color 0.15s ease;
}
.markview-sidebar a:hover {
  background: var(--quote-bg);
  color: var(--fg);
}
.markview-sidebar a[aria-current="page"] {
  background: var(--accent);
  color: var(--bg);
  font-weight: 600;
}
@media (max-width: 700px) {
  .markview-shell {
    display: block;
    width: min(860px, calc(100vw - 48px));
    margin: 0 auto;
  }
  .markview-shell main {
    width: auto;
    margin: 0;
  }
  .markview-sidebar {
    position: static;
    max-height: none;
    overflow: visible;
    flex-direction: row;
    flex-wrap: wrap;
    gap: 0.35rem;
    margin-bottom: 1.25rem;
  }
  .markview-sidebar a {
    max-width: 12rem;
  }
  .markview-footer {
    align-items: flex-start;
    flex-direction: column;
    gap: 0.35rem;
  }
  .markview-refreshed {
    text-align: left;
    white-space: normal;
  }
  .markview-footer-meta {
    text-align: left;
  }
  .markview-modified,
  .markview-refreshed {
    white-space: normal;
  }
}
</style>"#;
    let html = html.replace("</style>", &format!("</style>\n{style}"));
    let html = match nav_layout(config) {
        NavLayout::Sidebar => html
            .replacen(
                "<main>",
                &format!(r#"<div class="markview-shell">{nav}<main>"#),
                1,
            )
            .replacen("</main>", "</main></div>", 1),
        NavLayout::Empty | NavLayout::Tabs => html.replacen("<main>", &format!("{nav}\n<main>"), 1),
    };
    html.replace("</body>", &format!("{footer}\n{script}\n</body>"))
}

fn modified_timestamp_millis(path: &Path) -> Option<u128> {
    path.metadata()
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_millis())
}

fn html_escape(value: &str) -> String {
    let value = repair_utf8_mojibake(value);
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ if !ch.is_ascii() => {
                escaped.push_str("&#");
                escaped.push_str(&(ch as u32).to_string());
                escaped.push(';');
            }
            _ => escaped.push(ch),
        }
    }
    escaped
}

fn current_document_title(document: &ServedDocument) -> String {
    fs::read_to_string(&document.source_path)
        .map(|source| title_for_source(&document.source_path, &source))
        .unwrap_or_else(|_| document.title.clone())
}

fn title_for_source(path: &Path, source: &str) -> String {
    let title = first_heading_title(source).unwrap_or_else(|| {
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("Markdown")
            .to_owned()
    });
    repair_utf8_mojibake(&title)
}

fn repair_utf8_mojibake(value: &str) -> String {
    let mut repaired = value.to_owned();
    for _ in 0..3 {
        let mapped = replace_windows_1252_mojibake(&repaired);
        let decoded = decode_latin1_mojibake(&mapped).unwrap_or_else(|| mapped.clone());
        if decoded == repaired {
            return decoded;
        }
        repaired = decoded;
    }
    repaired
}

fn replace_windows_1252_mojibake(value: &str) -> String {
    let replacements = [
        ("\u{00E2}\u{20AC}\u{201D}", "—"),
        ("\u{00E2}\u{20AC}\u{201C}", "–"),
        ("\u{00E2}\u{20AC}\u{02DC}", "‘"),
        ("\u{00E2}\u{20AC}\u{2122}", "’"),
        ("\u{00E2}\u{20AC}\u{0153}", "“"),
        ("\u{00E2}\u{20AC}\u{009D}", "”"),
        ("\u{00E2}\u{20AC}\u{00A6}", "…"),
    ];
    let mut repaired = value.to_owned();
    for (bad, good) in replacements {
        repaired = repaired.replace(bad, good);
    }
    repaired
}

fn decode_latin1_mojibake(value: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(value.len());
    for ch in value.chars() {
        let codepoint = ch as u32;
        if codepoint > u8::MAX as u32 {
            return None;
        }
        bytes.push(codepoint as u8);
    }
    String::from_utf8(bytes).ok()
}

fn write_response(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &str,
    head_only: bool,
) -> io::Result<()> {
    write_bytes_response(stream, status, content_type, body.as_bytes(), head_only)
}

fn write_bytes_response(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &[u8],
    head_only: bool,
) -> io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nX-Content-Type-Options: nosniff\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    if !head_only {
        stream.write_all(body)?;
    }
    Ok(())
}

fn write_404(stream: &mut TcpStream, head_only: bool) -> io::Result<()> {
    write_response(
        stream,
        "404 Not Found",
        "text/plain; charset=utf-8",
        "Not found",
        head_only,
    )
}
