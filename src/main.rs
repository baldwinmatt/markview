mod serve_annotations;
mod serve_api;
mod serve_review;

use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
#[cfg(unix)]
use std::os::fd::{AsFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, RwLock};
use std::thread;
use std::time::{Duration, SystemTime};

use markview::annotations;
use markview::{
    help, percent_decode, render, repair_utf8_mojibake, Cli, FrontendRenderer, HtmlRenderer,
    MarkdownDocument, OutputFormat, ALERT_DARK_VARIABLES, ALERT_LIGHT_VARIABLES,
    DIFF_DARK_VARIABLES, DIFF_LIGHT_VARIABLES, THEME_DARK_VARIABLES, THEME_LIGHT_VARIABLES,
};
use notify::{event::ModifyKind, Config, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use pulldown_cmark::{Event, Parser, Tag};

fn main() -> ExitCode {
    match run() {
        Ok((output, status)) => {
            print!("{output}");
            status
        }
        Err(error) => {
            eprintln!("markview: {error}");
            ExitCode::FAILURE
        }
    }
}

/// What to print on stdout and how to exit. A run can print useful output
/// and still fail, as an annotation listing does past a broken document.
fn run() -> Result<(String, ExitCode), Box<dyn std::error::Error>> {
    let cli = Cli::parse(std::env::args().skip(1))?;

    if cli.help {
        return Ok((help().to_owned(), ExitCode::SUCCESS));
    }

    if cli.annotations {
        return list_annotations(&cli.inputs, cli.include_resolved);
    }

    if let Some(port) = cli.review {
        let (session, result) = serve_review::Session::new();
        let result = serve_markdown(
            cli.inputs.iter().map(PathBuf::from).collect(),
            port,
            cli.recurse,
            !cli.no_open,
            true,
            Some((session, result)),
        )?
        .expect("review session returns a verdict");
        let status = if cli.gate && result.changes {
            ExitCode::from(2)
        } else {
            ExitCode::SUCCESS
        };
        return Ok((result.output, status));
    }

    if let Some(port) = cli.serve {
        if cli.inputs.is_empty() {
            return Err(markview::CliError::MissingServeInput.into());
        }
        let inputs = cli.inputs.iter().map(PathBuf::from).collect::<Vec<_>>();
        if cli.daemon {
            serve_markdown_daemon(inputs, port, cli.recurse, cli.open, cli.annotate)?;
        } else {
            serve_markdown(inputs, port, cli.recurse, cli.open, cli.annotate, None)?;
        }
        return Ok((String::new(), ExitCode::SUCCESS));
    }

    let markdown = match cli.input.as_deref() {
        Some(path) => fs::read_to_string(path)?,
        None => {
            let mut input = String::new();
            io::stdin().read_to_string(&mut input)?;
            input
        }
    };

    let output = match cli.output {
        OutputFormat::Terminal => render(&markdown, cli.options),
        OutputFormat::Html => {
            let document = cli
                .input
                .as_deref()
                .map(|path| MarkdownDocument::from_path(&markdown, path))
                .unwrap_or_else(|| MarkdownDocument::new(&markdown));
            HtmlRenderer.render_document(&document)
        }
    };
    Ok((output, ExitCode::SUCCESS))
}

/// Lists annotations for files and directories. A document that cannot be
/// listed is reported on stderr as `error: <document>: <problem>` and the
/// rest are still listed; the exit status then says the listing is incomplete.
fn list_annotations(
    inputs: &[String],
    include_resolved: bool,
) -> Result<(String, ExitCode), Box<dyn std::error::Error>> {
    // Store paths are canonical. Shown relative to the working directory
    // they can be pasted straight into an editor or another command.
    let cwd = std::env::current_dir()
        .and_then(|cwd| cwd.canonicalize())
        .ok();
    let relative = |path: &Path| -> String {
        cwd.as_deref()
            .and_then(|cwd| path.strip_prefix(cwd).ok())
            .unwrap_or(path)
            .display()
            .to_string()
    };
    let mut output = String::new();
    let mut failed = false;
    let mut seen = std::collections::HashSet::new();
    for input in inputs {
        let path = Path::new(input);
        let is_dir = path
            .metadata()
            .map_err(|error| format!("{input}: {error}"))?
            .is_dir();
        if is_annotation_store(path) {
            eprintln!("error: {input}: this is an annotation store, not a document; pass the Markdown file it annotates");
            failed = true;
            continue;
        }
        let paths = if is_dir {
            collect_markdown_paths(path, true)?
        } else {
            vec![path.to_owned()]
        };
        let mut has_stores = false;
        for file in paths {
            if !is_markdown_path(&file) {
                return Err(format!("input is not Markdown: {}", file.display()).into());
            }
            match document_annotations(&file, is_dir, include_resolved, &relative, &mut seen) {
                Ok(Some(listing)) => {
                    has_stores = true;
                    output.push_str(&listing);
                }
                Ok(None) => {}
                Err(problem) => {
                    has_stores = true;
                    failed = true;
                    eprintln!("error: {}: {problem}", file.display());
                }
            }
        }
        if !has_stores {
            let _ = writeln!(output, "No open annotations for {input}.");
        }
    }
    let status = if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    };
    Ok((output, status))
}

/// Whether `path` lies in a `.markview/annotations` store. Listing one would
/// resolve a second store for the store file itself.
fn is_annotation_store(path: &Path) -> bool {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_owned());
    let parts: Vec<_> = canonical.iter().collect();
    parts
        .windows(2)
        .any(|pair| pair[0] == ".markview" && pair[1] == "annotations")
}

/// The listing for one document, or `None` when it has nothing to list: no
/// store under a directory input, or a document already listed. The error
/// says what is wrong and names the store when the store is at fault.
fn document_annotations(
    file: &Path,
    from_directory: bool,
    include_resolved: bool,
    relative: &dyn Fn(&Path) -> String,
    seen: &mut std::collections::HashSet<PathBuf>,
) -> Result<Option<String>, String> {
    let location = annotations::resolve(file).map_err(|error| error.to_string())?;
    let store = relative(&location.path);
    let in_store = |error: &dyn std::fmt::Display| format!("{store}: {error}");
    let bytes = annotations::snapshot(&location).map_err(|error| in_store(&error))?;
    if from_directory && bytes.is_none() {
        return Ok(None);
    }
    if !seen.insert(location.document.clone()) {
        return Ok(None);
    }
    let source = fs::read_to_string(&location.document).map_err(|error| {
        if error.kind() == io::ErrorKind::InvalidData {
            "the document is not valid UTF-8".to_owned()
        } else {
            error.to_string()
        }
    })?;
    let review = annotations::read(&location, &annotations::TextProjection::new(&source))
        .map_err(|error| in_store(&error))?;
    for warning in annotations::store_warnings(&location, bytes.as_deref().unwrap_or_default()) {
        let line = warning
            .line
            .map_or(String::new(), |line| format!(":{line}"));
        eprintln!("warning: {store}{line}: {}", warning.message);
    }

    let mut output = String::new();
    format_annotations(
        &mut output,
        file,
        Path::new(&store),
        &review,
        include_resolved,
    );
    Ok(Some(output))
}

fn format_annotations(
    output: &mut String,
    file: &Path,
    store: &Path,
    review: &annotations::Review,
    include_resolved: bool,
) {
    let _ = writeln!(
        output,
        "# Annotations: {}\nStore: {} (change `[open]` to `[resolved]` there once addressed)\n",
        file.display(),
        store.display()
    );
    let resolved = review
        .annotations
        .iter()
        .filter(|record| record.status == annotations::Status::Resolved)
        .count();
    if resolved == review.annotations.len() {
        let _ = writeln!(output, "No open annotations for {}.\n", file.display());
    }
    for record in &review.annotations {
        if record.status == annotations::Status::Resolved && !include_resolved {
            continue;
        }
        let _ = write!(output, "## a{} · ", record.id);
        match record.lines {
            Some((first, last)) if first == last => {
                let _ = write!(output, "line {first}");
            }
            Some((first, last)) => {
                let _ = write!(output, "lines {first}-{last}");
            }
            None => output.push_str("document"),
        }
        if let Some(section) = &record.section {
            let _ = write!(output, " · {section}");
        }
        if !record.attached {
            output.push_str(" · detached");
        }
        if record.status == annotations::Status::Resolved {
            output.push_str(" · resolved");
        }
        output.push('\n');
        if let Some(quote) = &record.quote {
            let _ = writeln!(output, "> {quote}");
        }
        let _ = writeln!(output, "\n{}\n", record.body);
    }
    if !include_resolved && resolved > 0 {
        let label = if resolved == 1 {
            "annotation"
        } else {
            "annotations"
        };
        let _ = writeln!(output, "{resolved} resolved {label} not shown.\n");
    }
}

#[derive(Debug)]
struct ServeConfig {
    root: PathBuf,
    documents: Vec<ServedDocument>,
    default_document: PathBuf,
    port: u16,
    /// What was decided when the server started. A rescan builds a fresh
    /// config and carries this across whole.
    session: ServeSession,
    /// Projections and resolved reviews of recently requested documents.
    review_cache: annotations::ReviewCache,
    /// What the blocking review's verdict bar shows for the collection.
    review_summary: serve_review::SummaryCache,
    /// Directories under the root that are not served because their route
    /// would fall under the reserved `/_markview/` prefix.
    reserved: Vec<PathBuf>,
    mode: ServeMode,
    sidebar_nav: Option<NavDir>,
    /// Memoized `scan_assets()` result, invalidated by comparing each served
    /// document's mtime against the snapshot taken when the cache was built.
    /// `ServeConfig` itself is swapped out wholesale on a rescan (see
    /// `SharedConfig`), so this only needs to survive edits to already-known
    /// documents within one config generation, not additions/removals.
    asset_cache: Mutex<Option<AssetCache>>,
}

/// The part of a `ServeConfig` that belongs to the running server rather
/// than to one scan of the inputs. `publish_rescan` copies the struct, not
/// its fields, so a field added here cannot be forgotten there.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ServeSession {
    /// The write token of `--annotate` and `--review`.
    review_token: Option<String>,
    /// The independently configured credential for versioned API writes.
    api_token: Option<String>,
    /// Whether this is `--review`, which watches every document's store.
    blocking_review: bool,
    /// Which scan published this config. Later scans have higher numbers.
    rescan_id: u64,
}

