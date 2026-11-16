//! Projection-versus-DOM parity.
//!
//! `dom_scalars` stands in for the browser: it parses rendered HTML the way
//! an HTML parser builds text nodes, then walks them like `markviewReviewText`
//! in review.js. The scalars it yields must equal the Rust `TextProjection`,
//! because annotation offsets index into both.

use super::projection::normalized;
use super::TextProjection;

/// Windows-1252 remapping that HTML applies to numeric references in the C1
/// range. Code points without an entry stay as they are.
const WINDOWS_1252: [(u32, char); 27] = [
    (0x80, '\u{20ac}'),
    (0x82, '\u{201a}'),
    (0x83, '\u{0192}'),
    (0x84, '\u{201e}'),
    (0x85, '\u{2026}'),
    (0x86, '\u{2020}'),
    (0x87, '\u{2021}'),
    (0x88, '\u{02c6}'),
    (0x89, '\u{2030}'),
    (0x8a, '\u{0160}'),
    (0x8b, '\u{2039}'),
    (0x8c, '\u{0152}'),
    (0x8e, '\u{017d}'),
    (0x91, '\u{2018}'),
    (0x92, '\u{2019}'),
    (0x93, '\u{201c}'),
    (0x94, '\u{201d}'),
    (0x95, '\u{2022}'),
    (0x96, '\u{2013}'),
    (0x97, '\u{2014}'),
    (0x98, '\u{02dc}'),
    (0x99, '\u{2122}'),
    (0x9a, '\u{0161}'),
    (0x9b, '\u{203a}'),
    (0x9c, '\u{0153}'),
    (0x9e, '\u{017e}'),
    (0x9f, '\u{0178}'),
];

/// Decodes one character reference at the start of `input` (which begins with
/// `&`), returning the character and the bytes consumed.
fn character_reference(input: &str) -> Option<(char, usize)> {
    let end = input.find(';')?;
    let name = &input[1..end];
    let ch = if let Some(number) = name.strip_prefix('#') {
        let code = match number.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16).ok()?,
            None => number.parse().ok()?,
        };
        WINDOWS_1252
            .iter()
            .find(|(c1, _)| *c1 == code)
            .map(|(_, mapped)| *mapped)
            .or_else(|| char::from_u32(code).filter(|ch| *ch != '\0'))
            .unwrap_or('\u{fffd}')
    } else {
        match name {
            "amp" => '&',
            "lt" => '<',
            "gt" => '>',
            "quot" => '"',
            "apos" => '\'',
            _ => return None,
        }
    };
    Some((ch, end + 1))
}

/// The text of one text node, as the HTML parser would build it in a body.
fn text_node(raw: &str) -> String {
    let mut text = String::new();
    let mut rest = raw;
    while let Some(ch) = rest.chars().next() {
        if let Some((decoded, consumed)) = (ch == '&').then(|| character_reference(rest)).flatten()
        {
            text.push(decoded);
            rest = &rest[consumed..];
            continue;
        }
        // "In body", a literal U+0000 character token is ignored.
        if ch != '\0' {
            text.push(ch);
        }
        rest = &rest[ch.len_utf8()..];
    }
    text
}

fn attribute<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let value = tag.split_once(&format!(" {name}=\""))?.1;
    value.split_once('"').map(|(value, _)| value)
}

/// Non-whitespace scalars of the rendered document root, in DOM order,
/// skipping the same elements as `markviewReviewExcluded` in review.js.
pub(super) fn dom_scalars(html: &str) -> Vec<char> {
    let main = html.find("<main").expect("document root");
    let mut rest = &html[main..];
    let mut scalars = Vec::new();
    // Open elements, each flagged when it or an ancestor is excluded.
    let mut open: Vec<(&str, bool)> = Vec::new();
    while !rest.is_empty() {
        let Some(tag) = rest.strip_prefix('<') else {
            let end = rest.find('<').unwrap_or(rest.len());
            if !open.last().is_some_and(|(_, excluded)| *excluded) {
                scalars.extend(
                    text_node(&rest[..end])
                        .chars()
                        .filter(|ch| !ch.is_whitespace()),
                );
            }
            rest = &rest[end..];
            continue;
        };
        let end = tag.find('>').expect("closed tag");
        let (tag, after) = (&tag[..end], &tag[end + 1..]);
        rest = after;
        if let Some(name) = tag.strip_prefix('/') {
            while let Some((opened, _)) = open.pop() {
                if opened == name {
                    break;
                }
            }
            if name == "main" {
                break;
            }
            continue;
        }
        let name = tag.split([' ', '/']).next().expect("tag name");
        let class = attribute(tag, "class").unwrap_or("");
        let excluded = open.last().is_some_and(|(_, excluded)| *excluded)
            || matches!(name, "img" | "input")
            || class
                .split(' ')
                .any(|class| matches!(class, "footnote-reference" | "footnote-definition-label"));
        if !matches!(name, "img" | "input" | "br" | "hr") {
            open.push((name, excluded));
        }
    }
    scalars
}

fn assert_parity(name: &str, source: &str) {
    let html = crate::render_html(source);
    let dom: String = dom_scalars(&html).into_iter().collect();
    let projection: String = normalized(&TextProjection::new(source).text())
        .into_iter()
        .collect();
    assert_eq!(
        dom.escape_unicode().to_string(),
        projection.escape_unicode().to_string(),
        "{name}: DOM (left) and projection (right) differ"
    );
    assert!(!dom.is_empty(), "{name}: empty document");
}

