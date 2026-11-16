use std::ops::Range;

use pulldown_cmark::{Event, Parser, Tag, TagEnd};

#[derive(Debug)]
struct Point {
    display: usize,
    line: usize,
    heading: Option<usize>,
}

#[derive(Debug)]
pub struct TextProjection {
    pub revision: String,
    scalars: Vec<char>,
    display: Vec<char>,
    points: Vec<Point>,
    sections: Vec<String>,
}

pub fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(super) fn normalized(text: &str) -> Vec<char> {
    text.chars().filter(|ch| !ch.is_whitespace()).collect()
}

impl TextProjection {
    pub fn new(source: &str) -> Self {
        let prepared = crate::prepare_source(source);
        Self::from_prepared(source, &prepared)
    }

    fn from_prepared(source: &str, prepared: &str) -> Self {
        let mut projection = Self {
            revision: super::source_revision(source),
            scalars: Vec::new(),
            display: Vec::new(),
            points: Vec::new(),
            sections: Vec::new(),
        };
        let bytes = prepared.as_bytes();
        let newlines: Vec<_> = bytes
            .iter()
            .enumerate()
            .filter_map(|(offset, byte)| {
                (*byte == b'\n' || (*byte == b'\r' && bytes.get(offset + 1) != Some(&b'\n')))
                    .then_some(offset)
            })
            .collect();
        let mut image_depth = 0;
        let mut heading_stack: Vec<(u8, String)> = Vec::new();
        let mut heading = None;
        let mut pending_heading = None;
        let mut pending_space = false;
        for (event, span) in Parser::new_ext(prepared, crate::markdown_options()).into_offset_iter()
        {
            match event {
                Event::Start(Tag::Image { .. }) => image_depth += 1,
                Event::End(TagEnd::Image) => image_depth -= 1,
                _ => {}
            }
            if image_depth != 0 {
                continue;
            }
            match &event {
                Event::Start(Tag::Heading { level, .. }) => {
                    let level = crate::heading_level_number(*level);
                    heading_stack.retain(|(previous, _)| *previous < level);
                    heading = Some(projection.sections.len());
                    projection.sections.push(String::new());
                    pending_heading = Some((level, projection.display.len()));
                    pending_space = true;
                }
                Event::End(TagEnd::Heading(_)) => {
                    if let Some((level, start)) = pending_heading.take() {
                        let title =
                            collapse(&projection.display[start..].iter().collect::<String>());
                        heading_stack.push((level, title));
                        if let Some(index) = heading {
                            projection.sections[index] = heading_stack
                                .iter()
                                .map(|(_, title)| title.as_str())
                                .collect::<Vec<_>>()
                                .join(" > ");
                        }
                    }
                    pending_space = true;
                }
                Event::Start(
                    Tag::Paragraph
                    | Tag::BlockQuote(_)
                    | Tag::CodeBlock(_)
                    | Tag::Item
                    | Tag::TableCell
                    | Tag::FootnoteDefinition(_),
                )
                | Event::End(
                    TagEnd::Paragraph
                    | TagEnd::BlockQuote(_)
                    | TagEnd::CodeBlock
                    | TagEnd::Item
                    | TagEnd::TableCell
                    | TagEnd::FootnoteDefinition,
                )
                | Event::SoftBreak
                | Event::HardBreak
                | Event::Rule => pending_space = true,
                _ => {}
            }
            let mut line = 1 + newlines.partition_point(|position| *position < span.start);
            let last_line = 1 + newlines.partition_point(|position| *position < span.end);
            // Inline code renders source line endings as spaces. Recover those
            // line boundaries before pairing its rendered characters with source.
            let inline_code = matches!(event, Event::Code(_));
            if let Event::Text(text) | Event::Code(text) = crate::sanitize_html_event(event) {
                let raw_code = inline_code.then(|| {
                    let raw = &prepared[span];
                    let ticks = raw.bytes().take_while(|byte| *byte == b'`').count();
                    raw[ticks..raw.len() - ticks]
                        .replace("\r\n", "\n")
                        .replace('\r', "\n")
                });
                let mut raw_chars = raw_code.as_deref().map(str::chars);
                if let Some(raw) = &mut raw_chars {
                    if raw.clone().count() == text.chars().count() + 2 {
                        line += usize::from(raw.next() == Some('\n'));
                    }
                }
                let mut chars = text.chars().peekable();
                while let Some(ch) = chars.next() {
                    if ch.is_whitespace() {
                        pending_space = true;
                    } else {
                        if pending_space && !projection.display.is_empty() {
                            projection.display.push(' ');
                        }
                        pending_space = false;
                        projection.points.push(Point {
                            display: projection.display.len(),
                            line,
                            heading,
                        });
                        projection.display.push(ch);
                        projection.scalars.push(ch);
                    }
                    let new_line = raw_chars.as_mut().map_or_else(
                        || ch == '\n' || (ch == '\r' && chars.peek() != Some(&'\n')),
                        |raw| raw.next() == Some('\n'),
                    );
                    line = last_line.min(line + usize::from(new_line));
                }
            }
        }
        projection
    }

