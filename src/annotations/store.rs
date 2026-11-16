use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Open,
    Resolved,
}

impl Status {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "open" => Some(Self::Open),
            "resolved" => Some(Self::Resolved),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Resolved => "resolved",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Annotation {
    pub id: u64,
    pub status: Status,
    pub lines: Option<(usize, usize)>,
    pub metadata: BTreeMap<String, String>,
    pub quote: Option<String>,
    pub body: String,
    raw_metadata: Option<String>,
}

impl Annotation {
    /// Records a machine hint. A hand-edited metadata line that does not
    /// parse is written back verbatim, so it takes no new fields.
    pub(super) fn hint(&mut self, key: &str, value: String) {
        if self.raw_metadata.is_none() {
            self.metadata.insert(key.to_owned(), value);
        }
    }

    pub(super) fn new(id: u64, body: String) -> Self {
        Self {
            id,
            status: Status::Open,
            lines: None,
            metadata: BTreeMap::new(),
            quote: None,
            body,
            raw_metadata: None,
        }
    }
}

#[derive(Debug)]
pub struct Store {
    pub preamble: String,
    pub annotations: Vec<Annotation>,
    /// Every line in the file read ended with CRLF, so writes keep CRLF.
    crlf: bool,
}

const BYTE_ORDER_MARK: char = '\u{feff}';

fn number<T: std::str::FromStr>(value: &str) -> Option<T> {
    (!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| value.parse().ok())
        .flatten()
}

type Header = (u64, Status, Option<(usize, usize)>);

fn header(line: &str) -> Option<Header> {
    let (id, rest) = line.strip_prefix("## a")?.split_once(" [")?;
    let (status, anchor) = rest.split_once("] ")?;
    let lines = if anchor == "document" {
        None
    } else if let Some(first) = anchor.strip_prefix("line ") {
        let first = number(first)?;
        Some((first, first))
    } else {
        let (first, last) = anchor.strip_prefix("lines ")?.split_once('-')?;
        Some((number(first)?, number(last)?))
    };
    if lines.is_some_and(|(first, last)| first == 0 || last < first) {
        return None;
    }
    Some((number(id)?, Status::parse(status)?, lines))
}

fn metadata(line: &str) -> Option<BTreeMap<String, String>> {
    let mut fields = BTreeMap::new();
    let mut input = line.strip_prefix("<!-- markview:")?.strip_suffix(" -->")?;
    while !input.is_empty() {
        input = input.strip_prefix(' ')?;
        let (key, rest) = input.split_once("=\"")?;
        if key.is_empty()
            || !key
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
        {
            return None;
        }
        let (value, rest) = rest.split_once('"')?;
        if fields.insert(key.to_owned(), decode_value(value)).is_some() {
            return None;
        }
        input = rest;
    }
    Some(fields)
}

fn decode_value(mut input: &str) -> String {
    let mut result = String::new();
    while let Some((prefix, suffix)) = input.split_once('&') {
        result.push_str(prefix);
        let entity = [
            ("amp;", '&'),
            ("quot;", '"'),
            ("lt;", '<'),
            ("gt;", '>'),
            ("#45;", '-'),
            ("#10;", '\n'),
            ("#13;", '\r'),
        ]
        .into_iter()
        .find_map(|(name, value)| suffix.strip_prefix(name).map(|rest| (value, rest)));
        if let Some((value, rest)) = entity {
            result.push(value);
            input = rest;
        } else {
            result.push('&');
            input = suffix;
        }
    }
    result.push_str(input);
    result
}

fn encode_value(value: &str) -> String {
    let mut result = String::new();
    let mut previous_hyphen = false;
    for ch in value.chars() {
        match ch {
            '&' => result.push_str("&amp;"),
            '"' => result.push_str("&quot;"),
            '<' => result.push_str("&lt;"),
            '>' => result.push_str("&gt;"),
            '-' if previous_hyphen => result.push_str("&#45;"),
            '\n' => result.push_str("&#10;"),
            '\r' => result.push_str("&#13;"),
            _ => result.push(ch),
        }
        previous_hyphen = ch == '-';
    }
    result
}

pub(super) fn trim_body(body: &str) -> &str {
    let first = body
        .split_inclusive('\n')
        .find(|line| !line.trim().is_empty());
    let Some(first) = first else { return "" };
    let start = first.as_ptr() as usize - body.as_ptr() as usize;
    let last = body
        .rsplit('\n')
        .find(|line| !line.trim().is_empty())
        .unwrap_or(first);
    let end = last.as_ptr() as usize - body.as_ptr() as usize + last.len();
    body[start..end]
        .strip_suffix('\r')
        .unwrap_or(&body[start..end])
}

impl Store {
    pub fn generated_preamble(relative: &Path) -> String {
        let name = relative.to_string_lossy().replace(['\r', '\n'], " ");
        format!("# Annotations: {name}\n\n<!-- markview-annotations v1. To resolve one, change [open] to [resolved]. Deleting a section deletes the annotation. Prefix a header-shaped body line with a backslash. -->\n\n")
    }