/// The rendered corpus in `tests/fixtures/parity`: one Markdown file per
/// area of the renderer, each a document a reviewer could annotate.
fn corpus() -> Vec<(String, String)> {
    let mut files: Vec<_> = std::fs::read_dir("tests/fixtures/parity")
        .expect("parity corpus")
        .map(|entry| entry.expect("corpus entry").path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "md"))
        .collect();
    files.sort();
    files
        .into_iter()
        .map(|path| {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            (name, std::fs::read_to_string(&path).expect("corpus file"))
        })
        .collect()
}

#[test]
fn projection_matches_the_dom_across_the_rendered_corpus() {
    let corpus = corpus();
    let names: Vec<_> = corpus.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(
        names,
        [
            "blocks.md",
            "code-and-diff.md",
            "footnotes.md",
            "html-and-mojibake.md",
            "inline.md",
            "unicode.md"
        ]
    );
    for (name, source) in &corpus {
        assert_parity(name, source);
        assert_parity(&format!("{name} with CRLF"), &source.replace('\n', "\r\n"));
        // Serve rewrites link and image destinations. That must leave the
        // text, and so every annotation offset, exactly as it was.
        let rewritten = crate::HtmlRenderer
            .render_with_destinations(&crate::MarkdownDocument::new(source.as_str()), |_, _| {
                Some("/rewritten/route.md#fragment".to_owned())
            });
        assert!(rewritten.contains("/rewritten/route.md#fragment") || !source.contains("]("));
        assert_eq!(
            dom_scalars(&rewritten),
            dom_scalars(&crate::render_html(source)),
            "{name}: rewriting destinations changed the text"
        );
    }
}

#[test]
fn corpus_selections_map_to_the_same_text_and_source_line() {
    // Each quote appears once, verbatim on one source line, so the expected
    // line comes straight from the file and not from the projection.
    for (file, quote) in [
        ("inline.md", "strikethrough"),
        ("inline.md", "sits between words"),
        ("blocks.md", "another nested bullet"),
        ("blocks.md", "an open task with"),
        ("blocks.md", "with two lines"),
        ("code-and-diff.md", "+added \"line\" with café"),
        ("code-and-diff.md", "let message ="),
        ("code-and-diff.md", "+still a diff"),
        ("footnotes.md", "Its second paragraph is indented."),
        ("footnotes.md", "Text after the definitions."),
        ("html-and-mojibake.md", "literal &amp; text inside a block"),
        (
            "html-and-mojibake.md",
            "<script>alert(\"never runs\")</script>",
        ),
        ("unicode.md", "日本語のテキストです。"),
        ("unicode.md", "👨\u{200d}👩\u{200d}👧\u{200d}👦"),
        ("unicode.md", "コード 😀 inside a fence"),
    ] {
        let source = std::fs::read_to_string(format!("tests/fixtures/parity/{file}")).unwrap();
        let lines: Vec<_> = source
            .lines()
            .enumerate()
            .filter(|(_, line)| line.contains(quote))
            .map(|(index, _)| index + 1)
            .collect();
        assert_eq!(
            lines.len(),
            1,
            "{file}: {quote:?} must be on exactly one line"
        );

        let projection = TextProjection::new(&source);
        let token = normalized(quote);
        let text = normalized(&projection.text());
        let starts: Vec<_> = text
            .windows(token.len())
            .enumerate()
            .filter(|(_, window)| *window == token)
            .map(|(start, _)| start)
            .collect();
        assert_eq!(starts.len(), 1, "{file}: {quote:?} must project once");
        let range = starts[0]..starts[0] + token.len();
        assert_eq!(
            projection.lines(range.clone()),
            (lines[0], lines[0]),
            "{file}: {quote:?}"
        );
        assert_eq!(projection.quote(range.clone()).as_deref(), Some(quote));
        // The same offsets select the same characters in the rendered DOM.
        let dom = dom_scalars(&crate::render_html(&source));
        assert_eq!(dom[range], token[..], "{file}: {quote:?}");
    }
}

#[test]
fn dom_walker_decodes_like_an_html_parser() {
    // The stand-in must reproduce the browser behaviour the parity test is
    // guarding against, or agreement with it would mean nothing.
    let scalars = |body: &str| -> String {
        dom_scalars(&format!("<main>{body}</main>"))
            .into_iter()
            .collect()
    };
    assert_eq!(
        scalars("a&#133;b&#128;c&#x9d;d"),
        "a\u{2026}b\u{20ac}c\u{9d}d"
    );
    assert_eq!(scalars("a\u{85}b\u{80}c"), "ab\u{80}c");
    assert_eq!(scalars("a\0b&#0;c"), "ab\u{fffd}c");
    assert_eq!(
        scalars("&lt;p&gt; &amp;amp; &#128512;&#8232;x"),
        "<p>&amp;😀x"
    );
    assert_eq!(
        scalars(concat!(
            "<p>one<img alt=\"skipped\" src=\"x\"> <code>two</code></p>",
            "<sup class=\"footnote-reference\"><a href=\"#n\">1</a></sup>",
            "<input type=\"checkbox\" disabled=\"\"><br>three"
        )),
        "onetwothree"
    );
}

#[test]
fn projection_matches_the_dom_for_controls_and_highlighted_code() {
    let controls = "c1 \u{80}\u{81}\u{85}\u{9f} nul \0 nel\u{85}next sep\u{2028}para 😀";
    let source = [
        format!("# Controls {controls}\n\n"),
        format!("Paragraph {controls} and `inline {controls}`.\n\n"),
        format!("```\nplain {controls}\n```\n\n"),
        format!("```diff\n-old {controls}\n+new {controls}\n context\n```\n\n"),
        format!("```rust\nfn main() {{ let s = \"{controls}\"; }} // {controls}\n```\n\n"),
        format!("- [x] task {controls}\n\n| a | b |\n| - | - |\n| {controls} | z |\n"),
    ]
    .concat();
    assert_parity("controls", &source);
}
