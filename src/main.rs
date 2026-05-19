use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;

use markview::{help, render, Cli, FrontendRenderer, HtmlRenderer, MarkdownDocument, OutputFormat};
use notify::{Config, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

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
        let input = cli
            .input
            .as_deref()
            .ok_or(markview::CliError::MissingServeInput)?;
        serve_markdown(PathBuf::from(input), port)?;
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

fn serve_markdown(path: PathBuf, port: u16) -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind(("127.0.0.1", port)).map_err(|error| {
        if error.kind() == io::ErrorKind::AddrInUse {
            format!("port {port} is already in use")
        } else {
            format!("failed to bind localhost:{port}: {error}")
        }
    })?;
    let address = listener.local_addr()?;
    let canonical_path = path.canonicalize()?;
    let clients = Arc::new(Mutex::new(Vec::new()));
    let _watcher = watch_file(canonical_path.clone(), clients.clone())?;

    println!(
        "Serving on http://localhost:{} — press Ctrl+C to stop",
        address.port()
    );
    io::stdout().flush()?;

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let path = canonical_path.clone();
                let clients = clients.clone();
                thread::spawn(move || {
                    if let Err(error) = handle_connection(stream, &path, clients) {
                        eprintln!("markview: serve error: {error}");
                    }
                });
            }
            Err(error) => eprintln!("markview: connection failed: {error}"),
        }
    }

    Ok(())
}

fn watch_file(
    path: PathBuf,
    clients: Arc<Mutex<Vec<mpsc::Sender<()>>>>,
) -> notify::Result<RecommendedWatcher> {
    let watch_path = path.clone();
    let (changes_tx, changes_rx) = mpsc::channel();
    let mut watcher = RecommendedWatcher::new(
        move |result: notify::Result<notify::Event>| {
            if let Ok(event) = result {
                if is_reload_event(&event.kind)
                    && event.paths.iter().any(|event_path| {
                        event_path
                            .canonicalize()
                            .map(|event_path| event_path == watch_path)
                            .unwrap_or(false)
                    })
                {
                    let _ = changes_tx.send(());
                }
            }
        },
        Config::default(),
    )?;

    let directory = path.parent().unwrap_or_else(|| Path::new("."));
    watcher.watch(directory, RecursiveMode::NonRecursive)?;
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
    path: &Path,
    clients: Arc<Mutex<Vec<mpsc::Sender<()>>>>,
) -> io::Result<()> {
    let mut request = String::new();
    {
        let mut reader = BufReader::new(stream.try_clone()?);
        reader.read_line(&mut request)?;
    }

    let route = request.split_whitespace().nth(1).unwrap_or("/");
    match route {
        "/" => serve_document(&mut stream, path),
        "/events" => serve_events(stream, clients),
        _ => write_response(
            &mut stream,
            "404 Not Found",
            "text/plain; charset=utf-8",
            "Not found",
        ),
    }
}

fn serve_document(stream: &mut TcpStream, path: &Path) -> io::Result<()> {
    match render_file(path) {
        Ok(html) => write_response(stream, "200 OK", "text/html; charset=utf-8", &html),
        Err(error) => write_response(
            stream,
            "500 Internal Server Error",
            "text/plain; charset=utf-8",
            &format!("Failed to render document: {error}"),
        ),
    }
}

fn serve_events(
    mut stream: TcpStream,
    clients: Arc<Mutex<Vec<mpsc::Sender<()>>>>,
) -> io::Result<()> {
    let (tx, rx) = mpsc::channel();
    clients.lock().expect("clients lock").push(tx);
    stream.write_all(
        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\n\r\n",
    )?;
    stream.flush()?;

    while rx.recv().is_ok() {
        stream.write_all(b"data: reload\n\n")?;
        stream.flush()?;
    }

    Ok(())
}

fn render_file(path: &Path) -> io::Result<String> {
    let markdown = fs::read_to_string(path)?;
    let document = MarkdownDocument::from_path(&markdown, path.display().to_string());
    Ok(inject_serve_script(
        &HtmlRenderer.render_document(&document),
    ))
}

fn inject_serve_script(html: &str) -> String {
    let script = r#"<script>
(() => {
  const events = new EventSource('/events');
  events.onmessage = async (event) => {
    if (event.data !== 'reload') return;
    const response = await fetch('/', { cache: 'no-store' });
    const text = await response.text();
    const next = new DOMParser().parseFromString(text, 'text/html');
    const currentMain = document.querySelector('main');
    const nextMain = next.querySelector('main');
    if (currentMain && nextMain) currentMain.innerHTML = nextMain.innerHTML;
    if (next.title) document.title = next.title;
  };
})();
</script>"#;

    html.replace("</body>", &format!("{script}\n</body>"))
}

fn write_response(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &str,
) -> io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}