    pub fn new(preamble: String) -> Self {
        Self {
            preamble,
            annotations: Vec::new(),
            crlf: false,
        }
    }

    /// Lines that look like a section header but do not parse as one, with
    /// their 1-based line numbers: `## a<N>` followed by a space or `[`.
    /// A typo such as `[OPEN]` or a trailing space lands here.
    pub(super) fn near_headers(source: &str) -> impl Iterator<Item = (usize, &str)> {
        source
            .strip_prefix(BYTE_ORDER_MARK)
            .unwrap_or(source)
            .lines()
            .enumerate()
            .filter(|(_, line)| {
                let Some(rest) = line.strip_prefix("## a") else {
                    return false;
                };
                let rest = rest.trim_start_matches(|ch: char| ch.is_ascii_digit());
                rest.len() < line.len() - 4
                    && rest.starts_with([' ', '['])
                    && header(line).is_none()
            })
            .map(|(index, line)| (index + 1, line))
    }

    pub fn parse(source: &str) -> Self {
        // Windows editors may prepend a byte order mark. It is kept in the
        // preamble but must not hide a header on the first line.
        let content = source.strip_prefix(BYTE_ORDER_MARK).unwrap_or(source);
        let mark = &source[..source.len() - content.len()];
        let mut store = Self::parse_content(content);
        store.preamble.insert_str(0, mark);
        store
    }