/// What the API's credential is allowed to do.
struct ApiAccess<'a> {
    token: &'a str,
    /// Whether it may also give the review's verdict.
    finish: bool,
}

impl ServeSession {
    /// The API's write access, or `None` while its writes are disabled. The
    /// catalog reports it and the write gate enforces it, from this alone.
    fn api_access(&self) -> Option<ApiAccess<'_>> {
        self.api_token.as_deref().map(|token| ApiAccess {
            token,
            finish: self.blocking_review,
        })
    }
}

#[derive(Debug)]
struct AssetCache {
    signature: Vec<SystemTime>,
    assets: Arc<[ServedAsset]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
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
    reserved: Vec<PathBuf>,
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

/// What kind of live-reload signal to push to connected browser tabs.
/// `Content` covers a plain edit to an already-known document: the client
/// can just patch `main`/nav/footer in place. `Structure` means a rescan
/// changed the served document set, which can also change the nav's overall
/// layout (e.g. single-file "no nav" becoming a real sidebar) — the
/// surrounding page shell (the nav's own class, the two-column wrapper) was
/// only ever set at full-page-load time, so an in-place patch can't apply
/// that change and a full page reload is needed instead.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ReloadKind {
    Annotations(String),
    ReviewFinished,
    Content,
    Structure,
}

/// The active `ServeConfig`, swappable so the file watcher can replace it
/// wholesale after a rescan (e.g. a Markdown file was added or removed under
/// the served root) without connection threads ever seeing a torn read.
type SharedConfig = Arc<RwLock<Arc<ServeConfig>>>;
static NEXT_RESCAN_ID: AtomicU64 = AtomicU64::new(1);
const WATCH_DEBOUNCE: Duration = Duration::from_millis(50);
/// How often an idle `/events` connection wakes up to send an SSE comment
/// keepalive. This is also the bound on how quickly a browser navigating away
/// (closing its `EventSource`) is noticed and pruned from `clients` — without
/// it the handler thread would block on `rx.recv()` forever, since a broken
/// pipe is only ever discovered on the next write.
const EVENTS_KEEPALIVE: Duration = Duration::from_secs(15);

/// Live `/events` subscribers, each tagged with a unique id so a connection
/// can remove exactly its own entry when its handler thread exits — without
/// this, a client's `Sender` would only ever be pruned by `broadcast_reload`,
/// which doesn't run at all while no served file changes on disk.
type Clients = Arc<Mutex<Vec<(u64, mpsc::Sender<ReloadKind>)>>>;
static NEXT_CLIENT_ID: AtomicU64 = AtomicU64::new(0);

/// Removes this connection's entry from `clients` when dropped, so it happens
/// on every exit path out of `serve_events` (clean disconnect, write error,
/// or the `?` operator) rather than only on the next broadcast.
struct ClientGuard {
    clients: Clients,
    id: u64,
}

impl Drop for ClientGuard {
    fn drop(&mut self) {
        retain_clients(&self.clients, |(id, _)| *id != self.id);
    }
}

/// Locks `clients` and applies `keep` via `retain`, doing nothing on a
/// poisoned lock. The shared shape behind both `ClientGuard::drop` (removing
/// exactly one id) and `broadcast_reload` (dropping every id whose send
/// failed), so a future third call site doesn't reimplement lock-and-retain
/// a third time with possibly-different poisoning behavior.
fn retain_clients(clients: &Clients, keep: impl FnMut(&(u64, mpsc::Sender<ReloadKind>)) -> bool) {
    if let Ok(mut clients) = clients.lock() {
        clients.retain(keep);
    }
}

fn serve_markdown(
    inputs: Vec<PathBuf>,
    port: u16,
    recurse: bool,
    open: bool,
    annotate: bool,
    review: Option<(
        Arc<serve_review::Session>,
        mpsc::Receiver<io::Result<serve_review::AcceptedReview>>,
    )>,
) -> Result<Option<serve_review::AcceptedReview>, Box<dyn std::error::Error>> {
    let mut config = ServeConfig::from_inputs(inputs.clone(), port, recurse)?;
    config.session.blocking_review = review.is_some();
    config.session.review_token = annotate.then(annotations::random_hex).transpose()?;
    if annotate {
        config.session.api_token = serve_api::configured_token()?;
    }
    let api_writes = config.session.api_token.is_some();
    // Said once, at startup: a rescan builds a fresh config but stays quiet.
    for directory in &config.reserved {
        eprintln!(
            "markview: warning: not serving {}: the /_markview/ route prefix is reserved",
            directory.display()
        );
    }
    #[cfg(unix)]
    let listener = if review.is_none() && std::env::var_os("MARKVIEW_INHERITED_LISTENER").is_some()
    {
        let listener = TcpListener::from(io::stdin().as_fd().try_clone_to_owned()?);
        if listener.local_addr()?.port() != port {
            return Err("inherited listener does not match --port".into());
        }
        listener
    } else {
        bind_serve_listener(port)?
    };
    #[cfg(not(unix))]
    let listener = bind_serve_listener(port)?;
    let address = listener.local_addr()?;
    config.port = address.port();
    let bound_port = config.port;
    let clients = Arc::new(Mutex::new(Vec::new()));
    let shared: SharedConfig = Arc::new(RwLock::new(Arc::new(config)));
    let stores = serve_annotations::watch_stores(shared.clone(), clients.clone())?;
    stores.sync(&shared);
    let _watcher = watch_root(
        shared.clone(),
        inputs,
        bound_port,
        recurse,
        clients.clone(),
        stores.clone(),
    )?;

    if review.is_some() {
        eprintln!("Waiting for review at {}", serve_open_url(bound_port));
    } else {
        println!("Serving on http://localhost:{bound_port} — press Ctrl+C to stop");
        io::stdout().flush()?;
    }
    if api_writes {
        eprintln!("markview: API writes enabled ({})", serve_api::TOKEN_ENV);
    } else if !annotate && serve_api::token_variable_set() {
        eprintln!(
            "markview: {} is set but ignored: API writes need --annotate or --review",
            serve_api::TOKEN_ENV
        );
    }

    if open {
        let url = serve_open_url(address.port());
        if review.is_some() {
            thread::spawn(move || open_browser(&url));
        } else {
            open_browser(&url);
        }
    }

    let Some((session, verdict)) = review else {
        return Err(accept_connections(&listener, &shared, &clients, &stores, None).into());
    };
    // Connections are accepted on their own thread, so this one can sleep
    // until there is a verdict instead of polling a nonblocking listener.
    thread::spawn(move || {
        let lost = accept_connections(&listener, &shared, &clients, &stores, Some(&session));
        session.fail(lost);
    });
    let result = verdict.recv()??;
    // Give SSE clients a bounded window to receive the event, while the
    // acceptor still returns conflicts to concurrent writers.
    thread::sleep(REVIEW_SHUTDOWN_GRACE);
    Ok(Some(result))
}

const REVIEW_SHUTDOWN_GRACE: Duration = Duration::from_millis(150);
const ACCEPT_BACKOFF: Duration = Duration::from_millis(10);

/// What a failed `accept` says about the listener.
#[derive(Debug, PartialEq, Eq)]
enum AcceptFailure {
    /// A signal arrived. Nothing is wrong.
    Interrupted,
    /// This one connection failed, for example it was reset while queued.
    Connection,
    /// The process or system is out of descriptors or memory. Connections
    /// stay queued, so waiting for some to close is enough.
    Exhausted,
    /// The listening socket itself is gone. No later `accept` can succeed.
    Lost,
}

fn accept_failure(error: &io::Error) -> AcceptFailure {
    if error.kind() == io::ErrorKind::Interrupted {
        return AcceptFailure::Interrupted;
    }
    #[cfg(unix)]
    match error.raw_os_error() {
        Some(libc::EMFILE | libc::ENFILE | libc::ENOBUFS | libc::ENOMEM) => {
            return AcceptFailure::Exhausted
        }
        Some(libc::EBADF | libc::EINVAL | libc::ENOTSOCK | libc::EOPNOTSUPP) => {
            return AcceptFailure::Lost
        }
        _ => {}
    }
    AcceptFailure::Connection
}

/// Serves connections until the listener is lost, and returns why. A failed
/// `accept` otherwise never ends the server: under `--review` that would
/// throw away the verdict the command exists to wait for.
fn accept_connections(
    listener: &TcpListener,
    shared: &SharedConfig,
    clients: &Clients,
    stores: &Arc<serve_annotations::Stores>,
    session: Option<&Arc<serve_review::Session>>,
) -> io::Error {
    // Said once per shortage, not once per 10 ms retry.
    let mut exhausted = false;
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                exhausted = false;
                let shared = shared.clone();
                let clients = clients.clone();
                let session = session.cloned();
                let stores = stores.clone();
                thread::spawn(move || {
                    if let Err(error) =
                        handle_connection(stream, &shared, clients, &stores, session)
                    {
                        eprintln!("markview: serve error: {error}");
                    }
                });
            }
            Err(error) => match accept_failure(&error) {
                AcceptFailure::Interrupted => {}
                AcceptFailure::Connection => eprintln!("markview: connection failed: {error}"),
                AcceptFailure::Exhausted => {
                    if !exhausted {
                        eprintln!("markview: connection failed: {error}; retrying");
                    }
                    exhausted = true;
                    thread::sleep(ACCEPT_BACKOFF);
                }
                AcceptFailure::Lost => return error,
            },
        }
    }
}

fn open_browser(url: &str) {
    if let Err(error) = open::that(url) {
        eprintln!("markview: failed to open browser: {error}");
    }
}

/// The URL `--open` should launch once the server is bound. The root route
/// always resolves to the default document (see `route_for_request`), so the
/// bare root URL is correct regardless of `ServeMode`.
fn serve_open_url(port: u16) -> String {
    format!("http://localhost:{port}/")
}

fn bind_serve_listener(port: u16) -> Result<TcpListener, String> {
    TcpListener::bind(("127.0.0.1", port)).map_err(|error| {
        if error.kind() == io::ErrorKind::AddrInUse {
            format!("port {port} is already in use")
        } else {
            format!("failed to bind localhost:{port}: {error}")
        }
    })
}

/// How long `serve_markdown_daemon` waits for the detached child to actually
/// start accepting connections before giving up and reporting failure.
const DAEMON_READY_TIMEOUT: Duration = Duration::from_secs(5);
/// Initial delay between readiness polls, doubled on each retry (100ms,
/// 200ms, 400ms, ...) until `DAEMON_READY_TIMEOUT` is exhausted.
const DAEMON_READY_INITIAL_DELAY: Duration = Duration::from_millis(100);

