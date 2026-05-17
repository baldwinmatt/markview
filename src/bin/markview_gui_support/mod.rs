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

pub(crate) fn save_runtime_preferences(
    path: &Path,
    preferences: &mut GuiPreferences,
    model: &AppModel,
    window: Option<&tao::window::Window>,
) {
    persist_open_state(path, preferences, model, window);
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
