use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use markview::{AppModel, GuiPreferences};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GuiCli {
    pub(crate) inputs: Vec<PathBuf>,
    pub(crate) help: bool,
}

impl GuiCli {
    pub(crate) fn parse<I, S>(args: I) -> Result<Self, String>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut inputs = Vec::new();
        let mut help = false;

        for arg in args.into_iter().map(Into::into) {
            match arg.as_str() {
                "-h" | "--help" => help = true,
                _ if arg.starts_with('-') => return Err(format!("unknown argument: {arg}")),
                _ => inputs.push(PathBuf::from(arg)),
            }
        }

        Ok(Self { inputs, help })
    }
}

pub(crate) fn help() -> &'static str {
    "Usage: markview-gui [FILE]...\n\nOpens Markdown files as rendered tabs in a native WebKit window.\n\nOptions:\n  -h, --help  Show this help"
}

pub(crate) fn normalize_path(path: PathBuf) -> PathBuf {
    path.canonicalize().unwrap_or(path)
}

pub(crate) fn is_markdown_path(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "md" | "markdown" | "mdown"
            )
        })
}

pub(crate) fn follow_document_link(
    model: &mut AppModel,
    tab_id: u64,
    href: &str,
    confirm_external: impl FnOnce(&str) -> bool,
    open_external: impl FnOnce(&str) -> io::Result<()>,
) -> Result<(), String> {
    let tab = model
        .tabs()
        .iter()
        .find(|tab| tab.id() == tab_id)
        .ok_or("The source tab is no longer open")?;
    let href = href.trim();
    let destination = match url::Url::parse(href) {
        Ok(url) => url,
        Err(url::ParseError::RelativeUrlWithoutBase) => {
            let path = tab
                .path()
                .ok_or("Save this document before opening relative links")?;
            url::Url::from_file_path(normalize_path(path.to_owned()))
                .map_err(|_| "The document path cannot be used to resolve links")?
                .join(href)
                .map_err(|error| error.to_string())?
        }
        Err(error) => return Err(error.to_string()),
    };
    if matches!(destination.scheme(), "http" | "https") {
        if confirm_external(destination.as_str()) {
            open_external(destination.as_str()).map_err(|error| error.to_string())?;
        }
        return Ok(());
    }
    let path = destination
        .to_file_path()
        .map_err(|_| "Unsupported document link")?;
    if !is_markdown_path(&path) {
        return Err("Only local Markdown documents can be opened in a tab".to_owned());
    }
    let source =
        fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    model.open_file(normalize_path(path), source);
    Ok(())
}

pub(crate) fn restore_files(preferences: &GuiPreferences) -> AppModel {
    let mut model = AppModel::new();
    for path in &preferences.last_open_files {
        if let Ok(source) = fs::read_to_string(path) {
            model.open_file(normalize_path(path.clone()), source);
        }
    }
    if let Some(active_file) = &preferences.active_file {
        let active_file = normalize_path(active_file.clone());
        if let Some(tab) = model
            .tabs()
            .iter()
            .find(|tab| tab.path() == Some(active_file.as_path()))
        {
            model.select(tab.id());
        }
    }
    model
}

pub(crate) fn preferences_path() -> PathBuf {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return PathBuf::from(".markview-preferences");
    };

    #[cfg(target_os = "macos")]
    {
        home.join("Library")
            .join("Application Support")
            .join("markview")
            .join("preferences.conf")
    }

    #[cfg(not(target_os = "macos"))]
    {
        home.join(".config")
            .join("markview")
            .join("preferences.conf")
    }
}

pub(crate) fn load_preferences(path: &Path) -> GuiPreferences {
    fs::read_to_string(path)
        .map(|source| GuiPreferences::parse(&source))
        .unwrap_or_default()
}

pub(crate) fn save_preferences(path: &Path, preferences: &GuiPreferences) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, preferences.serialize())
}