    pub fn text(&self) -> String {
        self.display.iter().collect()
    }

    pub fn quote(&self, range: Range<usize>) -> Option<String> {
        let (start, end) = self.display_bounds(range)?;
        Some(self.display[start..end].iter().collect())
    }

    fn display_bounds(&self, range: Range<usize>) -> Option<(usize, usize)> {
        if range.is_empty() {
            return None;
        }
        Some((
            self.points.get(range.start)?.display,
            self.points.get(range.end - 1)?.display + 1,
        ))
    }

    pub(super) fn matches(&self, range: Range<usize>, token: &[char]) -> bool {
        !token.is_empty() && self.scalars.get(range) == Some(token)
    }

    pub(super) fn lines(&self, range: Range<usize>) -> (usize, usize) {
        (
            self.points[range.start].line,
            self.points[range.end - 1].line,
        )
    }

    pub(super) fn section(&self, start: usize) -> &str {
        self.points[start]
            .heading
            .map_or("", |index| self.sections[index].as_str())
    }

    pub(super) fn context(&self, range: Range<usize>) -> (String, String) {
        let (start, end) = self.display_bounds(range).expect("valid selection");
        (
            self.display[start.saturating_sub(32)..start]
                .iter()
                .collect(),
            self.display[end..self.display.len().min(end + 32)]
                .iter()
                .collect(),
        )
    }

    pub(super) fn locate(&self, record: &super::Annotation) -> Option<Range<usize>> {
        let token = normalized(record.quote.as_deref()?);
        if token.is_empty() || token.len() > self.scalars.len() {
            return None;
        }
        if record.metadata.get("source_revision") == Some(&self.revision) {
            if let Some(start) = record
                .metadata
                .get("start")
                .and_then(|value| value.parse::<usize>().ok())
            {
                let range = start..start.saturating_add(token.len());
                if self.matches(range.clone(), &token) {
                    return Some(range);
                }
            }
        }
        let before = normalized(record.metadata.get("before").map_or("", String::as_str));
        let after = normalized(record.metadata.get("after").map_or("", String::as_str));
        let mut best = None;
        let mut best_score = None;
        for (start, candidate) in self.scalars.windows(token.len()).enumerate() {
            if candidate != token {
                continue;
            }
            let end = start + token.len();
            let context = agreement(before.iter().rev(), self.scalars[..start].iter().rev())
                + agreement(after.iter(), self.scalars[end..].iter());
            let section = record
                .metadata
                .get("section")
                .is_some_and(|value| value == self.section(start));
            let distance = record
                .lines
                .map_or(0, |(line, _)| line.abs_diff(self.points[start].line));
            let score = (context, section, std::cmp::Reverse(distance));
            match best_score.map(|previous| score.cmp(&previous)) {
                None | Some(std::cmp::Ordering::Greater) => {
                    best = Some(start..end);
                    best_score = Some(score);
                }
                Some(std::cmp::Ordering::Equal) => best = None,
                Some(std::cmp::Ordering::Less) => {}
            }
        }
        best
    }
}

fn agreement<'a>(
    saved: impl Iterator<Item = &'a char>,
    actual: impl Iterator<Item = &'a char>,
) -> usize {
    saved.zip(actual).take_while(|(a, b)| a == b).count()
}