    fn parse_content(source: &str) -> Self {
        let crlf = source.contains("\r\n") && !source.replace("\r\n", "").contains('\n');
        let mut sections = Vec::new();
        let mut offset = 0;
        for line in source.split_inclusive('\n') {
            if let Some((id, status, lines)) = header(
                line.trim_end_matches('\n')
                    .strip_suffix('\r')
                    .unwrap_or(line.trim_end_matches('\n')),
            ) {
                sections.push((offset, offset + line.len(), id, status, lines));
            }
            offset += line.len();
        }
        let preamble =
            source[..sections.first().map_or(source.len(), |section| section.0)].to_owned();
        let annotations = sections
            .iter()
            .enumerate()
            .map(|(index, &(_, start, id, status, lines))| {
                let end = sections
                    .get(index + 1)
                    .map_or(source.len(), |section| section.0);
                let (first, remaining) = source[start..end]
                    .split_once('\n')
                    .unwrap_or((&source[start..end], ""));
                let first = first.strip_suffix('\r').unwrap_or(first);
                let mut annotation = Annotation::new(id, String::new());
                annotation.status = status;
                annotation.lines = lines;
                let mut content = &source[start..end];
                if first.starts_with("<!-- markview:") {
                    if let Some(fields) = metadata(first) {
                        annotation.metadata = fields;
                    } else {
                        annotation.raw_metadata = Some(first.to_owned());
                    }
                    content = remaining;
                }
                content = trim_body(content);
                if lines.is_some() {
                    let mut quoted = Vec::new();
                    while let Some(line) = content.lines().next() {
                        let Some(value) = line.strip_prefix('>') else {
                            break;
                        };
                        quoted.push(value.strip_prefix(' ').unwrap_or(value));
                        content = content.split_once('\n').map_or("", |(_, rest)| rest);
                    }
                    if !quoted.is_empty() {
                        annotation.quote = Some(super::collapse(&quoted.join(" ")));
                    }
                }
                annotation.body = trim_body(content)
                    .split('\n')
                    // A CRLF store keeps its line endings out of the comment,
                    // so an edited body is written back with CRLF as well.
                    .map(|line| match line.strip_suffix('\r') {
                        Some(stripped) if crlf => stripped,
                        _ => line,
                    })
                    .map(|line| {
                        let unescaped = line.trim_start_matches('\\');
                        if unescaped.len() < line.len()
                            && header(unescaped.strip_suffix('\r').unwrap_or(unescaped)).is_some()
                        {
                            &line[1..]
                        } else {
                            line
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                annotation
            })
            .collect();
        Self {
            preamble,
            annotations,
            crlf,
        }
    }

    pub fn encode(&self) -> String {
        let mut output = String::new();
        for record in &self.annotations {
            let anchor = match record.lines {
                Some((first, last)) if first != last => format!("lines {first}-{last}"),
                Some((first, _)) => format!("line {first}"),
                None => "document".to_owned(),
            };
            let _ = writeln!(
                output,
                "## a{} [{}] {anchor}",
                record.id,
                record.status.label()
            );
            if let Some(raw) = &record.raw_metadata {
                let _ = writeln!(output, "{raw}");
            } else if !record.metadata.is_empty() {
                output.push_str("<!-- markview:");
                for (key, value) in &record.metadata {
                    let _ = write!(output, " {key}=\"{}\"", encode_value(value));
                }
                output.push_str(" -->\n");
            }
            output.push('\n');
            if let Some(quote) = &record.quote {
                let _ = writeln!(output, "> {quote}\n");
            }
            for line in trim_body(&record.body).split_terminator('\n') {
                if header(
                    line.trim_start_matches('\\')
                        .strip_suffix('\r')
                        .unwrap_or(line.trim_start_matches('\\')),
                )
                .is_some()
                {
                    output.push('\\');
                }
                let _ = writeln!(output, "{line}");
            }
            output.push('\n');
        }
        if self.crlf {
            output = output.replace("\r\n", "\n").replace('\n', "\r\n");
        }
        let mut content = self.preamble.clone();
        let text = content.strip_prefix(BYTE_ORDER_MARK).unwrap_or(&content);
        if !text.is_empty() && !text.ends_with('\n') && !output.is_empty() {
            content.push_str(if self.crlf { "\r\n" } else { "\n" });
        }
        content + &output
    }

    /// Refuses a store in which an ID names more than one section: an edit,
    /// status change or delete could not tell them apart.
    pub(super) fn check_ids(&self) -> Result<(), super::Error> {
        let mut ids = std::collections::HashSet::new();
        match self
            .annotations
            .iter()
            .find(|record| !ids.insert(record.id))
        {
            Some(record) => Err(super::Error::Section(
                record.id,
                "appears more than once; give each section a unique ID",
            )),
            None => Ok(()),
        }
    }

    /// Encodes the store, then reads the result back. A section that would
    /// not come back as it is now cannot be written without losing content,
    /// so the write is refused and the error names that annotation.
    pub(super) fn encode_checked(&self) -> Result<String, super::Error> {
        let content = self.encode();
        let reread = Self::parse(&content);
        let mut written = reread.annotations.iter();
        for record in &self.annotations {
            if written.next() != Some(record) {
                return Err(super::Error::Section(
                    record.id,
                    "cannot be saved without losing content; edit the store file instead",
                ));
            }
        }
        if written.next().is_some() || reread.preamble != self.preamble {
            return Err(super::Error::Invalid(
                "annotation store cannot be rewritten without losing content",
            ));
        }
        Ok(content)
    }
}
