use std::path::Path;

use pulldown_cmark::{html, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const ITALIC: &str = "\x1b[3m";
const UNDERLINE: &str = "\x1b[4m";
const CYAN: &str = "\x1b[36m";
const GREEN: &str = "\x1b[32m";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderOptions {
    pub color: bool,
    pub width: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkdownDocument {
    source: String,
    title: String,
}

impl MarkdownDocument {
    pub fn new(source: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            title: "Untitled Markdown".to_owned(),
        }
    }

    pub fn with_title(source: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            title: title.into(),
        }
    }

    pub fn from_path(source: impl Into<String>, path: impl AsRef<Path>) -> Self {
        let title = path
            .as_ref()
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("Markdown")
            .to_owned();
        Self::with_title(source, title)
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn title(&self) -> &str {
        &self.title
    }
}

pub trait FrontendRenderer {
    type Output;

    fn render_document(&self, document: &MarkdownDocument) -> Self::Output;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalRenderer {
    options: RenderOptions,
}

impl TerminalRenderer {
    pub fn new(options: RenderOptions) -> Self {
        Self { options }
    }
}

impl FrontendRenderer for TerminalRenderer {
    type Output = String;

    fn render_document(&self, document: &MarkdownDocument) -> Self::Output {
        render_terminal(document.source(), self.options)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HtmlRenderer;

impl FrontendRenderer for HtmlRenderer {
    type Output = String;

    fn render_document(&self, document: &MarkdownDocument) -> Self::Output {
        render_html_document(document)
    }
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            color: true,
            width: 88,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliError {
    MissingValue(&'static str),
    InvalidWidth(String),
    UnknownArgument(String),
    TooManyInputs,
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingValue(flag) => write!(f, "missing value for {flag}"),
            Self::InvalidWidth(value) => write!(f, "invalid width: {value}"),
            Self::UnknownArgument(arg) => write!(f, "unknown argument: {arg}"),
            Self::TooManyInputs => write!(f, "expected at most one input file"),
        }
    }
}

impl std::error::Error for CliError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cli {
    pub input: Option<String>,
    pub options: RenderOptions,
    pub help: bool,
}

impl Cli {
    pub fn parse<I, S>(args: I) -> Result<Self, CliError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut options = RenderOptions::default();
        let mut input = None;
        let mut help = false;
        let mut args = args.into_iter().map(Into::into);

        while let Some(arg) = args.next() {
            match arg.as_str() {
                "-h" | "--help" => help = true,
                "--no-color" => options.color = false,
                "-w" | "--width" => {
                    let value = args.next().ok_or(CliError::MissingValue("--width"))?;
                    options.width = parse_width(&value)?;
                }
                _ if arg.starts_with("--width=") => {
                    let value = arg.trim_start_matches("--width=");
                    options.width = parse_width(value)?;
                }
                _ if arg.starts_with('-') => return Err(CliError::UnknownArgument(arg)),
                _ => {
                    if input.replace(arg).is_some() {
                        return Err(CliError::TooManyInputs);
                    }
                }
            }
        }

        Ok(Self {
            input,
            options,
            help,
        })
    }
}

fn parse_width(value: &str) -> Result<usize, CliError> {
    let width = value
        .parse::<usize>()
        .map_err(|_| CliError::InvalidWidth(value.to_owned()))?;

    if width < 20 {
        return Err(CliError::InvalidWidth(value.to_owned()));
    }

    Ok(width)
}

pub fn help() -> &'static str {
    "Usage: markview [OPTIONS] [FILE]\n\nReads FILE or stdin and renders Markdown for the terminal.\n\nOptions:\n  -w, --width <COLUMNS>  Wrap text to a target width (minimum 20, default 88)\n      --no-color         Disable ANSI styling\n  -h, --help             Show this help\n"
}

pub fn render(markdown: &str, options: RenderOptions) -> String {
    render_terminal(markdown, options)
}

pub fn render_html(markdown: &str) -> String {
    HtmlRenderer.render_document(&MarkdownDocument::new(markdown))
}

fn render_terminal(markdown: &str, options: RenderOptions) -> String {
    let parser = Parser::new_ext(markdown, markdown_options());
    let mut renderer = Renderer::new(options);

    for event in parser {
        renderer.event(event);
    }

    renderer.finish()
}

fn render_html_document(document: &MarkdownDocument) -> String {
    let mut body = String::new();
    html::push_html(
        &mut body,
        Parser::new_ext(document.source(), markdown_options()),
    );

    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{}</title>
<style>
:root {{
  color-scheme: light dark;
  --bg: #f8f7f4;
  --fg: #242220;
  --muted: #68625c;
  --rule: #d8d2ca;
  --accent: #0f766e;
  --code-bg: #ebe6de;
  --quote-bg: #f1ede7;
}}
@media (prefers-color-scheme: dark) {{
  :root {{
    --bg: #181715;
    --fg: #eeeae4;
    --muted: #aaa39a;
    --rule: #39342f;
    --accent: #5eead4;
    --code-bg: #25221f;
    --quote-bg: #211f1c;
  }}
}}
* {{ box-sizing: border-box; }}
body {{
  margin: 0;
  background: var(--bg);
  color: var(--fg);
  font: 16px/1.65 -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
}}
main {{
  width: min(860px, calc(100vw - 48px));
  margin: 0 auto;
  padding: 40px 0 64px;
}}
h1, h2, h3, h4, h5, h6 {{
  line-height: 1.2;
  letter-spacing: 0;
  margin: 1.7em 0 0.55em;
}}
h1 {{ font-size: 2.35rem; margin-top: 0; }}
h2 {{ font-size: 1.7rem; padding-bottom: 0.25rem; border-bottom: 1px solid var(--rule); }}
h3 {{ font-size: 1.28rem; }}
p, ul, ol, blockquote, pre, table {{ margin: 0 0 1.05rem; }}
a {{ color: var(--accent); text-underline-offset: 0.18em; }}
blockquote {{
  border-left: 4px solid var(--accent);
  background: var(--quote-bg);
  margin-left: 0;
  padding: 0.75rem 1rem;
  color: var(--muted);
}}
code {{
  font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
  font-size: 0.92em;
  background: var(--code-bg);
  border-radius: 5px;
  padding: 0.12em 0.35em;
}}
pre {{
  overflow: auto;
  background: var(--code-bg);
  border: 1px solid var(--rule);
  border-radius: 8px;
  padding: 1rem;
}}
pre code {{ background: transparent; padding: 0; }}
table {{
  width: 100%;
  border-collapse: collapse;
  display: block;
  overflow-x: auto;
}}
th, td {{
  border: 1px solid var(--rule);
  padding: 0.45rem 0.65rem;
  text-align: left;
}}
th {{ background: var(--code-bg); }}
img {{ max-width: 100%; height: auto; }}
hr {{ border: 0; border-top: 1px solid var(--rule); margin: 2rem 0; }}
</style>
</head>
<body>
<main>
{}
</main>
</body>
</html>
"#,
        escape_html(document.title()),
        body
    )
}

fn markdown_options() -> Options {
    Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH
}

fn escape_html(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

struct Renderer {
    out: String,
    options: RenderOptions,
    line: String,
    quote_depth: usize,
    list_stack: Vec<ListState>,
    code_block: bool,
    table_depth: usize,
    pending_link: Option<String>,
}

#[derive(Debug, Clone, Copy)]
struct ListState {
    next: Option<u64>,
}

impl Renderer {
    fn new(options: RenderOptions) -> Self {
        Self {
            out: String::new(),
            options,
            line: String::new(),
            quote_depth: 0,
            list_stack: Vec::new(),
            code_block: false,
            table_depth: 0,
            pending_link: None,
        }
    }

    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => {
                if self.code_block {
                    self.flush_line();
                    self.write_code(&text);
                } else {
                    self.push_text(&text);
                }
            }
            Event::Code(code) => self.push_styled(&format!("`{code}`"), GREEN),
            Event::Html(html) | Event::InlineHtml(html) => self.push_text(&html),
            Event::SoftBreak => self.push_text(" "),
            Event::HardBreak => self.flush_line(),
            Event::Rule => {
                self.blank();
                self.out.push_str(&"─".repeat(self.options.width.min(80)));
                self.out.push('\n');
                self.blank();
            }
            Event::InlineMath(math) => self.push_styled(&format!("${math}$"), GREEN),
            Event::DisplayMath(math) => {
                self.blank();
                self.push_styled(&format!("$$\n{math}\n$$"), GREEN);
                self.blank();
            }
            Event::FootnoteReference(reference) => self.push_text(&format!("[^{reference}]")),
            Event::TaskListMarker(checked) => {
                self.push_text(if checked { "[x] " } else { "[ ] " });
            }
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => {}
            Tag::Heading { level, .. } => {
                self.blank();
                self.push_styled(&heading_prefix(level), CYAN);
            }
            Tag::BlockQuote(_) => {
                self.flush_line();
                self.quote_depth += 1;
            }
            Tag::CodeBlock(kind) => {
                self.blank();
                if let CodeBlockKind::Fenced(language) = kind {
                    if !language.is_empty() {
                        self.push_styled(language.as_ref(), DIM);
                        self.flush_line();
                    }
                }
                self.code_block = true;
            }
            Tag::List(start) => {
                self.flush_line();
                self.list_stack.push(ListState { next: start });
            }
            Tag::Item => {
                self.flush_line();
                let marker = match self
                    .list_stack
                    .last_mut()
                    .and_then(|state| state.next.as_mut())
                {
                    Some(next) => {
                        let marker = format!("{next}. ");
                        *next += 1;
                        marker
                    }
                    None => "- ".to_owned(),
                };
                self.push_text(&marker);
            }
            Tag::Emphasis => self.push_style(ITALIC),
            Tag::Strong => self.push_style(BOLD),
            Tag::Strikethrough => self.push_text("~"),
            Tag::Link { dest_url, .. } => self.pending_link = Some(dest_url.to_string()),
            Tag::Image {
                title, dest_url, ..
            } => {
                let label = if title.is_empty() {
                    format!("[image: {dest_url}]")
                } else {
                    format!("[image: {title} - {dest_url}]")
                };
                self.push_styled(&label, UNDERLINE);
            }
            Tag::Table(_) => {
                self.blank();
                self.table_depth += 1;
            }
            Tag::TableHead | Tag::TableRow | Tag::TableCell => {}
            Tag::FootnoteDefinition(name) => {
                self.blank();
                self.push_text(&format!("[^{name}]: "));
            }
            Tag::MetadataBlock(_) => {}
            Tag::HtmlBlock => {}
            Tag::DefinitionList | Tag::DefinitionListTitle | Tag::DefinitionListDefinition => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph | TagEnd::Heading(_) => {
                self.flush_line();
                self.blank();
            }
            TagEnd::BlockQuote(_) => {
                self.flush_line();
                self.quote_depth = self.quote_depth.saturating_sub(1);
            }
            TagEnd::CodeBlock => {
                self.code_block = false;
                self.blank();
            }
            TagEnd::List(_) => {
                self.flush_line();
                self.list_stack.pop();
                self.blank();
            }
            TagEnd::Item => self.flush_line(),
            TagEnd::Emphasis | TagEnd::Strong => self.push_reset(),
            TagEnd::Strikethrough => self.push_text("~"),
            TagEnd::Link => {
                if let Some(dest) = self.pending_link.take() {
                    self.push_styled(&format!(" ({dest})"), DIM);
                }
            }
            TagEnd::Image => {}
            TagEnd::Table => {
                self.table_depth = self.table_depth.saturating_sub(1);
                self.blank();
            }
            TagEnd::TableHead | TagEnd::TableRow => self.flush_line(),
            TagEnd::TableCell => self.push_text(" | "),
            TagEnd::FootnoteDefinition => self.blank(),
            TagEnd::MetadataBlock(_) => {}
            TagEnd::HtmlBlock => {}
            TagEnd::DefinitionList
            | TagEnd::DefinitionListTitle
            | TagEnd::DefinitionListDefinition => self.flush_line(),
        }
    }

    fn push_text(&mut self, text: &str) {
        for word in text.split_whitespace() {
            self.push_word(word);
        }

        if text.ends_with('\n') {
            self.flush_line();
        }
    }

    fn push_word(&mut self, word: &str) {
        let indent = self.indent();
        let limit = self.options.width.max(indent.len() + 20);
        let projected =
            self.line.chars().count() + word.chars().count() + usize::from(!self.line.is_empty());

        if projected > limit && !self.line.trim().is_empty() {
            self.flush_line();
        }

        if self.line.is_empty() {
            self.line.push_str(&indent);
        } else if !self.line.ends_with(' ') && !attaches_to_previous(word) {
            self.line.push(' ');
        }

        self.line.push_str(word);
    }

    fn push_style(&mut self, style: &str) {
        if self.options.color {
            self.line.push_str(style);
        }
    }

    fn push_reset(&mut self) {
        if self.options.color {
            self.line.push_str(RESET);
        }
    }

    fn push_styled(&mut self, text: &str, style: &str) {
        self.push_style(style);
        self.push_text(text);
        self.push_reset();
    }

    fn write_code(&mut self, code: &str) {
        for raw_line in code.lines() {
            let indent = self.indent();
            if self.options.color {
                self.out.push_str(DIM);
            }
            self.out.push_str(&indent);
            self.out.push_str("    ");
            self.out.push_str(raw_line);
            if self.options.color {
                self.out.push_str(RESET);
            }
            self.out.push('\n');
        }
    }

    fn flush_line(&mut self) {
        if !self.line.trim().is_empty() {
            self.out.push_str(self.line.trim_end());
            self.out.push('\n');
        }
        self.line.clear();
    }

    fn blank(&mut self) {
        self.flush_line();
        if !self.out.ends_with("\n\n") {
            self.out.push('\n');
        }
    }

    fn finish(mut self) -> String {
        self.flush_line();
        while self.out.ends_with("\n\n") {
            self.out.pop();
        }
        if !self.out.is_empty() && !self.out.ends_with('\n') {
            self.out.push('\n');
        }
        self.out
    }

    fn indent(&self) -> String {
        let quote = "> ".repeat(self.quote_depth);
        let list = "  ".repeat(self.list_stack.len().saturating_sub(1));
        format!("{quote}{list}")
    }
}