pub(crate) fn persist_open_state(
    path: &Path,
    preferences: &mut GuiPreferences,
    model: &AppModel,
    window: Option<&tao::window::Window>,
) {
    if let Some(window) = window {
        update_window_size(preferences, window);
    }
    preferences.record_open_files(
        model.watched_paths(),
        model.active_tab().and_then(|tab| tab.path()),
    );
    if let Err(error) = save_preferences(path, preferences) {
        eprintln!("markview-gui: failed to save preferences: {error}");
    }
}

pub(crate) fn update_window_size(preferences: &mut GuiPreferences, window: &tao::window::Window) {
    let size = window.inner_size().to_logical::<u32>(window.scale_factor());
    preferences.window_width = size.width;
    preferences.window_height = size.height;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_link_cancel_leaves_external_destination_unopened() {
        let mut model = AppModel::new();
        let id = model.open_untitled("source", "# Source".to_owned());
        let mut prompted = false;
        follow_document_link(
            &mut model,
            id,
            "https://example.com/",
            |url| {
                assert_eq!(url, "https://example.com/");
                prompted = true;
                false
            },
            |_| panic!("Cancel must leave the destination unopened"),
        )
        .unwrap();
        assert!(prompted);
        assert_eq!(model.active_tab_id(), Some(id));
        assert_eq!(model.tabs().len(), 1);
    }

    #[test]
    fn document_link_confirmation_opens_the_external_destination() {
        let mut model = AppModel::new();
        let id = model.open_untitled("source", "# Source".to_owned());
        let confirmed = std::cell::Cell::new(false);
        let mut opened = None;
        follow_document_link(
            &mut model,
            id,
            "https://example.com/guide?mode=read#intro",
            |_| {
                confirmed.set(true);
                true
            },
            |url| {
                assert!(confirmed.get(), "confirmation must precede opening");
                opened = Some(url.to_owned());
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(
            opened.as_deref(),
            Some("https://example.com/guide?mode=read#intro")
        );
        assert_eq!(model.active_tab_id(), Some(id));
    }

    #[test]
    fn document_link_decodes_relative_paths_and_preserves_unsaved_source() {
        let directory = tempfile::tempdir().unwrap();
        let docs = directory.path().join("docs");
        fs::create_dir(&docs).unwrap();
        let guide = directory.path().join("guide notes.MARKDOWN");
        fs::write(&guide, "# Guide notes").unwrap();
        let mut model = AppModel::new();
        let id = model.open_file(docs.join("source.md"), "# Source".to_owned());
        model.toggle_editing(id);
        model.update_source(id, "# Unsaved source".to_owned());
        follow_document_link(
            &mut model,
            id,
            "../guide%20notes.MARKDOWN#intro",
            |_| false,
            |_| panic!("local link"),
        )
        .unwrap();
        assert_eq!(
            model.active_tab().unwrap().path(),
            Some(guide.canonicalize().unwrap().as_path())
        );
        let source = &model.tabs()[0];
        assert!(source.is_dirty());
        assert!(source.is_editing());
        assert_eq!(source.document().source(), "# Unsaved source");
    }

    #[test]
    fn document_link_failures_leave_the_current_document_intact() {
        let directory = tempfile::tempdir().unwrap();
        let mut model = AppModel::new();
        let id = model.open_file(directory.path().join("source.md"), "# Source".to_owned());
        for href in [
            "missing.md",
            "notes.txt",
            "javascript:alert(1)",
            "markview://app/guide.md",
        ] {
            assert!(follow_document_link(
                &mut model,
                id,
                href,
                |_| panic!("unexpected prompt"),
                |_| panic!("unexpected external launch")
            )
            .is_err());
            assert_eq!(model.tabs().len(), 1);
            assert_eq!(model.active_tab_id(), Some(id));
            assert_eq!(model.active_tab().unwrap().document().source(), "# Source");
        }
        let untitled = model.open_untitled("untitled", "# Untitled".to_owned());
        assert!(
            follow_document_link(&mut model, untitled, "guide.md", |_| false, |_| Ok(()))
                .unwrap_err()
                .contains("Save this document")
        );
    }

    #[test]
    fn document_link_opens_relative_markdown_beside_its_source_in_a_new_tab() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.md");
        let guide = directory.path().join("guide.md");
        fs::write(&guide, "# Local guide opened").unwrap();
        let mut model = AppModel::new();
        let id = model.open_file(source, "[Local guide](guide.md)".to_owned());
        model.open_untitled("other active tab", "# Other".to_owned());

        follow_document_link(
            &mut model,
            id,
            "guide.md",
            |_| panic!("local link must not prompt"),
            |_| panic!("local link must not launch a browser"),
        )
        .unwrap();

        assert_eq!(model.tabs().len(), 3);
        let active = model.active_tab().unwrap();
        assert_eq!(active.path(), Some(guide.canonicalize().unwrap().as_path()));
        assert_eq!(active.document().source(), "# Local guide opened");
    }

    #[test]
    fn parses_multiple_input_files() {
        let cli = GuiCli::parse(["README.md", "guide.md"]).expect("parse");

        assert_eq!(
            cli.inputs,
            vec![PathBuf::from("README.md"), PathBuf::from("guide.md")]
        );
        assert!(!cli.help);
    }

    #[test]
    fn rejects_unknown_gui_flags() {
        let error = GuiCli::parse(["--bogus"]).expect_err("unknown flag");

        assert_eq!(error, "unknown argument: --bogus");
    }

    #[test]
    fn saves_and_loads_preferences_file() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("preferences.conf");
        let preferences = GuiPreferences {
            theme: markview::ThemePreference::Light,
            sidebar_visible: false,
            auto_refresh: false,
            window_width: 1110,
            window_height: 720,
            recent_files: vec![PathBuf::from("/tmp/readme.md")],
            last_open_files: vec![PathBuf::from("/tmp/readme.md")],
            active_file: Some(PathBuf::from("/tmp/readme.md")),
        };

        save_preferences(&path, &preferences).expect("save preferences");

        assert_eq!(load_preferences(&path), preferences);
    }

    #[test]
    fn persists_open_state_without_window() {
        let directory = tempfile::tempdir().expect("temp dir");
        let first = directory.path().join("first.md");
        let second = directory.path().join("second.md");
        fs::write(&first, "# First").expect("write first");
        fs::write(&second, "# Second").expect("write second");
        let mut model = AppModel::new();
        model.open_file(first.clone(), "# First".to_owned());
        model.open_file(second.clone(), "# Second".to_owned());
        let path = directory.path().join("preferences.conf");
        let mut preferences = GuiPreferences::default();

        persist_open_state(&path, &mut preferences, &model, None);

        let loaded = load_preferences(&path);
        assert_eq!(loaded.last_open_files, vec![first.clone(), second.clone()]);
        assert_eq!(loaded.recent_files, vec![second.clone(), first.clone()]);
        assert_eq!(loaded.active_file, Some(second));
    }

    #[test]
    fn restores_open_files_from_preferences() {
        let directory = tempfile::tempdir().expect("temp dir");
        let first = directory.path().join("first.md");
        let second = directory.path().join("second.md");
        fs::write(&first, "# First").expect("write first");
        fs::write(&second, "# Second").expect("write second");
        let preferences = GuiPreferences {
            last_open_files: vec![first.clone(), second.clone()],
            active_file: Some(first.clone()),
            ..GuiPreferences::default()
        };

        let model = restore_files(&preferences);

        let first = normalize_path(first);
        assert_eq!(model.tabs().len(), 2);
        assert_eq!(
            model.active_tab().and_then(|tab| tab.path()),
            Some(first.as_path())
        );
        assert_eq!(
            model
                .tabs()
                .iter()
                .map(|tab| tab.document().source())
                .collect::<Vec<_>>(),
            vec!["# First", "# Second"]
        );
    }
}
