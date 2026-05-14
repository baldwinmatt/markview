use std::fs;
use std::io::{self, Read};
use std::path::PathBuf;
use std::process::ExitCode;

use markview::{FrontendRenderer, HtmlRenderer, MarkdownDocument};
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoop};
use tao::window::WindowBuilder;
use wry::WebViewBuilder;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("markview-gui: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args = GuiCli::parse(std::env::args().skip(1))?;

    if args.help {
        println!("{}", help());
        return Ok(());
    }

    let document = read_document(args.input)?;
    let html = HtmlRenderer.render_document(&document);

    let event_loop = EventLoop::new();
    let window = WindowBuilder::new()
        .with_title(format!("markview - {}", document.title()))
        .with_inner_size(tao::dpi::LogicalSize::new(920.0, 760.0))
        .build(&event_loop)?;

    let _webview = WebViewBuilder::new().with_html(html).build(&window)?;

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;

        if let Event::WindowEvent {
            event: WindowEvent::CloseRequested,
            ..
        } = event
        {
            *control_flow = ControlFlow::Exit;
        }
    });
}

fn read_document(input: Option<PathBuf>) -> Result<MarkdownDocument, Box<dyn std::error::Error>> {
    match input {
        Some(path) => {
            let source = fs::read_to_string(&path)?;
            Ok(MarkdownDocument::from_path(source, path))
        }
        None => {
            let mut source = String::new();
            io::stdin().read_to_string(&mut source)?;
            Ok(MarkdownDocument::with_title(source, "stdin"))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GuiCli {
    input: Option<PathBuf>,
    help: bool,
}

impl GuiCli {
    fn parse<I, S>(args: I) -> Result<Self, String>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut input = None;
        let mut help = false;

        for arg in args.into_iter().map(Into::into) {
            match arg.as_str() {
                "-h" | "--help" => help = true,
                _ if arg.starts_with('-') => return Err(format!("unknown argument: {arg}")),
                _ => {
                    if input.replace(PathBuf::from(arg)).is_some() {
                        return Err("expected at most one input file".to_owned());
                    }
                }
            }
        }

        Ok(Self { input, help })
    }
}

fn help() -> &'static str {
    "Usage: markview-gui [FILE]\n\nOpens FILE or stdin as rendered Markdown in a native WebKit window.\n\nOptions:\n  -h, --help  Show this help"
}