fn heading_prefix(level: HeadingLevel) -> String {
    let marks = match level {
        HeadingLevel::H1 => "#",
        HeadingLevel::H2 => "##",
        HeadingLevel::H3 => "###",
        HeadingLevel::H4 => "####",
        HeadingLevel::H5 => "#####",
        HeadingLevel::H6 => "######",
    };
    format!("{marks} ")
}

fn attaches_to_previous(word: &str) -> bool {
    matches!(
        word,
        "." | "," | ":" | ";" | "!" | "?" | ")" | "]" | "}" | "'" | "\""
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(markdown: &str) -> String {
        render(
            markdown,
            RenderOptions {
                color: false,
                width: 60,
            },
        )
    }

    #[test]
    fn renders_headings_and_paragraphs() {
        let rendered = plain("# Hello\n\nThis is **small** and _fast_.\n");

        assert!(rendered.contains("# Hello\n"));
        assert!(rendered.contains("This is small and fast."));
    }

    #[test]
    fn renders_links_with_destinations() {
        let rendered = plain("Read [Rust](https://www.rust-lang.org/) today.");

        assert!(rendered.contains("Rust (https://www.rust-lang.org/)"));
    }

    #[test]
    fn renders_ordered_and_unordered_lists() {
        let rendered = plain("- alpha\n- beta\n\n3. third\n4. fourth\n");

        assert!(rendered.contains("- alpha"));
        assert!(rendered.contains("- beta"));
        assert!(rendered.contains("3. third"));
        assert!(rendered.contains("4. fourth"));
    }

    #[test]
    fn wraps_long_text_to_width() {
        let rendered = render(
            "one two three four five six seven eight nine ten",
            RenderOptions {
                color: false,
                width: 24,
            },
        );

        assert!(rendered.lines().all(|line| line.chars().count() <= 24));
        assert!(rendered.lines().count() > 1);
    }

    #[test]
    fn keeps_code_blocks_verbatim() {
        let rendered = plain("```rust\nfn main() {}\n```\n");

        assert!(rendered.contains("rust"));
        assert!(rendered.contains("    fn main() {}"));
    }

    #[test]
    fn parses_cli_defaults() {
        let cli = Cli::parse(["README.md"]).expect("valid args");

        assert_eq!(cli.input.as_deref(), Some("README.md"));
        assert_eq!(cli.options, RenderOptions::default());
        assert!(!cli.help);
    }

    #[test]
    fn parses_cli_options() {
        let cli = Cli::parse(["--no-color", "--width=40"]).expect("valid args");

        assert_eq!(cli.input, None);
        assert_eq!(
            cli.options,
            RenderOptions {
                color: false,
                width: 40,
            }
        );
    }

    #[test]
    fn rejects_tiny_width() {
        assert_eq!(
            Cli::parse(["--width", "12"]).expect_err("invalid width"),
            CliError::InvalidWidth("12".to_owned())
        );
    }

    #[test]
    fn renders_full_html_document() {
        let document = MarkdownDocument::with_title(
            "# Hello\n\nThis is **rendered** Markdown.",
            "Notes <draft>",
        );
        let html = HtmlRenderer.render_document(&document);

        assert!(html.contains("<title>Notes &lt;draft&gt;</title>"));
        assert!(html.contains("<h1>Hello</h1>"));
        assert!(html.contains("<strong>rendered</strong>"));
        assert!(html.contains("<main>"));
    }

    #[test]
    fn renderer_trait_allows_frontend_swap() {
        fn render_with<R: FrontendRenderer<Output = String>>(
            renderer: R,
            document: &MarkdownDocument,
        ) -> String {
            renderer.render_document(document)
        }

        let document = MarkdownDocument::new("A [link](https://example.com).");
        let terminal = render_with(
            TerminalRenderer::new(RenderOptions {
                color: false,
                width: 80,
            }),
            &document,
        );
        let html = render_with(HtmlRenderer, &document);

        assert!(terminal.contains("link (https://example.com)."));
        assert!(html.contains(r#"<a href="https://example.com">link</a>"#));
    }
}
