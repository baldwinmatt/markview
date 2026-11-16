use std::fmt::Write as _;
use std::io;

use markview::annotations::{json_string, TextProjection};

use super::{title_for_source, validated_served_path, ServeConfig, ServedDocument};

pub const PREFIX: &str = "/_markview/api/v1/";
pub const TOKEN_ENV: &str = "MARKVIEW_API_TOKEN";

/// The variable's value, with an empty one counting as unset.
fn token_variable() -> Option<std::ffi::OsString> {
    std::env::var_os(TOKEN_ENV).filter(|value| !value.is_empty())
}

/// Whether the environment names a token at all, valid or not.
pub fn token_variable_set() -> bool {
    token_variable().is_some()
}

pub fn configured_token() -> io::Result<Option<String>> {
    let Some(value) = token_variable() else {
        return Ok(None);
    };
    value
        .into_string()
        .ok()
        .filter(|value| {
            value.len() == 64
                && value
                    .bytes()
                    .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        })
        .map(Some)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "MARKVIEW_API_TOKEN must contain 64 lowercase hex digits",
            )
        })
}

pub fn catalog(config: &ServeConfig, blocking: bool) -> String {
    let default = config
        .default_document()
        .map_or_else(|| "null".to_owned(), |doc| json_string(&doc.route_path));
    let can_annotate = config.session.api_token.is_some();
    let can_finish = can_annotate && blocking;
    let mut body = format!(
        r#"{{"default_document":{default},"can_annotate":{can_annotate},"can_finish":{can_finish},"documents":["#
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

pub fn source(config: &ServeConfig, doc: &ServedDocument) -> io::Result<String> {
    let path = validated_served_path(&config.root, &doc.source_path)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "document is unavailable"))?;
    #[cfg(unix)]
    let markdown = markview::annotations::read_canonical_source(&path)?;
    #[cfg(not(unix))]
    let markdown = std::fs::read_to_string(&path)?;
    let projection = TextProjection::new(&markdown);
    Ok(format!(
        r#"{{"route":{},"title":{},"source_revision":{},"markdown":{},"projected_text":{}}}"#,
        json_string(&doc.route_path),
        json_string(&title_for_source(&path, &markdown)),
        json_string(&projection.revision),
        json_string(&markdown),
        json_string(&projection.text()),
    ))
}