/// Starts a background child and reports success only after its serving banner
/// appears and it is still alive. On Unix the bound socket is inherited through
/// stdin, so choosing a concrete port for `--port 0` never releases the port.
/// Other platforms rebind in the child; its banner distinguishes readiness from
/// an unrelated listener. The concrete `--port` remains visible in child argv.
///
/// Output goes to a per-port log. The child stays in the parent's session and
/// process group and is reparented when this launcher exits.
fn serve_markdown_daemon(
    inputs: Vec<PathBuf>,
    port: u16,
    recurse: bool,
    open: bool,
    annotate: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let probe = bind_serve_listener(port)?;
    // Port 0 asks the OS for any free port; the child needs the concrete one.
    let port = probe.local_addr()?.port();
    #[cfg(not(unix))]
    drop(probe);

    let current_exe = std::env::current_exe()?;
    let log_path = daemon_log_path(port);
    let log_file = fs::File::create(&log_path)?;

    let mut command = Command::new(current_exe);
    command.arg("--serve");
    if annotate {
        command.arg("--annotate");
    }
    for input in &inputs {
        command.arg(input);
    }
    command.args(["--port", &port.to_string()]);
    if recurse {
        command.arg("--recurse");
    }
    if open {
        command.arg("--open");
    }
    #[cfg(unix)]
    command
        .env("MARKVIEW_INHERITED_LISTENER", "1")
        .stdin(Stdio::from(OwnedFd::from(probe)));
    #[cfg(not(unix))]
    command.stdin(Stdio::null());
    command
        .stdout(Stdio::from(log_file.try_clone()?))
        .stderr(Stdio::from(log_file));

    let mut child = command.spawn()?;

    if let Err(error) = wait_for_daemon_ready(&mut child, port, &log_path) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!("{error} — see the log for details: {}", log_path.display()).into());
    }

    println!(
        "markview: serving in background (pid {}) at http://localhost:{port} — logs: {}",
        child.id(),
        log_path.display()
    );
    io::stdout().flush()?;

    Ok(())
}

/// Require this child's flushed startup banner and check for early exit before
/// accepting a connection as readiness. A different listener is never enough.
fn wait_for_daemon_ready(
    child: &mut std::process::Child,
    port: u16,
    log_path: &Path,
) -> Result<(), String> {
    let deadline = std::time::Instant::now() + DAEMON_READY_TIMEOUT;
    let mut delay = DAEMON_READY_INITIAL_DELAY;

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return Err(format!(
                    "background server for port {port} exited before it started serving ({status})"
                ));
            }
            Ok(None) => {}
            Err(error) => return Err(format!("failed to check background server: {error}")),
        }

        let banner = format!("Serving on http://localhost:{port} — press Ctrl+C to stop");
        if fs::read_to_string(log_path).is_ok_and(|log| log.lines().any(|line| line == banner))
            && TcpStream::connect(("127.0.0.1", port)).is_ok()
            && child
                .try_wait()
                .map_err(|error| error.to_string())?
                .is_none()
        {
            return Ok(());
        }

        let now = std::time::Instant::now();
        if now >= deadline {
            return Err(format!(
                "background server for port {port} did not become ready within {:.0}s",
                DAEMON_READY_TIMEOUT.as_secs_f64()
            ));
        }
        thread::sleep(delay.min(deadline - now));
        delay *= 2;
    }
}

#[cfg(test)]
mod daemon_tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn unrelated_listener_cannot_make_a_dead_child_ready() {
        let listener = bind_serve_listener(0).unwrap();
        let port = listener.local_addr().unwrap().port();
        let log = tempfile::NamedTempFile::new().unwrap();
        fs::write(
            log.path(),
            format!("Serving on http://localhost:{port} — press Ctrl+C to stop\n"),
        )
        .unwrap();
        let mut child = Command::new("sh").args(["-c", "exit 1"]).spawn().unwrap();
        child.wait().unwrap();
        assert!(wait_for_daemon_ready(&mut child, port, log.path())
            .unwrap_err()
            .contains("exited before"));
    }

    /// Whether `port` can be bound again, allowing a moment for it to free.
    /// While this process spawns a child, the child holds a copy of every
    /// open descriptor until its exec closes the close-on-exec ones. A
    /// listener dropped during that window keeps its port for a fraction of
    /// a millisecond longer (about 0.1 ms when measured), so "free at once"
    /// is not something a multi-threaded test binary can promise.
    #[cfg(unix)]
    fn port_released(port: u16) -> bool {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if bind_serve_listener(port).is_ok() {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[cfg(unix)]
    #[test]
    fn inherited_listener_keeps_port_reserved() {
        // Other tests in this binary spawn processes, and a spawn in progress
        // matters here (see `port_released`). Spawn throughout, so the test
        // meets that case every run instead of once in a while.
        const SPAWNS: u64 = 20;
        let spawned = Arc::new(AtomicU64::new(0));
        let spawner = {
            let spawned = spawned.clone();
            thread::spawn(move || {
                while spawned.load(Ordering::Relaxed) < SPAWNS {
                    Command::new("sh").args(["-c", "exit 0"]).status().unwrap();
                    spawned.fetch_add(1, Ordering::Relaxed);
                }
            })
        };
        while spawned.load(Ordering::Relaxed) < SPAWNS {
            let listener = bind_serve_listener(0).unwrap();
            let port = listener.local_addr().unwrap().port();
            let inherited = Stdio::from(OwnedFd::from(listener));
            assert!(bind_serve_listener(port).is_err());
            drop(inherited);
            assert!(
                port_released(port),
                "port {port} stayed reserved after its last handle was dropped"
            );
        }
        spawner.join().unwrap();
    }
}

fn daemon_log_path(port: u16) -> PathBuf {
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(format!(".markview-serve-{port}.log"))
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
        let layout = nav_layout_for(build.mode, build.documents.len());
        let sidebar_nav = (layout == NavLayout::Sidebar).then(|| NavDir::build(&build.documents));
        let config = Self {
            root: build.root,
            documents: build.documents,
            default_document: build.default_document,
            port,
            session: ServeSession::default(),
            review_cache: annotations::ReviewCache::default(),
            review_summary: serve_review::SummaryCache::default(),
            reserved: build.reserved,
            mode: build.mode,
            sidebar_nav,
            asset_cache: Mutex::new(None),
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

        let root = explicit_serve_root(&canonical_files)?;

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
            reserved: Vec::new(),
        })
    }

    fn discover_directory(
        input: &Path,
        recurse: bool,
    ) -> Result<ServeBuild, Box<dyn std::error::Error>> {
        let root = input.canonicalize()?;
        if is_filesystem_root(&root) {
            return Err("served directory cannot be a filesystem root".into());
        }
        let candidates = collect_markdown_paths(input, recurse)?;

        let mut collision_keys: Vec<String> = Vec::new();
        let mut documents = Vec::new();
        let mut reserved = Vec::new();
        for candidate in candidates {
            let canonical = candidate.canonicalize()?;
            if !canonical.starts_with(&root) {
                let relative = candidate.strip_prefix(input).unwrap_or(&candidate);
                return Err(format!(
                    "served document resolves outside root: {}",
                    relative.display()
                )
                .into());
            }
            let relative = canonical.strip_prefix(&root).unwrap_or(&canonical);
            let route_path = relative_to_route(relative);
            if serve_annotations::reserved(&route_path) {
                let directory = root.join(relative.iter().next().unwrap_or_default());
                if !reserved.contains(&directory) {
                    reserved.push(directory);
                }
                continue;
            }
            if documents
                .iter()
                .any(|document: &ServedDocument| document.source_path == canonical)
            {
                continue;
            }
            let collision_key = route_path.to_lowercase();
            if collision_keys.iter().any(|known| known == &collision_key) {
                return Err(
                    format!("directory contains colliding Markdown routes: {route_path}").into(),
                );
            }
            collision_keys.push(collision_key);
            documents.push(document_for_path(&root, canonical)?);
        }
        if documents.is_empty() {
            let scope = if recurse {
                "recursively"
            } else {
                "at its top level"
            };
            return Err(format!("served directory has no Markdown files {scope}").into());
        }

        let default_document = ["readme.md", "index.md"]
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
                                .is_some_and(|file| file.eq_ignore_ascii_case(name))
                    })
                    .map(|document| document.source_path.clone())
            })
            .unwrap_or_else(|| documents[0].source_path.clone());

        Ok(ServeBuild {
            root,
            documents,
            default_document,
            mode: ServeMode::Directory,
            reserved,
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

    /// The served document a route names, with `/` naming the default page.
    /// A route is a key into the served allowlist: it is compared as sent
    /// and never joined to a path, so another spelling of it names nothing.
    fn document_for_route(&self, route: &str) -> Option<&ServedDocument> {
        if route == "/" {
            self.default_document()
        } else {
            self.document_by_route(route)
        }
    }

    /// Where `document` and its annotation store are, provided it is still
    /// the file that was allowlisted: the same canonical path under the
    /// root, not a symlink that now leads somewhere else. Everything that
    /// reads or writes a review for a served document starts here.
    fn served_location(&self, document: &ServedDocument) -> io::Result<annotations::Location> {
        let path = validated_served_path(&self.root, &document.source_path)
            .filter(|path| path == &document.source_path)
            .ok_or_else(|| io::Error::other("served document is unavailable"))?;
        let location = annotations::resolve(&path)?;
        if location.document != path {
            return Err(io::Error::other("served document changed location"));
        }
        Ok(location)
    }

    fn document_route_for_path(&self, path: &Path) -> Option<&str> {
        self.documents
            .iter()
            .find(|document| document.source_path == path)
            .map(|document| document.route_path.as_str())
    }

    fn asset_by_route(&self, route: &str) -> Option<ServedAsset> {
        self.scan_assets()
            .iter()
            .find(|asset| asset.route_path == route)
            .cloned()
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
        if decoded == "/favicon.ico" {
            return Some("/favicon.ico".to_owned());
        }
        let repeated = decode_repeated(path)?;
        if repeated.chars().any(|ch| ch == '\0' || ch.is_control()) {
            return None;
        }
        if serve_annotations::reserved(&repeated)
            || path_resolves_outside_root(&self.root, &repeated)
        {
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
        route_from_root(&self.root, &candidate).filter(|route| !serve_annotations::reserved(route))
    }

    /// Document mtimes, in `self.documents` order — the signature the asset
    /// cache is keyed on. `self.documents` itself is fixed for a `ServeConfig`
    /// generation (see `asset_cache`'s doc comment), so identity/order never
    /// changes between calls and only the mtimes themselves carry
    /// invalidation information; a document that fails to `stat` (e.g. it was
    /// just deleted out from under us) maps to `UNIX_EPOCH`, which is fine as
    /// a cache key: it's still a stable, comparable value, so the cache stays
    /// coherent even while the file is momentarily unreadable.
    fn document_mtimes(&self) -> Vec<SystemTime> {
        self.documents
            .iter()
            .map(|document| file_mtime(&document.source_path).unwrap_or(SystemTime::UNIX_EPOCH))
            .collect()
    }

    fn scan_assets(&self) -> Arc<[ServedAsset]> {
        let signature = self.document_mtimes();
        // A poisoned lock (some other thread panicked while holding it)
        // degrades to a forced cache miss rather than taking this thread
        // down too, matching how `clients` handling elsewhere tolerates
        // poisoning instead of propagating it.
        if let Ok(cache) = self.asset_cache.lock() {
            if let Some(cached) = cache.as_ref() {
                if cached.signature == signature {
                    // Cheap: bumps a refcount instead of deep-cloning every
                    // asset just so the caller can `.find()` one of them.
                    return cached.assets.clone();
                }
            }
        }
        // The disk scan itself runs with the lock released, so it doesn't
        // serialize every other thread's `scan_assets()` call (one thread
        // per connection) behind a single miss's I/O.
        let assets: Arc<[ServedAsset]> = self.scan_assets_uncached().into();
        if let Ok(mut cache) = self.asset_cache.lock() {
            *cache = Some(AssetCache {
                signature,
                assets: assets.clone(),
            });
        }
        assets
    }

    fn scan_assets_uncached(&self) -> Vec<ServedAsset> {
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
                if !canonical.starts_with(&self.root)
                    || !canonical.is_file()
                    || canonical.strip_prefix(&self.root).is_ok_and(|relative| {
                        relative
                            .components()
                            .any(|part| part.as_os_str() == ".markview")
                    })
                {
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
    let route_path = route_from_root(root, &canonical)
        .filter(|route| !serve_annotations::reserved(route))
        .ok_or_else(|| {
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

// Inputs have already been canonicalized and checked for duplicates.
fn explicit_serve_root(canonical_files: &[PathBuf]) -> Result<PathBuf, &'static str> {
    let root = if canonical_files.len() == 1 {
        canonical_files[0]
            .parent()
            .ok_or("served file has no parent directory")?
            .to_path_buf()
    } else {
        deepest_common_ancestor(canonical_files)
            .ok_or("explicit served files have no common ancestor")?
    };
    if is_filesystem_root(&root) {
        return Err("explicit served files must share a non-root ancestor directory");
    }
    Ok(root)
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

fn markdown_references(source: &str) -> Vec<String> {
    Parser::new_ext(source, markview::scanner_options())
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
    for event in Parser::new_ext(source, markview::scanner_options()) {
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
            // A `/events` write timing out (see `set_write_timeout`) means a
            // peer went silently unreachable rather than disconnecting
            // cleanly — routine in the same way, so it's quieted the same way.
            | io::ErrorKind::TimedOut
            | io::ErrorKind::WouldBlock
    )
}

/// Watches the served root for changes. A plain edit to an already-known
/// document just pushes a reload signal (cheap, matches today's behavior). A
/// Markdown file being created or removed anywhere in scope — or any event on
/// a Markdown path that isn't currently a known document, e.g. an atomic
/// editor save that shows up as remove+create — triggers a full rescan
/// (re-running the same discovery `serve_markdown` used at startup) before
/// pushing the reload, so the served document set stays in sync with disk
/// instead of only ever reflecting what existed when the server started. Any
/// other (non-Markdown) change under the root — an asset being added,
/// edited, or removed — invalidates the active config's `asset_cache` so the
/// next request re-scans instead of serving a stale (e.g. missing) result.
fn watch_root(
    shared: SharedConfig,
    inputs: Vec<PathBuf>,
    port: u16,
    recurse: bool,
    clients: Clients,
    stores: Arc<serve_annotations::Stores>,
) -> notify::Result<RecommendedWatcher> {
    let (root, mode) = {
        let config = shared.read().expect("config lock");
        (config.root.clone(), config.mode)
    };
    let watch_mode = match mode {
        ServeMode::Directory if recurse => RecursiveMode::Recursive,
        ServeMode::Directory | ServeMode::SingleFile => RecursiveMode::NonRecursive,
        // Explicit files can sit at any depth under their shared common root,
        // so their parent directories aren't known/watchable individually.
        ServeMode::Explicit => RecursiveMode::Recursive,
    };
    let (changes_tx, changes_rx) = mpsc::channel();
    let mut watcher = RecommendedWatcher::new(
        move |result: notify::Result<notify::Event>| {
            if let Ok(event) = result {
                let _ = changes_tx.send(event);
            }
        },
        Config::default(),
    )?;
    watcher.watch(&root, watch_mode)?;

    thread::spawn(move || {
        while let Ok(event) = changes_rx.recv() {
            let mut events = vec![event];
            while let Ok(event) = changes_rx.recv_timeout(WATCH_DEBOUNCE) {
                events.push(event);
            }
            handle_fs_events(events, &shared, &clients, &stores, &inputs, port, recurse);
        }
    });

    Ok(watcher)
}

fn handle_fs_events(
    events: Vec<notify::Event>,
    shared: &SharedConfig,
    clients: &Clients,
    stores: &Arc<serve_annotations::Stores>,
    inputs: &[PathBuf],
    port: u16,
    recurse: bool,
) {
    let current = shared.read().expect("config lock").clone();
    let mut relevant = false;
    let mut needs_rescan = false;
    // Any non-Markdown change under the watched root — an asset being
    // created, edited, or removed — can't add/remove a served *document*, but
    // it can change what `scan_assets()` should return (e.g. an image a
    // document already references shows up on disk). `scan_assets()`'s cache
    // is only keyed on document mtimes, so it has no way to notice this on
    // its own; the cache must be invalidated here instead.
    let mut asset_changed = false;
    for event in events {
        if !is_reload_event(&event.kind) {
            continue;
        }
        let is_structural = matches!(
            event.kind,
            EventKind::Create(_) | EventKind::Remove(_) | EventKind::Modify(ModifyKind::Name(_))
        );
        for event_path in &event.paths {
            if event_path
                .components()
                .any(|part| part.as_os_str() == ".markview")
            {
                continue;
            }
            // Recursive backends may report only the new directory when files
            // are created before its watch is installed. Rescan the tree on
            // structural directory events, including removal/rename of known
            // document parents, rather than requiring a Markdown file event.
            if current.mode == ServeMode::Directory
                && recurse
                && is_structural
                && (event_path.is_dir()
                    || matches!(
                        event.kind,
                        EventKind::Create(notify::event::CreateKind::Folder)
                            | EventKind::Remove(notify::event::RemoveKind::Folder)
                    )
                    || current
                        .documents
                        .iter()
                        .any(|doc| doc.source_path.starts_with(event_path)))
            {
                needs_rescan = true;
            }
            if !is_markdown_path(event_path) {
                relevant = true;
                asset_changed = true;
                continue;
            }
            relevant = true;
            let lookup_path = event_path
                .canonicalize()
                .unwrap_or_else(|_| event_path.clone());
            let known = current
                .documents
                .iter()
                .any(|document| document.source_path == lookup_path);
            if !known || is_structural {
                needs_rescan = true;
            }
        }
    }
    if !relevant {
        return;
    }
    if needs_rescan {
        // A fresh `ServeConfig` carries its own empty `asset_cache`, so
        // there's nothing to invalidate here even if `asset_changed` too.
        let rescan_id = NEXT_RESCAN_ID.fetch_add(1, Ordering::Relaxed);
        let shared = shared.clone();
        let clients = clients.clone();
        let stores = stores.clone();
        let inputs = inputs.to_vec();
        thread::spawn(
            move || match ServeConfig::from_inputs(inputs, port, recurse) {
                Ok(fresh) => publish_rescan(&shared, &clients, &stores, fresh, rescan_id),
                Err(error) => eprintln!("markview: failed to rescan served directory: {error}"),
            },
        );
    } else {
        if asset_changed {
            if let Ok(mut cache) = current.asset_cache.lock() {
                *cache = None;
            }
        }
        broadcast_reload(clients, ReloadKind::Content);
    }
}

fn publish_rescan(
    shared: &SharedConfig,
    clients: &Clients,
    stores: &serve_annotations::Stores,
    mut fresh: ServeConfig,
    rescan_id: u64,
) {
    let reload = {
        // Held for the swap alone: every request waits on this lock.
        let mut published = shared.write().expect("config lock");
        // An earlier scan may finish after a newer one on the background workers.
        if rescan_id <= published.session.rescan_id {
            return;
        }
        fresh.session = ServeSession {
            rescan_id,
            ..published.session.clone()
        };
        let reload = if nav_layout(&published) == nav_layout(&fresh) {
            ReloadKind::Content
        } else {
            ReloadKind::Structure
        };
        *published = Arc::new(fresh);
        reload
    };
    stores.sync(shared);
    broadcast_reload(clients, reload);
}

fn broadcast_reload(clients: &Clients, reload_kind: ReloadKind) {
    retain_clients(clients, |(_, client)| {
        client.send(reload_kind.clone()).is_ok()
    });
}

fn is_reload_event(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
    )
}

fn handle_connection(
    mut stream: TcpStream,
    shared: &SharedConfig,
    clients: Clients,
    stores: &serve_annotations::Stores,
    session: Option<Arc<serve_review::Session>>,
) -> io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    // Pages, assets and `/events` need only the method and target, so the
    // request line is read leniently and their headers are never parsed. The
    // strict reader is for the reserved prefix alone, which accepts writes.
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let request_path = parts.next().unwrap_or("/");
    if let Some((content_type, body)) = serve_annotations::static_asset(request_path) {
        // Static and public, so it is read as leniently as any page asset.
        let result = if matches!(method, "GET" | "HEAD") {
            write_response_headers(
                &mut stream,
                "200 OK",
                content_type,
                body.as_bytes(),
                method == "HEAD",
                serve_annotations::STATIC_CACHE,
            )
        } else {
            write_response(
                &mut stream,
                "405 Method Not Allowed",
                "text/plain; charset=utf-8",
                "Method not allowed",
                false,
            )
        };
        drain_request(&stream, &mut reader);
        return result;
    }
    if serve_annotations::reserved(request_path) {
        let result =
            match serve_annotations::HttpRequest::parse(&request_line, &mut reader, deadline) {
                Ok(request) => serve_annotations::handle(
                    &mut stream,
                    &mut reader,
                    shared,
                    &request,
                    deadline,
                    session.as_deref(),
                    &clients,
                ),
                Err(_) => serve_annotations::malformed(&mut stream),
            };
        // A refused POST is answered before its body is read. Without the
        // drain, a body larger than the socket buffer resets the connection
        // and the page reports "Failed to fetch" instead of the error.
        drain_request(&stream, &mut reader);
        return result;
    }
    let config = shared.read().expect("config lock").clone();
    let config = &*config;
    let head_only = method == "HEAD";
    let route = config.route_for_request(request_path);
    let route = route.as_deref().unwrap_or("");
    if method == "GET" && route == "/events" {
        return match serve_events(stream, clients, session.as_deref()) {
            Ok(()) => Ok(()),
            Err(error) if is_client_disconnect(&error) => {
                if std::env::var_os("MARKVIEW_LOG_EVENT_DISCONNECTS").is_some() {
                    eprintln!("markview: /events client disconnected");
                }
                Ok(())
            }
            Err(error) => Err(error),
        };
    }
    // The write token goes only into pages requested by a loopback name.
    // Reached through any other Host the page is read-only, so a page that
    // rebinds its own name to this server finds no token to harvest.
    let token = |reader: &mut BufReader<TcpStream>| {
        config.session.review_token.as_deref().filter(|_| {
            serve_annotations::lenient_host(reader, deadline)
                .is_some_and(|host| serve_annotations::loopback_host(&host, config.port))
        })
    };
    let result = if !matches!(method, "GET" | "HEAD") {
        write_response(
            &mut stream,
            "405 Method Not Allowed",
            "text/plain; charset=utf-8",
            "Method not allowed",
            false,
        )
    } else {
        match route {
            "/" => match config.default_document() {
                Some(document) => {
                    let token = token(&mut reader);
                    serve_document(
                        &mut stream,
                        config,
                        stores,
                        document,
                        head_only,
                        token,
                        session.is_some(),
                    )
                }
                None => write_404(&mut stream, head_only),
            },
            "/events" => {
                write_bytes_response(&mut stream, "200 OK", "text/event-stream", &[], true)
            }
            "/favicon.ico" => serve_favicon(&mut stream, config, head_only),
            _ => {
                if let Some(document) = config.document_by_route(route) {
                    let token = token(&mut reader);
                    serve_document(
                        &mut stream,
                        config,
                        stores,
                        document,
                        head_only,
                        token,
                        session.is_some(),
                    )
                } else if let Some(asset) = config.asset_by_route(route) {
                    serve_asset(&mut stream, config, &asset, head_only)
                } else {
                    write_404(&mut stream, head_only)
                }
            }
        }
    };
    drain_request(&stream, &mut reader);
    result
}

/// How much unread request the server discards after answering, and for how
/// long, before it closes the connection.
const DRAIN_CAP: usize = 256 * 1024;
const DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

/// Lets the client read the whole response. Closing a socket that still holds
/// unread request bytes (headers that were never parsed, or a body that was
/// refused) resets the connection, and the client can lose the response. So
/// finish the response, then discard what the client sent until it closes.
fn drain_request(stream: &TcpStream, reader: &mut BufReader<TcpStream>) {
    let _ = stream.shutdown(std::net::Shutdown::Write);
    let deadline = std::time::Instant::now() + DRAIN_TIMEOUT;
    let mut remaining = DRAIN_CAP;
    while remaining > 0 {
        let timeout = deadline
            .checked_duration_since(std::time::Instant::now())
            .filter(|timeout| !timeout.is_zero());
        if timeout.is_none() || stream.set_read_timeout(timeout).is_err() {
            break;
        }
        match reader.fill_buf() {
            Ok(unread) if !unread.is_empty() => {
                let count = unread.len().min(remaining);
                reader.consume(count);
                remaining -= count;
            }
            _ => break,
        }
    }
}

/// The favicon a browser requests automatically. Browsers fetch
/// `/favicon.ico` unprompted, with no Markdown reference required, so
/// `route_for_request` treats it as a virtual route (like `/events`) and
/// dispatch here checks the filesystem directly (the same way `serve_asset`
/// validates any other on-disk file) rather than the reference-scanned
/// asset allowlist that `document_by_route`/`asset_by_route` are built
/// from. A real `favicon.ico` under the served root always wins; this app
/// icon is only the fallback when there is none.
static FAVICON: &[u8] = include_bytes!("../assets/favicon.png");

fn serve_favicon(stream: &mut TcpStream, config: &ServeConfig, head_only: bool) -> io::Result<()> {
    let candidate = config.root.join("favicon.ico");
    if let Some(canonical) = validated_served_path(&config.root, &candidate) {
        if let Ok(bytes) = fs::read(&canonical) {
            return write_response_headers(
                stream,
                "200 OK",
                safe_content_type("favicon.ico"),
                &bytes,
                head_only,
                asset_policy(config),
            );
        }
    }
    write_bytes_response(stream, "200 OK", "image/png", FAVICON, head_only)
}

fn serve_document(
    stream: &mut TcpStream,
    config: &ServeConfig,
    stores: &serve_annotations::Stores,
    document: &ServedDocument,
    head_only: bool,
    token: Option<&str>,
    review: bool,
) -> io::Result<()> {
    let nonce = config
        .session
        .review_token
        .as_ref()
        .map(|_| annotations::random_hex())
        .transpose()?;
    // A page needs the review script when comments can be added to it
    // (`--annotate`) or it already has some. Any other page carries no
    // review machinery: its extras are the copy controls, alert styles and
    // live-reload shell, pinned by tests/fixtures/serve-baseline.expected.html.
    let review = (config.session.review_token.is_some()
        || serve_annotations::has_store(&document.source_path))
    .then_some(ReviewHooks {
        nonce: nonce.as_deref(),
        token,
        blocking_review: review && token.is_some(),
    });
    stores.served(&document.source_path, review.is_some());
    match render_file(config, document, review) {
        Ok(html) => {
            let policy = nonce.map(|nonce| {
                format!(
                    "Content-Security-Policy: {}\r\n",
                    serve_annotations::content_security_policy(&nonce)
                )
            });
            write_response_headers(
                &mut *stream,
                "200 OK",
                "text/html; charset=utf-8",
                html.as_bytes(),
                head_only,
                policy.as_deref().unwrap_or(""),
            )
        }
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

/// With `--annotate` the origin holds a write token, so a served file opened
/// as a document must not be able to act as that origin. `sandbox` gives it
/// an opaque origin and no script; images and downloads are unaffected.
fn asset_policy(config: &ServeConfig) -> &'static str {
    if config.session.review_token.is_some() {
        "Content-Security-Policy: sandbox\r\n"
    } else {
        ""
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
        Ok(bytes) => write_response_headers(
            stream,
            "200 OK",
            asset.content_type,
            &bytes,
            head_only,
            asset_policy(config),
        ),
        Err(_) => write_404(stream, head_only),
    }
}

fn serve_events(
    mut stream: TcpStream,
    clients: Clients,
    session: Option<&serve_review::Session>,
) -> io::Result<()> {
    let (tx, rx) = mpsc::channel();
    let id = NEXT_CLIENT_ID.fetch_add(1, Ordering::Relaxed);
    clients
        .lock()
        .map_err(|_| io::Error::other("clients lock poisoned"))?
        .push((id, tx));
    // Ensures this entry is removed from `clients` as soon as this function
    // returns by any path, instead of leaving it for the next real
    // `broadcast_reload` (a file change) to notice via a failed send.
    let _guard = ClientGuard {
        clients: clients.clone(),
        id,
    };
    // Without this, a peer that vanishes without a clean FIN/RST (network
    // death, laptop sleep) may never make a write fail at all, and this
    // thread — along with its `clients` entry — would leak forever instead
    // of being noticed and cleaned up like an ordinary disconnect.
    stream.set_write_timeout(Some(EVENTS_KEEPALIVE))?;
    stream.write_all(
        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nX-Content-Type-Options: nosniff\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\n\r\n",
    )?;
    stream.flush()?;
    if session.is_some_and(serve_review::Session::finished) {
        return stream.write_all(b"event: review-finished\ndata: finished\n\n");
    }

    // Poll on a timeout rather than blocking on `rx.recv()` forever: this is a
    // long-lived connection with no read side, so a periodic keepalive write is
    // the only way to notice the browser navigated away and closed its end. A
    // bare `recv()` would leave this thread parked forever, since a broken pipe
    // is only ever discovered on the next *write*.
    loop {
        match rx.recv_timeout(EVENTS_KEEPALIVE) {
            Ok(ReloadKind::Annotations(route)) => {
                write!(stream, "event: annotations\ndata: {route}\n\n")?
            }
            Ok(ReloadKind::ReviewFinished) => {
                return stream.write_all(b"event: review-finished\ndata: finished\n\n");
            }
            Ok(ReloadKind::Content) => stream.write_all(b"data: reload\n\n")?,
            Ok(ReloadKind::Structure) => stream.write_all(b"data: rescan\n\n")?,
            Err(mpsc::RecvTimeoutError::Timeout) => stream.write_all(b": keepalive\n\n")?,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        stream.flush()?;
    }

    Ok(())
}

/// Present when a page carries the review script: the policy nonce of a
/// token-bearing page, and the write token if the client may have it.
#[derive(Clone, Copy)]
struct ReviewHooks<'a> {
    nonce: Option<&'a str>,
    token: Option<&'a str>,
    blocking_review: bool,
}

fn render_file(
    config: &ServeConfig,
    served: &ServedDocument,
    review: Option<ReviewHooks<'_>>,
) -> io::Result<String> {
    if validated_served_path(&config.root, &served.source_path).is_none() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "served document no longer resolves under serve root",
        ));
    }
    let markdown = fs::read_to_string(&served.source_path)?;
    let title = title_for_source(&served.source_path, &markdown);
    // Scanned once per render rather than once per link/image reference —
    // `scan_assets()` is otherwise a full document-mtime signature check
    // (and, on a miss, a directory walk) for every single reference.
    let assets = config.scan_assets();
    let revision = annotations::source_revision(&markdown);
    let document = MarkdownDocument::with_title(markdown, title);
    let modified_at = modified_timestamp_millis(&served.source_path);
    Ok(inject_serve_shell(
        config,
        &served.route_path,
        modified_at,
        &HtmlRenderer.render_with_destinations(&document, |kind, destination| {
            if serve_annotations::blocked_destination(kind, destination) {
                Some("#".to_owned())
            } else {
                rewritten_reference(config, served, destination, &assets)
            }
        }),
        review.map(|hooks| (revision.as_str(), hooks)),
    ))
}

fn rewritten_reference(
    config: &ServeConfig,
    served: &ServedDocument,
    reference: &str,
    assets: &[ServedAsset],
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
    assets
        .iter()
        .find(|asset| asset.source_path == canonical)
        .map(|asset| format!("{}{}", asset.route_path, fragment))
}

/// "dir / dir / **Title**" for the top bar.
fn render_breadcrumb(config: &ServeConfig, active_route: &str) -> String {
    let segments = route_path_segments(active_route);
    let Some((last, dirs)) = segments.split_last() else {
        return String::new();
    };
    let title = config
        .documents
        .iter()
        .find(|document| document.route_path == active_route)
        .map(current_document_title)
        .unwrap_or_else(|| percent_decode(last).unwrap_or_else(|| (*last).to_owned()));
    let mut html = String::new();
    for dir in dirs {
        html.push_str(&html_escape(
            &percent_decode(dir).unwrap_or_else(|| (*dir).to_owned()),
        ));
        html.push_str(" / ");
    }
    html.push_str(&format!("<b>{}</b>", html_escape(&title)));
    html
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
    let links = if layout == NavLayout::Sidebar {
        let open_ancestors = active_ancestor_prefixes(active_route);
        config
            .sidebar_nav
            .as_ref()
            .map(|tree| render_nav_dir(tree, &config.documents, active_route, &open_ancestors))
            .unwrap_or_default()
    } else {
        render_nav_flat(&config.documents, active_route)
    };
    format!(r#"<nav data-markview-nav class="{class}">{links}</nav>"#)
}

fn render_nav_flat(documents: &[ServedDocument], active_route: &str) -> String {
    documents
        .iter()
        .map(|document| render_nav_link(document, active_route))
        .collect::<Vec<_>>()
        .join("")
}

fn render_nav_link(document: &ServedDocument, active_route: &str) -> String {
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
}

/// A directory grouping for the sidebar's nav tree. `prefix` is the route
/// path to this directory (e.g. `/sub/deep`), used both as a stable open/
/// closed key (see the reload script in `inject_serve_shell`) and to decide
/// which directories sit on the path to the active document.
#[derive(Debug, Clone)]
struct NavDir {
    name: String,
    prefix: String,
    dirs: Vec<NavDir>,
    child_indexes: HashMap<String, usize>,
    docs: Vec<usize>,
}

impl NavDir {
    fn new(name: String, prefix: String) -> Self {
        Self {
            name,
            prefix,
            dirs: Vec::new(),
            child_indexes: HashMap::new(),
            docs: Vec::new(),
        }
    }

    fn build(documents: &[ServedDocument]) -> Self {
        let mut root = Self::new(String::new(), String::new());
        for (index, document) in documents.iter().enumerate() {
            let segments = route_path_segments(&document.route_path);
            root.insert(&segments, index);
        }
        root.sort(documents);
        root
    }

    fn insert(&mut self, segments: &[&str], document_index: usize) {
        match segments {
            [] | [_] => self.docs.push(document_index),
            [first, rest @ ..] => {
                let index = self.child_indexes.get(*first).copied().unwrap_or_else(|| {
                    let child_prefix = format!("{}/{first}", self.prefix);
                    let child_name = percent_decode(first).unwrap_or_else(|| (*first).to_owned());
                    let index = self.dirs.len();
                    self.dirs.push(Self::new(child_name, child_prefix));
                    self.child_indexes.insert((*first).to_owned(), index);
                    index
                });
                self.dirs[index].insert(rest, document_index);
            }
        }
    }

    fn sort(&mut self, documents: &[ServedDocument]) {
        self.dirs.sort_by(|a, b| a.name.cmp(&b.name));
        self.docs
            .sort_by(|a, b| documents[*a].route_path.cmp(&documents[*b].route_path));
        for dir in &mut self.dirs {
            dir.sort(documents);
        }
    }
}

/// Route prefixes (e.g. `["/sub", "/sub/deep"]`) of every directory on the
/// path to `active_route`, so the sidebar auto-expands the active document's
/// folder(s) by default while leaving unrelated folders collapsed.
fn active_ancestor_prefixes(active_route: &str) -> Vec<String> {
    let segments = route_path_segments(active_route);
    let mut prefixes = Vec::new();
    let mut prefix = String::new();
    for segment in segments.iter().take(segments.len().saturating_sub(1)) {
        prefix.push('/');
        prefix.push_str(segment);
        prefixes.push(prefix.clone());
    }
    prefixes
}

fn route_path_segments(route_path: &str) -> Vec<&str> {
    route_path
        .trim_start_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect()
}

fn render_nav_dir(
    dir: &NavDir,
    documents: &[ServedDocument],
    active_route: &str,
    open_ancestors: &[String],
) -> String {
    let mut html = String::new();
    for child in &dir.dirs {
        let open = if open_ancestors.iter().any(|prefix| prefix == &child.prefix) {
            " open"
        } else {
            ""
        };
        html.push_str(&format!(
            r#"<details class="markview-nav-dir" data-markview-dir="{}"{}><summary>{}</summary>"#,
            html_escape(&child.prefix),
            open,
            html_escape(&child.name)
        ));
        html.push_str(&render_nav_dir(
            child,
            documents,
            active_route,
            open_ancestors,
        ));
        html.push_str("</details>");
    }
    for document_index in &dir.docs {
        html.push_str(&render_nav_link(&documents[*document_index], active_route));
    }
    html
}

fn nav_layout(config: &ServeConfig) -> NavLayout {
    nav_layout_for(config.mode, config.documents.len())
}

fn nav_layout_for(mode: ServeMode, document_count: usize) -> NavLayout {
    if document_count <= 1 {
        NavLayout::Empty
    } else if mode != ServeMode::Directory && document_count <= 6 {
        NavLayout::Tabs
    } else {
        NavLayout::Sidebar
    }
}

/// Wraps a rendered document in what every served page has: the navigation,
/// the footer, the copy controls and the live-reload script. `review` adds
/// the review hooks; without it the page carries no review machinery.
fn inject_serve_shell(
    config: &ServeConfig,
    active_route: &str,
    modified_at: Option<u128>,
    html: &str,
    review: Option<(&str, ReviewHooks<'_>)>,
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
    let script = concat!("<script>\n", include_str!("serve_reload.js"), "</script>");

    let style = format!(
        "<style>\n:root[data-theme=\"light\"] {{\n  color-scheme: light;\n{THEME_LIGHT_VARIABLES}\n{DIFF_LIGHT_VARIABLES}\n{ALERT_LIGHT_VARIABLES}\n}}\n:root[data-theme=\"dark\"] {{\n  color-scheme: dark;\n{THEME_DARK_VARIABLES}\n{DIFF_DARK_VARIABLES}\n{ALERT_DARK_VARIABLES}\n}}\n{}</style>",
        include_str!("serve_shell.css")
    );
    let html = html.replace("</style>", &format!("</style>\n{style}"));
    let nonce_attr = review
        .and_then(|(_, hooks)| hooks.nonce)
        .map_or(String::new(), |nonce| format!(r#" nonce="{nonce}""#));
    // Applies a saved Light or Dark choice before first paint; Auto leaves the
    // attribute off so the system preference decides.
    let theme_script = format!(
        "<script{nonce_attr}>try{{var t=localStorage.getItem('markview-theme');if(t==='light'||t==='dark')document.documentElement.dataset.theme=t}}catch(e){{}}</script>"
    );
    let html = html.replacen("</head>", &format!("{theme_script}\n</head>"), 1);
    let blocking = review.is_some_and(|(_, hooks)| hooks.blocking_review);
    let verdict_slot = if blocking {
        r#"<div data-markview-verdict></div>"#
    } else {
        ""
    };
    let top = format!(
        r#"<header class="markview-top">
<a class="markview-brand" href="/">markview</a>
<span class="markview-crumb">{}</span>
<span class="markview-top-spacer"></span>
<div class="markview-theme" role="group" aria-label="Theme">
<button type="button" data-markview-theme="light" aria-pressed="false">Light</button>
<button type="button" data-markview-theme="auto" aria-pressed="true">Auto</button>
<button type="button" data-markview-theme="dark" aria-pressed="false">Dark</button>
</div>
{verdict_slot}
</header>
"#,
        render_breadcrumb(config, active_route)
    );
    let nav_kind = match nav_layout(config) {
        NavLayout::Sidebar => "sidebar",
        NavLayout::Tabs => "tabs",
        NavLayout::Empty => "none",
    };
    let aside = if review.is_some() {
        r#"<aside class="markview-aside" aria-label="Review">
<div class="markview-aside-tabs" role="tablist">
<button type="button" role="tab" data-markview-tab="comments" aria-selected="true">Comments</button>
<button type="button" role="tab" data-markview-tab="outline" aria-selected="false">Outline</button>
</div>
<div class="markview-aside-pane" data-markview-comments></div>
<div class="markview-aside-pane markview-outline" data-markview-outline hidden></div>
</aside>"#
    } else {
        ""
    };
    let aside_attr = if review.is_some() { " data-aside" } else { "" };
    let html = html
        .replacen(
            "<main>",
            &format!(
                r#"{top}<div class="markview-shell" data-nav="{nav_kind}"{aside_attr}>{nav}<main>"#
            ),
            1,
        )
        .replacen("</main>", &format!("</main>\n{footer}\n{aside}</div>"), 1);
    let (html, hooks) = if let Some((revision, hooks)) = review {
        let html = html.replacen(
            "<main>",
            &format!(
                r#"<main data-source-revision="{revision}" data-markview-route="{}">"#,
                html_escape(active_route)
            ),
            1,
        );
        (
            html,
            serve_annotations::hooks(
                active_route,
                hooks.token,
                config.port,
                &nonce_attr,
                hooks.blocking_review,
            ),
        )
    } else {
        (html, String::new())
    };
    let copy = serve_annotations::copy_hooks(&nonce_attr);
    let script = script.replacen("<script>", &format!("<script{nonce_attr}>"), 1);
    html.replace("</body>", &format!("{copy}\n{hooks}\n{script}\n</body>"))
}

fn file_mtime(path: &Path) -> Option<SystemTime> {
    path.metadata().ok()?.modified().ok()
}

fn modified_timestamp_millis(path: &Path) -> Option<u128> {
    file_mtime(path)?
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
    write_response_headers(stream, status, content_type, body, head_only, "")
}

fn write_response_headers(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &[u8],
    head_only: bool,
    headers: &str,
) -> io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nX-Content-Type-Options: nosniff\r\nContent-Length: {}\r\n{headers}Connection: close\r\n\r\n",
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

#[cfg(test)]
mod reference_scan_tests {
    use super::*;
    use std::time::Instant;

    fn rendered_references(source: &str) -> Vec<String> {
        let document = markview::RenderedDocument::from_markdown(&MarkdownDocument::new(source));
        let mut references = Vec::new();
        for (tag, attribute) in [("<a ", "href=\""), ("<img ", "src=\"")] {
            for element in document.html().split(tag).skip(1) {
                let opening = element.split('>').next().unwrap();
                if let Some(value) = opening.split(attribute).nth(1) {
                    let value = value.split('"').next().unwrap().replace("&amp;", "&");
                    // Generated footnote backlinks are not source asset references.
                    if !value.starts_with('#') {
                        references.push(value);
                    }
                }
            }
        }
        references.sort();
        references
    }

    #[test]
    fn reference_scanner_agrees_with_rendered_links_and_images() {
        for source in [
            "[[1]](cite.txt)",
            "[[RFC 2119]](rfc.txt)",
            "![[x]](pic2.png)",
            "[see [[x]]](d.txt)",
            "[[RFC2119]]\n\n[RFC2119]: rfc2119.txt\n",
            "[[x.txt]] ![[x.png]] [[.hidden/secret.txt]]",
            "| Text |\n| --- |\n| [[x.txt]] |\n",
            "- [[x.txt]]",
            "> ![[x.png]]",
            "[E=mc^2^](caret.txt) ![~x~](sub.png)",
            "# Title ^sup^ and [[Other Page]]\n\n[normal](normal.txt)",
            "`[code](hidden.txt)`\n\n```text\n![code](hidden.png)\n```",
            "<span>[label](visible.txt)</span>",
            "[query](paper.txt?a=1&b=2)",
        ] {
            let mut scanned = markdown_references(source);
            scanned.sort();
            assert_eq!(scanned, rendered_references(source), "{source}");
        }
    }

    #[test]
    fn reference_scanner_preserves_known_pre_013_divergences() {
        // These scanner/renderer differences predate 0.13. Do not hide new
        // divergences in this list: each exception pins both existing outputs.
        for (name, source, scanned, rendered) in [
            (
                "YAML front matter",
                "---\nasset: [x](yaml.txt)\n---\n",
                vec![],
                vec!["yaml.txt"],
            ),
            ("math", "$[x](math.txt)$", vec![], vec!["math.txt"]),
            (
                "old footnotes",
                "[^3]:\n\n    [x](footnote.txt)\n",
                vec![],
                vec!["footnote.txt"],
            ),
            (
                "definition lists",
                "Term\n: definition\n\n    [x](definition.txt)\n",
                vec!["definition.txt"],
                vec![],
            ),
        ] {
            assert_eq!(markdown_references(source), scanned, "scanner: {name}");
            assert_eq!(rendered_references(source), rendered, "renderer: {name}");
        }
    }

    #[test]
    fn reference_scan_is_fast_for_nested_wikilink_style_prose() {
        let source = format!("{}{}", "![[".repeat(40_000), "]]".repeat(40_000));
        let start = Instant::now();
        let references = markdown_references(&source);
        let elapsed = start.elapsed();
        eprintln!("reference scan: {elapsed:?}");
        assert!(
            elapsed < Duration::from_millis(1500),
            "reference scan took {elapsed:?}"
        );
        assert!(references.is_empty());
    }
}

#[cfg(test)]
mod title_scan_tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn title_scan_is_fast_for_nested_wikilink_style_prose() {
        let source = format!("{}{}", "![[".repeat(40_000), "]]".repeat(40_000));
        let start = Instant::now();
        let title = first_heading_title(&source);
        let elapsed = start.elapsed();
        eprintln!("title scan: {elapsed:?}");
        assert!(
            elapsed < Duration::from_millis(1500),
            "title scan took {elapsed:?}"
        );
        assert_eq!(title, None);
    }
}

#[cfg(test)]
mod serve_nav_tests {
    use super::*;
    use std::time::Duration;

    #[cfg(unix)]
    #[test]
    fn serve_mode_rejects_explicit_files_with_only_filesystem_root_common() {
        // These are canonical-path inputs to the same validation used after
        // filesystem checks; no runner-specific writable directories needed.
        assert_eq!(
            explicit_serve_root(&[PathBuf::from("/one/a.md"), PathBuf::from("/two/b.md")]),
            Err("explicit served files must share a non-root ancestor directory")
        );
        assert_eq!(
            explicit_serve_root(&[
                PathBuf::from("/docs/one/a.md"),
                PathBuf::from("/docs/two/b.md")
            ]),
            Ok(PathBuf::from("/docs"))
        );
    }

    #[test]
    fn directory_serve_config_precomputes_sidebar_nav_tree() {
        let dir = tempfile::tempdir().expect("temp dir");
        let guides = dir.path().join("guides");
        std::fs::create_dir(&guides).expect("guides dir");
        std::fs::write(dir.path().join("README.md"), "# Home\n").expect("write readme");
        std::fs::write(guides.join("setup.md"), "# Setup\n").expect("write setup");

        let config = ServeConfig::from_inputs(vec![dir.path().to_path_buf()], 0, true)
            .expect("serve config");

        assert!(config.sidebar_nav.is_some());
    }

    #[test]
    fn serve_listener_binds_to_loopback_only() {
        let listener = bind_serve_listener(0).unwrap();
        assert!(listener.local_addr().unwrap().ip().is_loopback());
    }

    #[test]
    fn serve_open_url_points_at_localhost_root() {
        assert_eq!(serve_open_url(7878), "http://localhost:7878/");
        assert_eq!(serve_open_url(3000), "http://localhost:3000/");
    }

    #[test]
    fn route_path_segments_trim_leading_and_empty_segments() {
        assert_eq!(
            route_path_segments("/sub/deep/readme.md"),
            vec!["sub", "deep", "readme.md"]
        );
        assert_eq!(route_path_segments("/"), Vec::<&str>::new());
        assert_eq!(
            route_path_segments("//sub///readme.md"),
            vec!["sub", "readme.md"]
        );
    }

    #[test]
    fn nav_dir_tracks_child_dirs_by_segment_name() {
        let documents = vec![
            served_doc("/zeta/one.md"),
            served_doc("/alpha/two.md"),
            served_doc("/zeta/three.md"),
        ];

        let tree = NavDir::build(&documents);

        assert!(tree.child_indexes.contains_key("zeta"));
        assert!(tree.child_indexes.contains_key("alpha"));
        assert_eq!(tree.dirs.len(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn structural_rescan_does_not_block_event_handler() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join("README.md"), "# Home\n").expect("write readme");
        let config =
            ServeConfig::from_inputs(vec![dir.path().to_path_buf()], 0, true).expect("config");
        let shared: SharedConfig = Arc::new(RwLock::new(Arc::new(config)));
        let clients = Arc::new(Mutex::new(Vec::new()));
        let fifo = dir.path().join("blocked.md");
        let status = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .expect("run mkfifo");
        assert!(status.success(), "mkfifo failed");
        let event = notify::Event {
            kind: EventKind::Create(notify::event::CreateKind::File),
            paths: vec![fifo.clone()],
            attrs: notify::event::EventAttributes::new(),
        };
        let (done_tx, done_rx) = mpsc::channel();
        let shared_for_event = shared.clone();
        let clients_for_event = clients.clone();
        let input = dir.path().to_path_buf();

        std::thread::spawn(move || {
            handle_fs_events(
                vec![event],
                &shared_for_event,
                &clients_for_event,
                &test_stores(),
                &[input],
                0,
                true,
            );
            done_tx.send(()).expect("send done");
        });

        assert!(
            done_rx.recv_timeout(Duration::from_millis(250)).is_ok(),
            "structural rescan blocked the watcher event handler"
        );
        std::fs::write(&fifo, "# Blocked\n").expect("unblock fifo reader");
    }

    #[test]
    fn older_rescan_cannot_replace_the_newer_published_collection() {
        let dir = tempfile::tempdir().unwrap();
        let inputs = vec![dir.path().to_owned()];
        fs::write(dir.path().join("README.md"), "# Home").unwrap();
        let mut original = ServeConfig::from_inputs(inputs.clone(), 0, false).unwrap();
        original.session.review_token = Some("session".to_owned());
        let shared: SharedConfig = Arc::new(RwLock::new(Arc::new(original)));
        let (tx, rx) = mpsc::channel();
        let clients = Arc::new(Mutex::new(vec![(0, tx)]));
        let older = ServeConfig::from_inputs(inputs.clone(), 0, false).unwrap();
        fs::write(dir.path().join("next.md"), "# Next").unwrap();
        let newer = ServeConfig::from_inputs(inputs, 0, false).unwrap();
        publish_rescan(&shared, &clients, &test_stores(), newer, 2);
        publish_rescan(&shared, &clients, &test_stores(), older, 1);
        let config = shared.read().unwrap();
        assert!(config.document_by_route("/next.md").is_some());
        assert_eq!(config.session.review_token.as_deref(), Some("session"));
        assert_eq!(rx.try_recv(), Ok(ReloadKind::Structure));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn rescan_syncs_store_watches_without_holding_the_config_write_lock() {
        let dir = tempfile::tempdir().unwrap();
        let inputs = vec![dir.path().to_owned()];
        fs::write(dir.path().join("README.md"), "# Home").unwrap();
        fs::write(dir.path().join("old.md"), "# Old").unwrap();
        let mut original = ServeConfig::from_inputs(inputs.clone(), 0, false).unwrap();
        original.session.review_token = Some("session".to_owned());
        original.session.api_token = Some("api".to_owned());
        original.session.blocking_review = true;
        let old = original
            .document_by_route("/old.md")
            .unwrap()
            .source_path
            .clone();
        let shared: SharedConfig = Arc::new(RwLock::new(Arc::new(original)));
        let clients = Arc::new(Mutex::new(Vec::new()));
        let stores = test_stores();
        stores.sync(&shared);
        assert!(stores.watches(&old));

        // Whether a request could read the config each time the watches
        // were synced: store work stats and opens directories, and must not
        // stall every connection behind the write lock.
        let readable = Arc::new(Mutex::new(Vec::new()));
        let (observed, config) = (readable.clone(), shared.clone());
        stores.on_sync(move || observed.lock().unwrap().push(config.try_read().is_ok()));

        fs::remove_file(dir.path().join("old.md")).unwrap();
        fs::write(dir.path().join("next.md"), "# Next").unwrap();
        let fresh = ServeConfig::from_inputs(inputs, 0, false).unwrap();
        publish_rescan(&shared, &clients, &stores, fresh, 7);

        let readable = readable.lock().unwrap();
        assert!(!readable.is_empty(), "the rescan did not sync the watches");
        assert!(
            readable.iter().all(|readable| *readable),
            "store work ran under the config write lock"
        );
        // The session's own state survives the swap...
        let config = shared.read().unwrap();
        assert_eq!(
            config.session,
            ServeSession {
                review_token: Some("session".to_owned()),
                api_token: Some("api".to_owned()),
                blocking_review: true,
                rescan_id: 7,
            }
        );
        // ...and the watches follow the published collection.
        let next = &config.document_by_route("/next.md").unwrap().source_path;
        assert!(
            stores.watches(next),
            "a blocking review watches every store"
        );
        assert!(!stores.watches(&old), "a dropped document is forgotten");
    }

    #[test]
    fn new_directory_event_discovers_files_created_before_recursive_watch() {
        let dir = tempfile::tempdir().expect("temp dir");
        fs::write(dir.path().join("README.md"), "# Home").unwrap();
        let config = ServeConfig::from_inputs(vec![dir.path().to_path_buf()], 0, true).unwrap();
        let shared = Arc::new(RwLock::new(Arc::new(config)));
        let (tx, rx) = mpsc::channel();
        let clients = Arc::new(Mutex::new(vec![(0, tx)]));
        let nested = dir.path().join("guides");
        fs::create_dir(&nested).unwrap();
        fs::write(nested.join("setup.md"), "# Setup").unwrap();
        // Inotify may only report the directory: setup.md was created before
        // the backend installed its watch on that new directory.
        handle_fs_events(
            vec![
                notify::Event::new(EventKind::Create(notify::event::CreateKind::Folder))
                    .add_path(nested),
            ],
            &shared,
            &clients,
            &test_stores(),
            &[dir.path().to_path_buf()],
            0,
            true,
        );
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(1)),
            Ok(ReloadKind::Structure)
        );
        assert!(shared
            .read()
            .unwrap()
            .documents
            .iter()
            .any(|doc| doc.route_path == "/guides/setup.md"));
    }

    #[test]
    fn store_changes_inside_the_served_tree_do_not_reload_documents() {
        let dir = tempfile::tempdir().expect("temp dir");
        let readme = dir.path().join("README.md");
        fs::write(&readme, "# Home").unwrap();
        let config = ServeConfig::from_inputs(vec![dir.path().to_path_buf()], 0, true).unwrap();
        let shared: SharedConfig = Arc::new(RwLock::new(Arc::new(config)));
        let (tx, rx) = mpsc::channel();
        let clients = Arc::new(Mutex::new(vec![(0, tx)]));
        let store = dir.path().join(".markview/annotations");
        fs::create_dir_all(&store).unwrap();
        fs::write(store.join("README.md"), "## a1 [open] document\n\nNote\n").unwrap();
        let handle = |kind: EventKind, path: PathBuf| {
            handle_fs_events(
                vec![notify::Event::new(kind).add_path(path)],
                &shared,
                &clients,
                &test_stores(),
                &[dir.path().to_path_buf()],
                0,
                true,
            );
        };
        // Everything a store write does: directories, the store file, its
        // lock and its temporary file. The store watcher reports these as
        // `annotations` events; the document must not be re-rendered too.
        let created = EventKind::Create(notify::event::CreateKind::Any);
        let modified = EventKind::Modify(ModifyKind::Any);
        for (kind, path) in [
            (created, dir.path().join(".markview")),
            (created, store.clone()),
            (created, store.join("README.md")),
            (modified, store.join("README.md")),
            (created, store.join("README.md.markview-lock")),
            (created, store.join(".markview-0123.tmp")),
            (
                EventKind::Remove(notify::event::RemoveKind::Any),
                store.join("README.md"),
            ),
        ] {
            handle(kind, path.clone());
            assert!(
                rx.recv_timeout(Duration::from_millis(100)).is_err(),
                "{} reloaded the document",
                path.display()
            );
        }
        // The same handler does reload for the document itself.
        handle(modified, readme);
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(1)),
            Ok(ReloadKind::Content)
        );
    }

    #[test]
    fn structural_event_bursts_share_one_rescan() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join("README.md"), "# Home\n").expect("write readme");
        let config =
            ServeConfig::from_inputs(vec![dir.path().to_path_buf()], 0, true).expect("config");
        let shared: SharedConfig = Arc::new(RwLock::new(Arc::new(config)));
        let (client_tx, client_rx) = mpsc::channel();
        let clients = Arc::new(Mutex::new(vec![(0, client_tx)]));
        let one = dir.path().join("one.md");
        let two = dir.path().join("two.md");
        std::fs::write(&one, "# One\n").expect("write one");
        std::fs::write(&two, "# Two\n").expect("write two");
        let events = vec![
            notify::Event {
                kind: EventKind::Create(notify::event::CreateKind::File),
                paths: vec![one],
                attrs: notify::event::EventAttributes::new(),
            },
            notify::Event {
                kind: EventKind::Create(notify::event::CreateKind::File),
                paths: vec![two],
                attrs: notify::event::EventAttributes::new(),
            },
        ];

        handle_fs_events(
            events,
            &shared,
            &clients,
            &test_stores(),
            &[dir.path().to_path_buf()],
            0,
            true,
        );

        assert_eq!(
            client_rx.recv_timeout(Duration::from_secs(1)),
            Ok(ReloadKind::Structure)
        );
        assert!(
            client_rx.recv_timeout(Duration::from_millis(250)).is_err(),
            "burst emitted more than one reload notification"
        );
    }

    #[test]
    fn shared_config_allows_concurrent_snapshot_readers() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join("README.md"), "# Home\n").expect("write readme");
        let config =
            ServeConfig::from_inputs(vec![dir.path().to_path_buf()], 0, false).expect("config");
        let shared: SharedConfig = Arc::new(RwLock::new(Arc::new(config)));

        let first = shared.read().expect("first read lock");
        let second = shared
            .try_read()
            .expect("second read lock should not block another reader");

        assert_eq!(first.default_document, second.default_document);
    }

    #[test]
    fn event_paths_are_canonicalized_before_known_document_lookup() {
        let cwd = std::env::current_dir().expect("cwd");
        let dir = tempfile::tempdir_in(&cwd).expect("temp dir");
        let file = dir.path().join("README.md");
        std::fs::write(&file, "# Home\n").expect("write readme");
        let config = ServeConfig::from_inputs(vec![file.clone()], 0, false).expect("config");
        let shared: SharedConfig = Arc::new(RwLock::new(Arc::new(config)));
        let (client_tx, client_rx) = mpsc::channel();
        let clients = Arc::new(Mutex::new(vec![(0, client_tx)]));
        let relative_file = file
            .strip_prefix(&cwd)
            .expect("relative file")
            .to_path_buf();
        let event = notify::Event {
            kind: EventKind::Modify(notify::event::ModifyKind::Data(
                notify::event::DataChange::Content,
            )),
            paths: vec![relative_file],
            attrs: notify::event::EventAttributes::new(),
        };

        handle_fs_events(
            vec![event],
            &shared,
            &clients,
            &test_stores(),
            &[],
            0,
            false,
        );

        assert_eq!(
            client_rx.recv_timeout(Duration::from_secs(1)),
            Ok(ReloadKind::Content)
        );
    }

    #[cfg(unix)]
    #[test]
    fn accept_loop_ends_only_when_the_listener_itself_is_lost() {
        for (errno, failure) in [
            (libc::EMFILE, AcceptFailure::Exhausted),
            (libc::ENFILE, AcceptFailure::Exhausted),
            (libc::ENOBUFS, AcceptFailure::Exhausted),
            (libc::ENOMEM, AcceptFailure::Exhausted),
            (libc::ECONNABORTED, AcceptFailure::Connection),
            (libc::EPROTO, AcceptFailure::Connection),
            (libc::EINTR, AcceptFailure::Interrupted),
            (libc::EBADF, AcceptFailure::Lost),
        ] {
            let error = io::Error::from_raw_os_error(errno);
            assert_eq!(accept_failure(&error), failure, "{error}");
        }
        // A descriptor that is not a socket can never accept: the loop
        // returns the error instead of spinning on it.
        let listener = TcpListener::from(OwnedFd::from(fs::File::open("/dev/null").unwrap()));
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("README.md"), "# Home").unwrap();
        let config = ServeConfig::from_inputs(vec![dir.path().to_owned()], 0, false).unwrap();
        let shared: SharedConfig = Arc::new(RwLock::new(Arc::new(config)));
        let clients: Clients = Arc::new(Mutex::new(Vec::new()));
        let lost = accept_connections(&listener, &shared, &clients, &test_stores(), None);
        assert_eq!(lost.raw_os_error(), Some(libc::ENOTSOCK));
    }

    fn test_stores() -> Arc<serve_annotations::Stores> {
        let shared: SharedConfig = Arc::new(RwLock::new(Arc::new(
            ServeConfig::from_inputs(vec![PathBuf::from("README.md")], 0, false).unwrap(),
        )));
        serve_annotations::watch_stores(shared, Arc::new(Mutex::new(Vec::new()))).unwrap()
    }

    fn served_doc(route_path: &str) -> ServedDocument {
        ServedDocument {
            source_path: PathBuf::from(route_path.trim_start_matches('/')),
            route_path: route_path.to_owned(),
            title: route_path.to_owned(),
        }
    }
}
