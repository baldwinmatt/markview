use std::collections::HashSet;
use std::fs;
use std::io::{self, IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::process::ExitCode;

#[cfg(unix)]
use std::os::fd::{AsRawFd, RawFd};

use markview::{
    app_view_with_preferences, AppModel, AppView, FrontendRenderer, GuiPreferences, HtmlRenderer,
};
use notify::{Config, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tao::window::WindowBuilder;
use wry::{http::Request, WebView, WebViewBuilder};

#[path = "markview_gui_support/mod.rs"]
mod gui_support;

use gui_support::{
    help, load_preferences, normalize_path, persist_open_state, preferences_path, restore_files,
    update_window_size, GuiCli,
};

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

    let preferences_path = preferences_path();
    let mut preferences = load_preferences(&preferences_path);
    let mut model = initial_model(&args.inputs, &preferences)?;
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let mut watcher = FileWatcher::new(proxy.clone())?;

    install_application_menu(proxy.clone());
    watcher.sync(model.watched_directories())?;

    let window = WindowBuilder::new()
        .with_title(window_title(&model))
        .with_inner_size(tao::dpi::LogicalSize::new(
            preferences.window_width as f64,
            preferences.window_height as f64,
        ))
        .build(&event_loop)?;

    let webview = build_webview(
        &window,
        proxy.clone(),
        &app_view_with_preferences(&model, preferences.clone()),
    )?;

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;

        match event {
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } if !any_tab_dirty(&model)
                || confirm_discard_changes(
                    &window,
                    "You have unsaved changes. Discard them and quit?",
                ) =>
            {
                persist_open_state(&preferences_path, &mut preferences, &model, Some(&window));
                *control_flow = ControlFlow::Exit;
            }
            Event::WindowEvent {
                event: WindowEvent::Resized(_),
                ..
            } => {
                update_window_size(&mut preferences, &window);
            }
            Event::UserEvent(UserEvent::OpenRequested) => {
                if let Err(error) = open_document(&window, &mut model, &mut watcher) {
                    eprintln!("markview-gui: {error}");
                }
                persist_open_state(&preferences_path, &mut preferences, &model, Some(&window));
                sync_view(&webview, &model, &preferences);
                window.set_title(&window_title(&model));
            }
            Event::UserEvent(UserEvent::RefreshRequested) => {
                let confirmed = !model.active_tab().is_some_and(markview::DocumentTab::is_dirty)
                    || confirm_discard_changes(
                        &window,
                        "This tab has unsaved changes. Discard them and refresh?",
                    );
                if confirmed {
                    if let Err(error) = model.force_refresh_active(|path| fs::read_to_string(path)) {
                        eprintln!("markview-gui: {error}");
                    }
                    sync_persisted_view(&preferences_path, &mut preferences, &model, &webview, &window);
                }
            }
            Event::UserEvent(UserEvent::ToggleSidebar) => {
                preferences.sidebar_visible = !preferences.sidebar_visible;
                persist_open_state(&preferences_path, &mut preferences, &model, Some(&window));
                sync_view(&webview, &model, &preferences);
            }
            Event::UserEvent(UserEvent::ToggleAutoRefresh) => {
                preferences.auto_refresh = !preferences.auto_refresh;
                persist_open_state(&preferences_path, &mut preferences, &model, Some(&window));
                sync_view(&webview, &model, &preferences);
            }
            Event::UserEvent(UserEvent::CycleTheme) => {
                preferences.theme = preferences.theme.cycle();
                persist_open_state(&preferences_path, &mut preferences, &model, Some(&window));
                sync_view(&webview, &model, &preferences);
            }
            Event::UserEvent(UserEvent::PrintRequested) => {
                if let Err(error) = webview.print() {
                    eprintln!("markview-gui: {error}");
                }
            }
            Event::UserEvent(UserEvent::ExportHtmlRequested) => {
                if let Err(error) = export_html(&window, &model) {
                    eprintln!("markview-gui: {error}");
                }
            }
            Event::UserEvent(UserEvent::FindRequested) => {
                if let Err(error) =
                    webview.evaluate_script("document.getElementById('find-input')?.focus();")
                {
                    eprintln!("markview-gui: failed to focus find: {error}");
                }
            }
            Event::UserEvent(UserEvent::QuitRequested)
                if !any_tab_dirty(&model)
                    || confirm_discard_changes(
                        &window,
                        "You have unsaved changes. Discard them and quit?",
                    ) =>
            {
                persist_open_state(&preferences_path, &mut preferences, &model, Some(&window));
                *control_flow = ControlFlow::Exit;
            }
            Event::UserEvent(UserEvent::OpenExternal(url)) => {
                if let Err(error) = open_external_url(&url) {
                    eprintln!("markview-gui: failed to open link: {error}");
                }
            }
            Event::UserEvent(UserEvent::DroppedFiles(paths)) => {
                if let Err(error) = open_dropped_documents(paths, &mut model, &mut watcher) {
                    eprintln!("markview-gui: {error}");
                }
                persist_open_state(&preferences_path, &mut preferences, &model, Some(&window));
                sync_view(&webview, &model, &preferences);
                window.set_title(&window_title(&model));
            }
            Event::Opened { urls } => {
                let paths = urls
                    .iter()
                    .filter_map(opened_url_file_path)
                    .collect::<Vec<_>>();
                if let Err(error) = open_paths(paths, &mut model, &mut watcher) {
                    eprintln!("markview-gui: {error}");
                }
                persist_open_state(&preferences_path, &mut preferences, &model, Some(&window));
                sync_view(&webview, &model, &preferences);
                window.set_title(&window_title(&model));
            }
            Event::UserEvent(UserEvent::OpenRecent(path)) => {
                if let Err(error) = open_path(path, &mut model, &mut watcher) {
                    eprintln!("markview-gui: {error}");
                }
                persist_open_state(&preferences_path, &mut preferences, &model, Some(&window));
                sync_view(&webview, &model, &preferences);
                window.set_title(&window_title(&model));
            }
            Event::UserEvent(UserEvent::SelectTab(id)) => {
                model.select(id);
                persist_open_state(&preferences_path, &mut preferences, &model, Some(&window));
                sync_view(&webview, &model, &preferences);
                window.set_title(&window_title(&model));
            }
            Event::UserEvent(UserEvent::CloseTab(id)) => {
                let message = model
                    .tabs()
                    .iter()
                    .find(|tab| tab.id() == id)
                    .map(|tab| {
                        format!(
                            "\"{}\" has unsaved changes. Discard them and close this tab?",
                            tab.title()
                        )
                    })
                    .unwrap_or_else(|| "This tab has unsaved changes. Discard them and close it?".to_owned());
                if confirm_if_dirty(&window, tab_dirty(&model, id), &message) {
                    model.close(id);
                    sync_after_tab_mutation(
                        &mut watcher,
                        &preferences_path,
                        &mut preferences,
                        &model,
                        &webview,
                        &window,
                    );
                }
            }
            Event::UserEvent(UserEvent::CloseActiveTab) => {
                if let Some(id) = model.active_tab_id() {
                    let message = model
                        .active_tab()
                        .map(|tab| {
                            format!(
                                "\"{}\" has unsaved changes. Discard them and close this tab?",
                                tab.title()
                            )
                        })
                        .unwrap_or_else(|| "This tab has unsaved changes. Discard them and close it?".to_owned());
                    if confirm_if_dirty(&window, tab_dirty(&model, id), &message) {
                        model.close(id);
                        sync_after_tab_mutation(
                            &mut watcher,
                            &preferences_path,
                            &mut preferences,
                            &model,
                            &webview,
                            &window,
                        );
                    }
                }
            }
            Event::UserEvent(UserEvent::CloseOtherTabs(id)) => {
                if confirm_if_dirty(
                    &window,
                    other_tabs_dirty(&model, id),
                    "Other open tabs have unsaved changes. Discard them and close those tabs?",
                ) {
                    model.close_others(id);
                    sync_after_tab_mutation(
                        &mut watcher,
                        &preferences_path,
                        &mut preferences,
                        &model,
                        &webview,
                        &window,
                    );
                }
            }
            Event::UserEvent(UserEvent::CloseTabsToLeft(id)) => {
                if confirm_if_dirty(
                    &window,
                    tabs_to_left_dirty(&model, id),
                    "Some of the tabs you're closing have unsaved changes. Discard them and close those tabs?",
                ) {
                    model.close_to_left(id);
                    sync_after_tab_mutation(
                        &mut watcher,
                        &preferences_path,
                        &mut preferences,
                        &model,
                        &webview,
                        &window,
                    );
                }
            }
            Event::UserEvent(UserEvent::CloseTabsToRight(id)) => {
                if confirm_if_dirty(
                    &window,
                    tabs_to_right_dirty(&model, id),
                    "Some of the tabs you're closing have unsaved changes. Discard them and close those tabs?",
                ) {
                    model.close_to_right(id);
                    sync_after_tab_mutation(
                        &mut watcher,
                        &preferences_path,
                        &mut preferences,
                        &model,
                        &webview,
                        &window,
                    );
                }
            }
            Event::UserEvent(UserEvent::ReloadTab(id)) => {
                let message = model
                    .tabs()
                    .iter()
                    .find(|tab| tab.id() == id)
                    .map(|tab| {
                        format!(
                            "\"{}\" has unsaved changes. Discard them and reload this tab?",
                            tab.title()
                        )
                    })
                    .unwrap_or_else(|| "This tab has unsaved changes. Discard them and reload it?".to_owned());
                if confirm_if_dirty(&window, tab_dirty(&model, id), &message) {
                    if let Err(error) = model.force_refresh(id, |path| fs::read_to_string(path)) {
                        eprintln!("markview-gui: {error}");
                    }
                    sync_persisted_view(&preferences_path, &mut preferences, &model, &webview, &window);
                }
            }
            Event::UserEvent(UserEvent::ExportTabHtml(id)) => {
                if let Err(error) = export_tab_html(&window, &model, id) {
                    eprintln!("markview-gui: {error}");
                }
            }
            Event::UserEvent(UserEvent::ToggleActiveEdit) => {
                if let Some(id) = model.active_tab_id() {
                    model.toggle_editing(id);
                    sync_view(&webview, &model, &preferences);
                }
            }
            Event::UserEvent(UserEvent::EditChanged(id, text)) => {
                model.update_source(id, text);
                sync_view(&webview, &model, &preferences);
                window.set_title(&window_title(&model));
            }
            Event::UserEvent(UserEvent::SaveRequested) => {
                match save_active(&window, &mut model) {
                    Ok(SaveOutcome::Saved { path_changed }) => {
                        if path_changed {
                            sync_watcher(&mut watcher, &model);
                        }
                    }
                    Ok(SaveOutcome::Cancelled) => {}
                    Err(error) => eprintln!("markview-gui: {error}"),
                }
                sync_persisted_view(&preferences_path, &mut preferences, &model, &webview, &window);
            }
            Event::UserEvent(UserEvent::FilesChanged(paths)) => {
                if preferences.auto_refresh {
                    if let Err(error) = model
                        .refresh_changed_paths(paths.iter().map(PathBuf::as_path), |path| {
                            fs::read_to_string(path)
                        })
                    {
                        eprintln!("markview-gui: {error}");
                    }
                } else {
                    model.mark_changed_paths_stale(paths.iter().map(PathBuf::as_path));
                }
                sync_view(&webview, &model, &preferences);
                window.set_title(&window_title(&model));
            }
            _ => {}
        }
    });
}

fn build_webview(
    window: &tao::window::Window,
    proxy: EventLoopProxy<UserEvent>,
    initial_view: &AppView,
) -> wry::Result<WebView> {
    let ipc_proxy = proxy.clone();
    let handler = move |request: Request<String>| {
        let body = request.body();
        let event = match body.as_str() {
            "open" => Some(UserEvent::OpenRequested),
            "refresh" => Some(UserEvent::RefreshRequested),
            "print" => Some(UserEvent::PrintRequested),
            "export-html" => Some(UserEvent::ExportHtmlRequested),
            "find" => Some(UserEvent::FindRequested),
            "quit" => Some(UserEvent::QuitRequested),
            "toggle-sidebar" => Some(UserEvent::ToggleSidebar),
            "toggle-auto-refresh" => Some(UserEvent::ToggleAutoRefresh),
            "cycle-theme" => Some(UserEvent::CycleTheme),
            "toggle-edit" => Some(UserEvent::ToggleActiveEdit),
            "save" => Some(UserEvent::SaveRequested),
            _ if body.starts_with("edit:") => {
                let rest = body.trim_start_matches("edit:");
                rest.split_once(':').and_then(|(id, text)| {
                    id.parse::<u64>()
                        .ok()
                        .map(|id| UserEvent::EditChanged(id, text.to_owned()))
                })
            }
            _ if body.starts_with("select:") => body
                .trim_start_matches("select:")
                .parse::<u64>()
                .ok()
                .map(UserEvent::SelectTab),
            _ if body.starts_with("close:") => body
                .trim_start_matches("close:")
                .parse::<u64>()
                .ok()
                .map(UserEvent::CloseTab),
            _ if body.starts_with("close-others:") => body
                .trim_start_matches("close-others:")
                .parse::<u64>()
                .ok()
                .map(UserEvent::CloseOtherTabs),
            _ if body.starts_with("close-left:") => body
                .trim_start_matches("close-left:")
                .parse::<u64>()
                .ok()
                .map(UserEvent::CloseTabsToLeft),
            _ if body.starts_with("close-right:") => body
                .trim_start_matches("close-right:")
                .parse::<u64>()
                .ok()
                .map(UserEvent::CloseTabsToRight),
            _ if body.starts_with("reload-tab:") => body
                .trim_start_matches("reload-tab:")
                .parse::<u64>()
                .ok()
                .map(UserEvent::ReloadTab),
            _ if body.starts_with("export-tab-html:") => body
                .trim_start_matches("export-tab-html:")
                .parse::<u64>()
                .ok()
                .map(UserEvent::ExportTabHtml),
            _ if body.starts_with("recent:") => Some(UserEvent::OpenRecent(PathBuf::from(
                body.trim_start_matches("recent:"),
            ))),
            _ => None,
        };

        if let Some(event) = event {
            let _ = ipc_proxy.send_event(event);
        }
    };

    let navigation_proxy = proxy.clone();
    let navigation_handler = move |url: String| {
        if is_external_url(&url) {
            let _ = navigation_proxy.send_event(UserEvent::OpenExternal(url));
            false
        } else {
            true
        }
    };

    let drop_proxy = proxy;
    let drag_drop_handler = move |event: wry::DragDropEvent| {
        if let wry::DragDropEvent::Drop { paths, .. } = event {
            let _ = drop_proxy.send_event(UserEvent::DroppedFiles(paths));
            true
        } else {
            false
        }
    };

    WebViewBuilder::new()
        .with_html(app_shell_html(initial_view))
        .with_ipc_handler(handler)
        .with_navigation_handler(navigation_handler)
        .with_drag_drop_handler(drag_drop_handler)
        .build(window)
}

fn initial_model(
    inputs: &[PathBuf],
    preferences: &GuiPreferences,
) -> Result<AppModel, Box<dyn std::error::Error>> {
    let mut model = AppModel::new();

    if inputs.is_empty() {
        let stdin = io::stdin();
        if should_read_stdin(detect_stdin_source(&stdin)) {
            let mut source = String::new();
            stdin.lock().read_to_string(&mut source)?;
            model.open_untitled("stdin", source);
        } else {
            model = restore_files(preferences);
        }
    } else {
        for path in inputs {
            let source = fs::read_to_string(path)?;
            model.open_file(normalize_path(path.clone()), source);
        }
    }

    Ok(model)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StdinSource {
    Terminal,
    Pipe,
    File,
    Other,
}

fn should_read_stdin(source: StdinSource) -> bool {
    matches!(source, StdinSource::Pipe | StdinSource::File)
}

fn detect_stdin_source(stdin: &io::Stdin) -> StdinSource {
    if stdin.is_terminal() {
        return StdinSource::Terminal;
    }

    #[cfg(unix)]
    {
        stdin_source_from_raw_fd(stdin.as_raw_fd()).unwrap_or(StdinSource::Other)
    }

    #[cfg(not(unix))]
    {
        StdinSource::Other
    }
}

#[cfg(unix)]
fn stdin_source_from_raw_fd(fd: RawFd) -> io::Result<StdinSource> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    let status = unsafe { libc::fstat(fd, stat.as_mut_ptr()) };
    if status != 0 {
        return Err(io::Error::last_os_error());
    }

    let mode = unsafe { stat.assume_init().st_mode } & libc::S_IFMT;
    Ok(match mode {
        libc::S_IFREG => StdinSource::File,
        libc::S_IFIFO => StdinSource::Pipe,
        _ => StdinSource::Other,
    })
}

fn open_path(
    path: PathBuf,
    model: &mut AppModel,
    watcher: &mut FileWatcher,
) -> Result<(), Box<dyn std::error::Error>> {
    let source = fs::read_to_string(&path)?;
    model.open_file(normalize_path(path), source);
    watcher.sync(model.watched_directories())?;
    Ok(())
}

fn open_paths<I>(
    paths: I,
    model: &mut AppModel,
    watcher: &mut FileWatcher,
) -> Result<(), Box<dyn std::error::Error>>
where
    I: IntoIterator<Item = PathBuf>,
{
    for path in paths {
        open_path(path, model, watcher)?;
    }
    Ok(())
}

fn opened_url_file_path(url: &url::Url) -> Option<PathBuf> {
    if url.scheme() == "file" {
        url.to_file_path().ok()
    } else {
        None
    }
}

fn open_document(
    window: &tao::window::Window,
    model: &mut AppModel,
    watcher: &mut FileWatcher,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(path) = rfd::FileDialog::new()
        .set_parent(window)
        .add_filter("Markdown", &["md", "markdown", "mdown"])
        .add_filter("Text", &["txt"])
        .pick_file()
    else {
        return Ok(());
    };

    open_path(path, model, watcher)?;
    Ok(())
}

fn open_dropped_documents(
    paths: Vec<PathBuf>,
    model: &mut AppModel,
    watcher: &mut FileWatcher,
) -> Result<(), Box<dyn std::error::Error>> {
    for path in paths.into_iter().filter(|path| is_markdown_path(path)) {
        open_path(path, model, watcher)?;
    }
    Ok(())
}

fn export_html(
    window: &tao::window::Window,
    model: &AppModel,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(tab) = model.active_tab() else {
        return Ok(());
    };
    export_document_html(window, tab)
}

fn export_tab_html(
    window: &tao::window::Window,
    model: &AppModel,
    id: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(tab) = model.tabs().iter().find(|tab| tab.id() == id) else {
        return Ok(());
    };
    export_document_html(window, tab)
}

fn export_document_html(
    window: &tao::window::Window,
    tab: &markview::DocumentTab,
) -> Result<(), Box<dyn std::error::Error>> {
    let default_name = tab
        .path()
        .and_then(|p| p.file_stem())
        .and_then(|s| s.to_str())
        .unwrap_or_else(|| tab.title())
        .to_owned()
        + ".html";

    let mut dialog = rfd::FileDialog::new()
        .set_parent(window)
        .add_filter("HTML", &["html"])
        .set_file_name(&default_name);

    if let Some(dir) = tab.path().and_then(|p| p.parent()) {
        if !dir.as_os_str().is_empty() {
            dialog = dialog.set_directory(dir);
        }
    }

    let Some(dest) = dialog.save_file() else {
        return Ok(());
    };

    fs::write(dest, HtmlRenderer.render_document(tab.document()))?;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SaveOutcome {
    Saved { path_changed: bool },
    Cancelled,
}

fn save_active(
    window: &tao::window::Window,
    model: &mut AppModel,
) -> Result<SaveOutcome, Box<dyn std::error::Error>> {
    let Some(tab) = model.active_tab() else {
        return Ok(SaveOutcome::Cancelled);
    };
    let id = tab.id();

    let (path, path_changed) = match tab.path() {
        Some(path) => (path.to_path_buf(), false),
        None => {
            let default_name = if is_markdown_path(Path::new(tab.title())) {
                tab.title().to_owned()
            } else {
                format!("{}.md", tab.title())
            };
            let Some(chosen) = rfd::FileDialog::new()
                .set_parent(window)
                .add_filter("Markdown", &["md", "markdown", "mdown"])
                .set_file_name(&default_name)
                .save_file()
            else {
                return Ok(SaveOutcome::Cancelled);
            };
            (chosen, true)
        }
    };

    fs::write(&path, tab.document().source())?;
    model.assign_path(id, normalize_path(path));
    model.mark_saved(id);
    Ok(SaveOutcome::Saved { path_changed })
}

fn sync_watcher(watcher: &mut FileWatcher, model: &AppModel) {
    if let Err(error) = watcher.sync(model.watched_directories()) {
        eprintln!("markview-gui: {error}");
    }
}

fn sync_after_tab_mutation(
    watcher: &mut FileWatcher,
    preferences_path: &Path,
    preferences: &mut GuiPreferences,
    model: &AppModel,
    webview: &WebView,
    window: &tao::window::Window,
) {
    sync_watcher(watcher, model);
    sync_persisted_view(preferences_path, preferences, model, webview, window);
}

fn sync_persisted_view(
    preferences_path: &Path,
    preferences: &mut GuiPreferences,
    model: &AppModel,
    webview: &WebView,
    window: &tao::window::Window,
) {
    persist_open_state(preferences_path, preferences, model, Some(window));
    sync_view(webview, model, preferences);
    window.set_title(&window_title(model));
}

fn sync_view(webview: &WebView, model: &AppModel, preferences: &GuiPreferences) {
    let script = format!(
        "window.markview.setState({});",
        view_js(&app_view_with_preferences(model, preferences.clone()))
    );
    if let Err(error) = webview.evaluate_script(&script) {
        eprintln!("markview-gui: failed to update view: {error}");
    }
}

fn window_title(model: &AppModel) -> String {
    let Some(tab) = model.active_tab() else {
        return "markview - No document".to_owned();
    };
    let marker = if tab.is_dirty() { "\u{25CF} " } else { "" };
    format!("markview - {marker}{}", tab.title())
}

/// Shows a blocking "discard changes?" prompt and returns whether the user confirmed.
fn confirm_discard_changes(window: &tao::window::Window, message: &str) -> bool {
    rfd::MessageDialog::new()
        .set_parent(window)
        .set_level(rfd::MessageLevel::Warning)
        .set_title("Unsaved Changes")
        .set_description(message)
        .set_buttons(rfd::MessageButtons::YesNo)
        .show()
        == rfd::MessageDialogResult::Yes
}

fn confirm_if_dirty(window: &tao::window::Window, dirty: bool, message: &str) -> bool {
    !dirty || confirm_discard_changes(window, message)
}

fn tab_dirty(model: &AppModel, id: u64) -> bool {
    model
        .tabs()
        .iter()
        .find(|tab| tab.id() == id)
        .is_some_and(markview::DocumentTab::is_dirty)
}

fn ids_dirty(model: &AppModel, ids: &[u64]) -> bool {
    model
        .tabs()
        .iter()
        .any(|tab| ids.contains(&tab.id()) && tab.is_dirty())
}

fn other_tabs_dirty(model: &AppModel, keep_id: u64) -> bool {
    ids_dirty(model, &model.other_tab_ids(keep_id))
}

fn tabs_to_left_dirty(model: &AppModel, id: u64) -> bool {
    ids_dirty(model, &model.tab_ids_to_left(id))
}

fn tabs_to_right_dirty(model: &AppModel, id: u64) -> bool {
    ids_dirty(model, &model.tab_ids_to_right(id))
}

fn any_tab_dirty(model: &AppModel) -> bool {
    model.tabs().iter().any(|tab| tab.is_dirty())
}

#[derive(Debug, Clone)]
enum UserEvent {
    OpenRequested,
    RefreshRequested,
    PrintRequested,
    FindRequested,
    QuitRequested,
    ToggleSidebar,
    ToggleAutoRefresh,
    CycleTheme,
    OpenExternal(String),
    DroppedFiles(Vec<PathBuf>),
    OpenRecent(PathBuf),
    SelectTab(u64),
    CloseTab(u64),
    CloseActiveTab,
    CloseOtherTabs(u64),
    CloseTabsToLeft(u64),
    CloseTabsToRight(u64),
    ReloadTab(u64),
    ExportHtmlRequested,
    ExportTabHtml(u64),
    FilesChanged(Vec<PathBuf>),
    ToggleActiveEdit,
    EditChanged(u64, String),
    SaveRequested,
}

#[cfg(target_os = "macos")]
fn install_application_menu(proxy: EventLoopProxy<UserEvent>) {
    use objc2::rc::Retained;
    use objc2::runtime::Sel;
    use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
    use objc2_app_kit::{NSApplication, NSEventModifierFlags, NSMenu, NSMenuItem};
    use objc2_foundation::{NSObject, NSObjectProtocol, NSString};

    struct MenuCommandTargetIvars {
        proxy: EventLoopProxy<UserEvent>,
    }

    define_class!(
        // SAFETY: NSObject has no extra subclassing requirements, and the
        // target only forwards menu actions to tao's event proxy.
        #[unsafe(super = NSObject)]
        #[thread_kind = MainThreadOnly]
        #[ivars = MenuCommandTargetIvars]
        struct MenuCommandTarget;

        unsafe impl NSObjectProtocol for MenuCommandTarget {}

        impl MenuCommandTarget {
            #[unsafe(method(markviewOpenDocument))]
            fn open_document(&self) {
                self.send(UserEvent::OpenRequested);
            }

            #[unsafe(method(markviewCloseTab))]
            fn close_tab(&self) {
                self.send(UserEvent::CloseActiveTab);
            }

            #[unsafe(method(markviewRefresh))]
            fn refresh(&self) {
                self.send(UserEvent::RefreshRequested);
            }

            #[unsafe(method(markviewSave))]
            fn save(&self) {
                self.send(UserEvent::SaveRequested);
            }

            #[unsafe(method(markviewToggleEdit))]
            fn toggle_edit(&self) {
                self.send(UserEvent::ToggleActiveEdit);
            }

            #[unsafe(method(markviewPrint))]
            fn print(&self) {
                self.send(UserEvent::PrintRequested);
            }

            #[unsafe(method(markviewFind))]
            fn find(&self) {
                self.send(UserEvent::FindRequested);
            }

            #[unsafe(method(markviewToggleSidebar))]
            fn toggle_sidebar(&self) {
                self.send(UserEvent::ToggleSidebar);
            }

            #[unsafe(method(markviewToggleAutoRefresh))]
            fn toggle_auto_refresh(&self) {
                self.send(UserEvent::ToggleAutoRefresh);
            }

            #[unsafe(method(markviewExportHtml))]
            fn export_html(&self) {
                self.send(UserEvent::ExportHtmlRequested);
            }

            #[unsafe(method(markviewQuit))]
            fn quit(&self) {
                self.send(UserEvent::QuitRequested);
            }
        }
    );

    impl MenuCommandTarget {
        fn new(mtm: MainThreadMarker, proxy: EventLoopProxy<UserEvent>) -> Retained<Self> {
            let this = mtm
                .alloc::<Self>()
                .set_ivars(MenuCommandTargetIvars { proxy });
            unsafe { msg_send![super(this), init] }
        }

        fn send(&self, event: UserEvent) {
            let _ = self.ivars().proxy.send_event(event);
        }
    }

    fn menu_item(menu: &NSMenu, target: &MenuCommandTarget, title: &str, action: Sel, key: &str) {
        let item = unsafe {
            menu.addItemWithTitle_action_keyEquivalent(
                &NSString::from_str(title),
                Some(action),
                &NSString::from_str(key),
            )
        };
        unsafe {
            item.setTarget(Some(target));
        }
        if !key.is_empty() {
            item.setKeyEquivalentModifierMask(NSEventModifierFlags::Command);
        }
    }

    // Items with no explicit target route through the responder chain, letting
    // the focused WebView handle standard editing actions (copy, paste, etc.).
    fn system_menu_item(menu: &NSMenu, title: &str, action: Sel, key: &str) {
        let item = unsafe {
            menu.addItemWithTitle_action_keyEquivalent(
                &NSString::from_str(title),
                Some(action),
                &NSString::from_str(key),
            )
        };
        if !key.is_empty() {
            item.setKeyEquivalentModifierMask(NSEventModifierFlags::Command);
        }
    }

    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };

    let app = NSApplication::sharedApplication(mtm);
    let main_menu = NSMenu::initWithTitle(mtm.alloc(), &NSString::from_str(""));
    let command_target = MenuCommandTarget::new(mtm, proxy);

    let app_item = NSMenuItem::new(mtm);
    let app_menu = NSMenu::initWithTitle(mtm.alloc(), &NSString::from_str("Markview"));
    unsafe {
        app_menu.addItemWithTitle_action_keyEquivalent(
            &NSString::from_str("Hide Markview"),
            Some(sel!(hide:)),
            &NSString::from_str("h"),
        );
    }
    let hide_others = unsafe {
        app_menu.addItemWithTitle_action_keyEquivalent(
            &NSString::from_str("Hide Others"),
            Some(sel!(hideOtherApplications:)),
            &NSString::from_str("h"),
        )
    };
    hide_others
        .setKeyEquivalentModifierMask(NSEventModifierFlags::Command | NSEventModifierFlags::Option);
    unsafe {
        app_menu.addItemWithTitle_action_keyEquivalent(
            &NSString::from_str("Show All"),
            Some(sel!(unhideAllApplications:)),
            &NSString::from_str(""),
        );
    }
    app_menu.addItem(&NSMenuItem::separatorItem(mtm));
    menu_item(
        &app_menu,
        &command_target,
        "Quit Markview",
        sel!(markviewQuit),
        "q",
    );
    app_item.setSubmenu(Some(&app_menu));
    main_menu.addItem(&app_item);

    let file_item = NSMenuItem::new(mtm);
    let file_menu = NSMenu::initWithTitle(mtm.alloc(), &NSString::from_str("File"));
    menu_item(
        &file_menu,
        &command_target,
        "Open...",
        sel!(markviewOpenDocument),
        "o",
    );
    menu_item(
        &file_menu,
        &command_target,
        "Close Tab",
        sel!(markviewCloseTab),
        "w",
    );
    menu_item(
        &file_menu,
        &command_target,
        "Refresh",
        sel!(markviewRefresh),
        "r",
    );
    file_menu.addItem(&NSMenuItem::separatorItem(mtm));
    menu_item(&file_menu, &command_target, "Save", sel!(markviewSave), "s");
    file_menu.addItem(&NSMenuItem::separatorItem(mtm));
    menu_item(
        &file_menu,
        &command_target,
        "Print...",
        sel!(markviewPrint),
        "p",
    );
    menu_item(
        &file_menu,
        &command_target,
        "Export as HTML...",
        sel!(markviewExportHtml),
        "",
    );
    file_item.setSubmenu(Some(&file_menu));
    main_menu.addItem(&file_item);

    let edit_item = NSMenuItem::new(mtm);
    let edit_menu = NSMenu::initWithTitle(mtm.alloc(), &NSString::from_str("Edit"));
    system_menu_item(&edit_menu, "Copy", sel!(copy:), "c");
    system_menu_item(&edit_menu, "Select All", sel!(selectAll:), "a");
    edit_menu.addItem(&NSMenuItem::separatorItem(mtm));
    menu_item(&edit_menu, &command_target, "Find", sel!(markviewFind), "f");
    edit_item.setSubmenu(Some(&edit_menu));
    main_menu.addItem(&edit_item);

    let view_item = NSMenuItem::new(mtm);
    let view_menu = NSMenu::initWithTitle(mtm.alloc(), &NSString::from_str("View"));
    menu_item(
        &view_menu,
        &command_target,
        "Toggle Edit Mode",
        sel!(markviewToggleEdit),
        "e",
    );
    menu_item(
        &view_menu,
        &command_target,
        "Toggle Sidebar",
        sel!(markviewToggleSidebar),
        "",
    );
    menu_item(
        &view_menu,
        &command_target,
        "Toggle Auto Refresh",
        sel!(markviewToggleAutoRefresh),
        "",
    );
    view_item.setSubmenu(Some(&view_menu));
    main_menu.addItem(&view_item);

    let window_item = NSMenuItem::new(mtm);
    let window_menu = NSMenu::initWithTitle(mtm.alloc(), &NSString::from_str("Window"));
    unsafe {
        window_menu.addItemWithTitle_action_keyEquivalent(
            &NSString::from_str("Minimize"),
            Some(sel!(performMiniaturize:)),
            &NSString::from_str("m"),
        );
        window_menu.addItemWithTitle_action_keyEquivalent(
            &NSString::from_str("Zoom"),
            Some(sel!(performZoom:)),
            &NSString::from_str(""),
        );
    }
    window_item.setSubmenu(Some(&window_menu));
    main_menu.addItem(&window_item);

    app.setMainMenu(Some(&main_menu));
    let _ = Retained::into_raw(command_target);
}

#[cfg(not(target_os = "macos"))]
fn install_application_menu(_proxy: EventLoopProxy<UserEvent>) {}

struct FileWatcher {
    watcher: RecommendedWatcher,
    watched_directories: HashSet<PathBuf>,
}

impl FileWatcher {
    fn new(proxy: EventLoopProxy<UserEvent>) -> notify::Result<Self> {
        let watcher = RecommendedWatcher::new(
            move |result: notify::Result<notify::Event>| {
                if let Ok(event) = result {
                    if is_refresh_event(&event.kind) {
                        let paths = event.paths.into_iter().map(normalize_path).collect();
                        let _ = proxy.send_event(UserEvent::FilesChanged(paths));
                    }
                }
            },
            Config::default(),
        )?;

        Ok(Self {
            watcher,
            watched_directories: HashSet::new(),
        })
    }

    fn sync<I>(&mut self, directories: I) -> notify::Result<()>
    where
        I: IntoIterator<Item = PathBuf>,
    {
        let next = directories
            .into_iter()
            .map(normalize_path)
            .collect::<HashSet<_>>();

        for directory in next.difference(&self.watched_directories) {
            self.watcher.watch(directory, RecursiveMode::NonRecursive)?;
        }

        for directory in self.watched_directories.difference(&next) {
            self.watcher.unwatch(directory)?;
        }

        self.watched_directories = next;
        Ok(())
    }
}

fn is_refresh_event(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
    )
}

fn is_external_url(url: &str) -> bool {
    url.starts_with("https://") || url.starts_with("http://")
}

fn is_markdown_path(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "md" | "markdown" | "mdown"
            )
        })
}

fn open_external_url(url: &str) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        run_external_open(Command::new("open").arg(url))
    }

    #[cfg(target_os = "windows")]
    {
        run_external_open(Command::new("cmd").args(["/C", "start", "", url]))
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        run_external_open(Command::new("xdg-open").arg(url))
    }
}

fn run_external_open(command: &mut Command) -> io::Result<()> {
    let status = command.status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "external opener exited with {status}"
        )))
    }
}

fn app_shell_html(view: &AppView) -> String {
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>markview</title>
<style>
:root {{
  color-scheme: light dark;
  --chrome: #ece8e1;
  --chrome-strong: #ded8cf;
  --bg: #f8f7f4;
  --fg: #242220;
  --muted: #6c665f;
  --rule: #d8d2ca;
  --accent: #0f766e;
  --code-bg: #ebe6de;
  --quote-bg: #f1ede7;
}}
@media (prefers-color-scheme: dark) {{
  :root {{
    --chrome: #211f1c;
    --chrome-strong: #302c27;
    --bg: #181715;
    --fg: #eeeae4;
    --muted: #aaa39a;
    --rule: #39342f;
    --accent: #5eead4;
    --code-bg: #25221f;
    --quote-bg: #211f1c;
  }}
}}
:root[data-theme="light"] {{
  color-scheme: light;
  --chrome: #ece8e1;
  --chrome-strong: #ded8cf;
  --bg: #f8f7f4;
  --fg: #242220;
  --muted: #6c665f;
  --rule: #d8d2ca;
  --accent: #0f766e;
  --code-bg: #ebe6de;
  --quote-bg: #f1ede7;
}}
:root[data-theme="dark"] {{
  color-scheme: dark;
  --chrome: #211f1c;
  --chrome-strong: #302c27;
  --bg: #181715;
  --fg: #eeeae4;
  --muted: #aaa39a;
  --rule: #39342f;
  --accent: #5eead4;
  --code-bg: #25221f;
  --quote-bg: #211f1c;
}}
* {{ box-sizing: border-box; }}
html {{
  height: 100%;
  overflow: hidden;
}}
body {{
  margin: 0;
  height: 100%;
  overflow: hidden;
  background: var(--bg);
  color: var(--fg);
  font: 16px/1.65 -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
  display: grid;
  grid-template-rows: 46px 38px minmax(0, 1fr);
}}
.toolbar {{
  height: 46px;
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 0 12px;
  background: var(--chrome);
  border-bottom: 1px solid var(--rule);
  min-width: 0;
}}
.tool-button {{
  appearance: none;
  border: 1px solid var(--rule);
  background: var(--bg);
  color: var(--fg);
  min-width: 34px;
  height: 30px;
  border-radius: 7px;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  cursor: default;
  position: relative;
}}
.tool-button:hover {{ border-color: var(--accent); }}
.tool-button[data-tooltip]:hover::after,
.tool-button[data-tooltip]:focus-visible::after {{
  content: attr(data-tooltip);
  position: absolute;
  top: calc(100% + 7px);
  left: 50%;
  transform: translateX(-50%);
  z-index: 20;
  white-space: nowrap;
  max-width: 220px;
  overflow: hidden;
  text-overflow: ellipsis;
  padding: 4px 7px;
  border-radius: 5px;
  background: var(--fg);
  color: var(--bg);
  font-size: 0.75rem;
  line-height: 1.2;
  pointer-events: none;
  box-shadow: 0 2px 8px rgba(0, 0, 0, 0.18);
}}
.tool-button[data-tooltip]:hover::before,
.tool-button[data-tooltip]:focus-visible::before {{
  content: "";
  position: absolute;
  top: calc(100% + 2px);
  left: 50%;
  transform: translateX(-50%);
  z-index: 21;
  border: 5px solid transparent;
  border-bottom-color: var(--fg);
  pointer-events: none;
}}
.tool-button.active {{
  border-color: var(--accent);
  color: var(--accent);
}}
.tool-button svg {{ width: 17px; height: 17px; }}
.recent-select {{
  appearance: none;
  height: 30px;
  max-width: 180px;
  border: 1px solid var(--rule);
  border-radius: 7px;
  background: var(--bg);
  color: var(--fg);
  padding: 0 26px 0 9px;
  font: inherit;
  font-size: 0.86rem;
}}
.recent-select:disabled {{
  color: var(--muted);
}}
.tabs {{
  height: 38px;
  display: flex;
  align-items: end;
  gap: 1px;
  padding: 0 8px;
  background: var(--chrome-strong);
  border-bottom: 1px solid var(--rule);
  overflow-x: auto;
  min-width: 0;
  scrollbar-width: thin;
}}
.tab {{
  appearance: none;
  border: 1px solid var(--rule);
  border-bottom: 0;
  background: var(--chrome);
  color: var(--muted);
  height: 31px;
  width: 190px;
  padding: 0 8px 0 13px;
  border-radius: 7px 7px 0 0;
  display: inline-flex;
  align-items: center;
  gap: 8px;
  flex: 0 0 190px;
  min-width: 0;
}}
.tab.active {{
  background: var(--bg);
  color: var(--fg);
  border-color: var(--accent);
}}
.tab.stale .tab-title::after {{
  content: " modified";
  color: var(--accent);
  font-size: 0.78rem;
  margin-left: 6px;
}}
.tab.dirty .tab-title::before {{
  content: "\25CF ";
  color: var(--accent);
  font-size: 0.7rem;
}}
.tab-title {{
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}}
.tab-close {{
  appearance: none;
  border: 0;
  background: transparent;
  color: var(--muted);
  width: 18px;
  height: 18px;
  border-radius: 50%;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  padding: 0;
  flex: 0 0 auto;
}}
.tab-close:hover {{
  background: var(--chrome-strong);
  color: var(--fg);
}}
.tab-close svg {{ width: 12px; height: 12px; }}
.tab-count {{
  color: var(--muted);
  font-size: 0.78rem;
  padding: 0 8px 8px;
  white-space: nowrap;
  flex: 0 0 auto;
}}
.context-menu {{
  position: fixed;
  z-index: 50;
  min-width: 180px;
  background: var(--bg);
  border: 1px solid var(--rule);
  border-radius: 8px;
  padding: 4px;
  box-shadow: 0 8px 24px rgba(0, 0, 0, 0.22);
}}
.context-menu.hidden {{
  display: none;
}}
.context-menu-item {{
  appearance: none;
  border: 0;
  background: transparent;
  color: var(--fg);
  width: 100%;
  min-height: 30px;
  border-radius: 5px;
  padding: 0 10px;
  text-align: left;
  font: inherit;
  font-size: 0.88rem;
}}
.context-menu-item:hover:not(:disabled) {{
  background: var(--chrome-strong);
}}
.context-menu-item:disabled {{
  color: var(--muted);
}}
.context-menu-separator {{
  height: 1px;
  margin: 4px 6px;
  background: var(--rule);
}}
.findbar {{
  display: inline-flex;
  align-items: center;
  gap: 6px;
  margin-left: auto;
  min-width: 0;
}}
.find-input {{
  appearance: none;
  width: 220px;
  height: 30px;
  border: 1px solid var(--rule);
  border-radius: 7px;
  background: var(--bg);
  color: var(--fg);
  padding: 0 9px;
  font: inherit;
  font-size: 0.88rem;
}}
.find-count {{
  min-width: 54px;
  color: var(--muted);
  font-size: 0.82rem;
  text-align: right;
}}
.scroll-root {{
  min-height: 0;
  overflow: auto;
}}
.content-shell {{
  display: grid;
  grid-template-columns: minmax(170px, 250px) minmax(0, 1fr);
  gap: 28px;
  width: min(1120px, calc(100vw - 48px));
  margin: 0 auto;
  padding: 0 0 64px;
}}
.toc {{
  position: sticky;
  top: 0;
  align-self: start;
  max-height: calc(100vh - 86px);
  overflow: auto;
  padding: 38px 0 0;
}}
.toc.hidden {{
  display: none;
}}
.content-shell.sidebar-hidden {{
  grid-template-columns: minmax(0, 1fr);
  width: min(860px, calc(100vw - 48px));
}}
.toc-list {{
  display: flex;
  flex-direction: column;
  gap: 2px;
}}
.toc-link {{
  appearance: none;
  border: 0;
  background: transparent;
  color: var(--muted);
  width: 100%;
  min-height: 28px;
  border-radius: 6px;
  padding: 4px 8px;
  text-align: left;
  font: inherit;
  font-size: 0.88rem;
  line-height: 1.3;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}}
.toc-link:hover {{
  background: var(--chrome);
  color: var(--fg);
}}
.toc-empty {{
  color: var(--muted);
  font-size: 0.86rem;
  padding: 4px 8px;
}}
main {{
  padding: 40px 0 64px;
  min-width: 0;
}}
.editor {{
  appearance: none;
  display: block;
  width: 100%;
  min-height: calc(100vh - 190px);
  resize: vertical;
  border: 1px solid var(--rule);
  border-radius: 8px;
  background: var(--code-bg);
  color: var(--fg);
  padding: 16px;
  font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
  font-size: 0.92rem;
  line-height: 1.6;
  tab-size: 2;
}}
.editor:focus {{
  outline: none;
  border-color: var(--accent);
}}
mark.find-hit {{
  background: #facc15;
  color: #1f2937;
  border-radius: 3px;
  padding: 0 1px;
}}
mark.find-hit.active {{
  background: #fb923c;
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
.syntax-keyword {{ color: var(--accent); font-weight: 700; }}
.syntax-comment {{ color: var(--muted); font-style: italic; }}
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
.empty-state {{
  color: var(--muted);
  min-height: calc(100vh - 210px);
  display: flex;
  flex-direction: column;
  justify-content: center;
  align-items: flex-start;
  gap: 14px;
  padding: 42px 0;
}}
.empty-eyebrow {{
  color: var(--accent);
  font-size: 0.78rem;
  font-weight: 700;
  letter-spacing: 0;
  text-transform: uppercase;
}}
.empty-state h1 {{
  color: var(--fg);
  font-size: 2rem;
  margin: 0;
}}
.empty-state p {{
  max-width: 520px;
  margin: 0;
}}
.empty-actions {{
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 12px;
  padding-top: 8px;
}}
.empty-action {{
  appearance: none;
  border: 1px solid var(--rule);
  background: var(--bg);
  color: var(--fg);
  min-height: 34px;
  border-radius: 7px;
  padding: 0 12px;
  font: inherit;
  font-size: 0.92rem;
}}
.empty-action.primary {{
  border-color: var(--accent);
  color: var(--accent);
}}
.empty-hint {{
  color: var(--muted);
  font-size: 0.86rem;
}}
@media (max-width: 760px) {{
  .find-input {{ width: 150px; }}
  .content-shell {{
    display: block;
    width: min(860px, calc(100vw - 32px));
  }}
  .toc {{
    position: static;
    max-height: none;
    padding-top: 18px;
  }}
  .empty-state {{ min-height: auto; }}
  main {{ padding-top: 24px; }}
}}
@media print {{
  @page {{
    margin: 0.65in;
  }}
  html, body {{
    height: auto;
    overflow: visible;
    display: block;
    background: #fff;
    color: #000;
    font-size: 11pt;
  }}
  .toolbar, .tabs, .toc {{
    display: none;
  }}
  .scroll-root {{
    overflow: visible;
  }}
  .content-shell {{
    display: block;
    width: auto;
    margin: 0;
    padding: 0;
  }}
  main {{
    padding: 0;
  }}
  h1, h2, h3, h4, h5, h6 {{
    color: #000;
    break-after: avoid;
  }}
  a {{
    color: #000;
  }}
  a[href^="http"]::after {{
    content: " (" attr(href) ")";
    font-size: 0.86em;
    overflow-wrap: anywhere;
  }}
  pre, blockquote, table {{
    break-inside: avoid;
  }}
  pre, code, blockquote, th {{
    background: #f5f5f5;
    color: #000;
  }}
  .syntax-keyword, .syntax-comment {{
    color: #000;
    font-weight: 700;
    font-style: normal;
  }}
  input[type="checkbox"] {{
    filter: grayscale(1);
  }}
}}
</style>
</head>
<body>
<header class="toolbar">
  <button class="tool-button" title="Open Markdown file" data-tooltip="Open Markdown file" aria-label="Open Markdown file" onclick="window.ipc.postMessage('open')">
    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
      <path d="M3 7h5l2 2h11v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2Z"></path>
      <path d="M3 7v11"></path>
    </svg>
  </button>
  <button class="tool-button" title="Refresh active document" data-tooltip="Refresh active document" aria-label="Refresh active document" onclick="window.ipc.postMessage('refresh')">
    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
      <path d="M21 12a9 9 0 0 1-15.5 6.2"></path>
      <path d="M3 12A9 9 0 0 1 18.5 5.8"></path>
      <path d="M18 2v5h-5"></path>
      <path d="M6 22v-5h5"></path>
    </svg>
  </button>
  <button class="tool-button" title="Toggle edit mode" data-tooltip="Toggle edit mode" aria-label="Toggle edit mode" id="edit-toggle" onclick="window.ipc.postMessage('toggle-edit')">
    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
      <path d="M17 3a2.85 2.83 0 1 1 4 4L7.5 20.5 2 22l1.5-5.5Z"></path>
    </svg>
  </button>
  <button class="tool-button" title="Save active document" data-tooltip="Save active document" aria-label="Save active document" onclick="window.ipc.postMessage('save')">
    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
      <path d="M19 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h11l5 5v11a2 2 0 0 1-2 2Z"></path>
      <path d="M17 21v-8H7v8"></path>
      <path d="M7 3v5h8"></path>
    </svg>
  </button>
  <button class="tool-button" title="Print document" data-tooltip="Print document" aria-label="Print document" onclick="window.ipc.postMessage('print')">
    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
      <path d="M6 9V2h12v7"></path>
      <path d="M6 18H4a2 2 0 0 1-2-2v-5a2 2 0 0 1 2-2h16a2 2 0 0 1 2 2v5a2 2 0 0 1-2 2h-2"></path>
      <path d="M6 14h12v8H6z"></path>
    </svg>
  </button>
  <button class="tool-button" title="Export as HTML" data-tooltip="Export as HTML" aria-label="Export as HTML" onclick="window.ipc.postMessage('export-html')">
    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
      <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"></path>
      <polyline points="7 10 12 15 17 10"></polyline>
      <line x1="12" y1="15" x2="12" y2="3"></line>
    </svg>
  </button>
  <button class="tool-button" title="Toggle table of contents" data-tooltip="Toggle table of contents" aria-label="Toggle table of contents" id="sidebar-toggle" onclick="window.ipc.postMessage('toggle-sidebar')">
    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
      <rect x="3" y="4" width="18" height="16" rx="2"></rect>
      <path d="M9 4v16"></path>
    </svg>
  </button>
  <button class="tool-button" title="Auto-refresh on file changes" data-tooltip="Auto-refresh on file changes" aria-label="Auto-refresh on file changes" id="auto-refresh-toggle" onclick="window.ipc.postMessage('toggle-auto-refresh')">
    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
      <circle cx="12" cy="12" r="8"></circle>
      <path d="M12 7v5l3 2"></path>
      <path d="M19 5v5h-5"></path>
      <path d="M5 19v-5h5"></path>
    </svg>
  </button>
  <button class="tool-button" title="Cycle theme" data-tooltip="Cycle theme" aria-label="Cycle theme" id="theme-toggle" onclick="window.ipc.postMessage('cycle-theme')">
    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
      <path d="M12 3a9 9 0 1 0 9 9 7 7 0 0 1-9-9Z"></path>
    </svg>
  </button>
  <select class="recent-select" id="recent-files" aria-label="Recent files">
    <option value="">Recent</option>
  </select>
  <div class="findbar">
    <input class="find-input" id="find-input" placeholder="Find" aria-label="Find in document">
    <button class="tool-button" title="Previous match" data-tooltip="Previous match" aria-label="Previous match" id="find-prev">
      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
        <path d="m18 15-6-6-6 6"></path>
      </svg>
    </button>
    <button class="tool-button" title="Next match" data-tooltip="Next match" aria-label="Next match" id="find-next">
      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
        <path d="m6 9 6 6 6-6"></path>
      </svg>
    </button>
    <span class="find-count" id="find-count"></span>
  </div>
</header>
<nav class="tabs" id="tabs"></nav>
<div class="context-menu hidden" id="tab-context-menu" role="menu"></div>
<div class="scroll-root" id="scroll-root">
  <div class="content-shell">
    <aside class="toc" id="toc"></aside>
    <main id="document"></main>
  </div>
</div>
<script>
function setTip(el, tip) {{
  el.title = tip;
  el.dataset.tooltip = tip;
  el.setAttribute('aria-label', tip);
}}
window.markview = {{
  state: {state},
  scrollPositions: new Map(),
  findQuery: '',
  findIndex: -1,
  findHits: [],
  setState(next) {{
    const scroller = document.getElementById('scroll-root');
    const previousId = this.state ? this.state.activeTabId : null;
    if (previousId !== null) {{
      this.scrollPositions.set(previousId, scroller.scrollTop);
    }}
    this.state = next;
    const tabs = document.getElementById('tabs');
    const pane = document.getElementById('document');
    const toc = document.getElementById('toc');
    const shell = document.querySelector('.content-shell');
    const recent = document.getElementById('recent-files');
    document.documentElement.dataset.theme = next.preferences.theme === 'system' ? '' : next.preferences.theme;
    const sidebarToggle = document.getElementById('sidebar-toggle');
    sidebarToggle.classList.toggle('active', next.preferences.sidebarVisible);
    setTip(sidebarToggle, next.preferences.sidebarVisible ? 'Hide table of contents' : 'Show table of contents');

    const autoRefreshToggle = document.getElementById('auto-refresh-toggle');
    autoRefreshToggle.classList.toggle('active', next.preferences.autoRefresh);
    setTip(autoRefreshToggle, next.preferences.autoRefresh ? 'Disable auto-refresh on file changes' : 'Enable auto-refresh on file changes');

    const themeToggle = document.getElementById('theme-toggle');
    setTip(themeToggle, `Theme: ${{next.preferences.theme}}`);

    const activeTabView = next.tabs.find(tab => tab.id === next.activeTabId) || null;
    const isEditingActive = Boolean(activeTabView && activeTabView.editing);
    const editToggle = document.getElementById('edit-toggle');
    editToggle.classList.toggle('active', Boolean(activeTabView && activeTabView.editing));
    editToggle.disabled = !activeTabView;
    setTip(editToggle, activeTabView && activeTabView.editing ? 'Preview document' : 'Edit document');

    recent.replaceChildren();
    const placeholder = document.createElement('option');
    placeholder.value = '';
    placeholder.textContent = 'Recent';
    recent.appendChild(placeholder);
    for (const path of next.preferences.recentFiles) {{
      const option = document.createElement('option');
      option.value = path;
      option.textContent = fileName(path);
      option.title = path;
      recent.appendChild(option);
    }}
    recent.disabled = next.preferences.recentFiles.length === 0;
    tabs.replaceChildren();
    for (const tab of next.tabs) {{
      const button = document.createElement('button');
      button.className = 'tab' + (tab.id === next.activeTabId ? ' active' : '') + (tab.stale ? ' stale' : '') + (tab.dirty ? ' dirty' : '');
      button.dataset.tabId = String(tab.id);
      button.title = tab.path || tab.title;
      button.onclick = () => window.ipc.postMessage(`select:${{tab.id}}`);
      button.oncontextmenu = (event) => {{
        event.preventDefault();
        window.markview.showTabContextMenu(tab.id, event.clientX, event.clientY);
      }};
      const label = document.createElement('span');
      label.className = 'tab-title';
      label.textContent = tab.title;
      const close = document.createElement('button');
      close.className = 'tab-close';
      close.title = `Close ${{tab.title}}`;
      close.setAttribute('aria-label', `Close ${{tab.title}}`);
      close.innerHTML = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4"><path d="M18 6 6 18"></path><path d="m6 6 12 12"></path></svg>';
      close.onclick = (event) => {{
        event.stopPropagation();
        window.ipc.postMessage(`close:${{tab.id}}`);
      }};
      button.append(label, close);
      tabs.appendChild(button);
    }}
    if (next.tabs.length > 0) {{
      const count = document.createElement('span');
      count.className = 'tab-count';
      count.textContent = `${{next.tabs.length}} open`;
      tabs.appendChild(count);
    }}
    const existingEditor = pane.querySelector('textarea.editor');
    if (isEditingActive) {{
      shell.classList.add('sidebar-hidden');
      toc.classList.add('hidden');
      if (!existingEditor || existingEditor.dataset.tabId !== String(next.activeTabId)) {{
        pane.innerHTML = '';
        const textarea = document.createElement('textarea');
        textarea.className = 'editor';
        textarea.dataset.tabId = String(next.activeTabId);
        textarea.value = next.activeSource;
        textarea.spellcheck = false;
        textarea.placeholder = 'Start writing Markdown...';
        textarea.addEventListener('input', () => {{
          window.ipc.postMessage(`edit:${{next.activeTabId}}:${{textarea.value}}`);
        }});
        pane.appendChild(textarea);
        textarea.focus();
      }}
    }} else {{
      pane.innerHTML = next.activeHtml;
      for (const action of pane.querySelectorAll('[data-action="open"]')) {{
        action.addEventListener('click', () => window.ipc.postMessage('open'));
      }}
      const renderedHeadings = pane.querySelectorAll('h1,h2,h3,h4,h5,h6');
      next.headings.forEach((heading, index) => {{
        if (renderedHeadings[index]) {{
          renderedHeadings[index].id = heading.id;
        }}
      }});
      toc.replaceChildren();
      toc.classList.toggle('hidden', !next.preferences.sidebarVisible);
      shell.classList.toggle('sidebar-hidden', !next.preferences.sidebarVisible);
      if (next.headings.length === 0) {{
        const empty = document.createElement('div');
        empty.className = 'toc-empty';
        empty.textContent = 'No headings';
        toc.appendChild(empty);
      }} else {{
        const list = document.createElement('div');
        list.className = 'toc-list';
        for (const heading of next.headings) {{
          const item = document.createElement('button');
          item.className = 'toc-link';
          item.style.paddingLeft = `${{8 + Math.max(0, heading.level - 1) * 12}}px`;
          item.textContent = heading.title;
          item.title = heading.title;
          item.onclick = () => {{
            const target = document.getElementById(heading.id);
            if (target) {{
              scrollInside(target, 'start');
              history.replaceState(null, '', `#${{heading.id}}`);
            }}
          }};
          list.appendChild(item);
        }}
        toc.appendChild(list);
      }}
    }}
    this.applyFind();
    const restoreY = this.scrollPositions.get(next.activeTabId) || 0;
    requestAnimationFrame(() => {{
      scroller.scrollTop = restoreY;
      const activeTab = tabs.querySelector('.tab.active');
      if (activeTab) {{
        activeTab.scrollIntoView({{ block: 'nearest', inline: 'nearest' }});
      }}
    }});
  }},
  applyFind() {{
    const pane = document.getElementById('document');
    const count = document.getElementById('find-count');
    unwrapFindMarks(pane);
    this.findHits = [];
    this.findIndex = -1;
    const query = this.findQuery.trim();
    if (query.length === 0) {{
      count.textContent = '';
      return;
    }}
    this.findHits = highlightText(pane, query);
    if (this.findHits.length > 0) {{
      this.findIndex = 0;
      this.activateFindHit(0);
    }}
    count.textContent = this.findHits.length === 0 ? '0/0' : `1/${{this.findHits.length}}`;
  }},
  activateFindHit(index) {{
    if (this.findHits.length === 0) {{
      document.getElementById('find-count').textContent = '0/0';
      return;
    }}
    this.findHits.forEach(hit => hit.classList.remove('active'));
    this.findIndex = (index + this.findHits.length) % this.findHits.length;
    const hit = this.findHits[this.findIndex];
    hit.classList.add('active');
    scrollInside(hit, 'center');
    document.getElementById('find-count').textContent = `${{this.findIndex + 1}}/${{this.findHits.length}}`;
  }},
  findNext() {{
    this.activateFindHit(this.findIndex + 1);
  }},
  findPrevious() {{
    this.activateFindHit(this.findIndex - 1);
  }},
  showTabContextMenu(id, x, y) {{
    const tabs = this.state.tabs;
    const index = tabs.findIndex(tab => tab.id === id);
    if (index === -1) return;
    const tab = tabs[index];
    const menu = document.getElementById('tab-context-menu');
    menu.replaceChildren();
    const addItem = (label, message, disabled) => {{
      const item = document.createElement('button');
      item.className = 'context-menu-item';
      item.setAttribute('role', 'menuitem');
      item.textContent = label;
      item.disabled = Boolean(disabled);
      if (!disabled) {{
        item.onclick = () => {{
          window.ipc.postMessage(message);
          this.hideTabContextMenu();
        }};
      }}
      menu.appendChild(item);
    }};
    const addSeparator = () => {{
      const separator = document.createElement('div');
      separator.className = 'context-menu-separator';
      menu.appendChild(separator);
    }};
    addItem('Close', `close:${{id}}`);
    addItem('Close Others', `close-others:${{id}}`, tabs.length < 2);
    addItem('Close to the Left', `close-left:${{id}}`, index === 0);
    addItem('Close to the Right', `close-right:${{id}}`, index === tabs.length - 1);
    addSeparator();
    addItem('Reload', `reload-tab:${{id}}`, !tab.path);
    addItem('Export as HTML...', `export-tab-html:${{id}}`);
    menu.classList.remove('hidden');
    const rect = menu.getBoundingClientRect();
    const maxX = window.innerWidth - rect.width - 8;
    const maxY = window.innerHeight - rect.height - 8;
    menu.style.left = `${{Math.max(8, Math.min(x, maxX))}}px`;
    menu.style.top = `${{Math.max(8, Math.min(y, maxY))}}px`;
  }},
  hideTabContextMenu() {{
    document.getElementById('tab-context-menu').classList.add('hidden');
  }}
}};
function unwrapFindMarks(root) {{
  for (const mark of [...root.querySelectorAll('mark.find-hit')]) {{
    mark.replaceWith(document.createTextNode(mark.textContent));
  }}
  root.normalize();
}}
function scrollInside(target, block) {{
  const scroller = document.getElementById('scroll-root');
  const targetRect = target.getBoundingClientRect();
  const scrollerRect = scroller.getBoundingClientRect();
  const offset = targetRect.top - scrollerRect.top + scroller.scrollTop;
  const centered = offset - (scroller.clientHeight / 2) + (targetRect.height / 2);
  scroller.scrollTo({{
    top: block === 'center' ? centered : offset,
    behavior: 'smooth'
  }});
}}
function highlightText(root, query) {{
  const hits = [];
  const needle = query.toLocaleLowerCase();
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT, {{
    acceptNode(node) {{
      if (!node.nodeValue || !node.nodeValue.toLocaleLowerCase().includes(needle)) {{
        return NodeFilter.FILTER_REJECT;
      }}
      const parent = node.parentElement;
      if (!parent || parent.closest('script,style,mark')) {{
        return NodeFilter.FILTER_REJECT;
      }}
      return NodeFilter.FILTER_ACCEPT;
    }}
  }});
  const nodes = [];
  while (walker.nextNode()) nodes.push(walker.currentNode);
  for (const node of nodes) {{
    const text = node.nodeValue;
    const lower = text.toLocaleLowerCase();
    const fragment = document.createDocumentFragment();
    let cursor = 0;
    let index = lower.indexOf(needle);
    while (index !== -1) {{
      fragment.appendChild(document.createTextNode(text.slice(cursor, index)));
      const mark = document.createElement('mark');
      mark.className = 'find-hit';
      mark.textContent = text.slice(index, index + query.length);
      fragment.appendChild(mark);
      hits.push(mark);
      cursor = index + query.length;
      index = lower.indexOf(needle, cursor);
    }}
    fragment.appendChild(document.createTextNode(text.slice(cursor)));
    node.replaceWith(fragment);
  }}
  return hits;
}}
document.getElementById('find-input').addEventListener('input', event => {{
  window.markview.findQuery = event.target.value;
  window.markview.applyFind();
}});
document.getElementById('find-input').addEventListener('keydown', event => {{
  if (event.key === 'Enter') {{
    event.preventDefault();
    event.shiftKey ? window.markview.findPrevious() : window.markview.findNext();
  }}
}});
document.getElementById('find-next').onclick = () => window.markview.findNext();
document.getElementById('find-prev').onclick = () => window.markview.findPrevious();
document.getElementById('recent-files').addEventListener('change', event => {{
  if (event.target.value) {{
    window.ipc.postMessage(`recent:${{event.target.value}}`);
    event.target.value = '';
  }}
}});
document.addEventListener('click', event => {{
  const menu = document.getElementById('tab-context-menu');
  if (!menu.classList.contains('hidden') && !menu.contains(event.target)) {{
    window.markview.hideTabContextMenu();
  }}
}});
document.addEventListener('contextmenu', event => {{
  const menu = document.getElementById('tab-context-menu');
  if (!menu.contains(event.target) && !event.target.closest('.tab')) {{
    window.markview.hideTabContextMenu();
  }}
}});
window.addEventListener('resize', () => window.markview.hideTabContextMenu());
window.addEventListener('blur', () => window.markview.hideTabContextMenu());
window.addEventListener('keydown', event => {{
  if (event.key === 'Escape') {{
    window.markview.hideTabContextMenu();
  }} else if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === 'f') {{
    event.preventDefault();
    document.getElementById('find-input').focus();
  }} else if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === 'o') {{
    event.preventDefault();
    window.ipc.postMessage('open');
  }} else if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === 'p') {{
    event.preventDefault();
    window.ipc.postMessage('print');
  }} else if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === 'r') {{
    event.preventDefault();
    window.ipc.postMessage('refresh');
  }} else if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === 'q') {{
    event.preventDefault();
    window.ipc.postMessage('quit');
  }} else if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === 'w') {{
    if (window.markview.state.activeTabId !== null) {{
      event.preventDefault();
      window.ipc.postMessage(`close:${{window.markview.state.activeTabId}}`);
    }}
  }} else if ((event.metaKey || event.ctrlKey) && event.shiftKey && event.key.toLowerCase() === 'e') {{
    event.preventDefault();
    window.ipc.postMessage('export-html');
  }} else if ((event.metaKey || event.ctrlKey) && !event.shiftKey && event.key.toLowerCase() === 'e') {{
    event.preventDefault();
    window.ipc.postMessage('toggle-edit');
  }} else if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === 's') {{
    event.preventDefault();
    window.ipc.postMessage('save');
  }}
}});
function fileName(path) {{
  return path.split(/[\\/]/).filter(Boolean).pop() || path;
}}
window.markview.setState(window.markview.state);
</script>
</body>
</html>
"#,
        state = view_js(view)
    )
}

fn view_js(view: &AppView) -> String {
    let tabs = view
        .tabs
        .iter()
        .map(|tab| {
            format!(
                "{{id:{},title:{},path:{},stale:{},editing:{},dirty:{}}}",
                tab.id,
                js_string(&tab.title),
                tab.path
                    .as_ref()
                    .map(|path| js_string(path))
                    .unwrap_or_else(|| "null".to_owned()),
                tab.stale,
                tab.editing,
                tab.dirty
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let active_tab_id = view
        .active_tab_id
        .map(|id| id.to_string())
        .unwrap_or_else(|| "null".to_owned());
    let headings = view
        .headings
        .iter()
        .map(|heading| {
            format!(
                "{{level:{},title:{},id:{}}}",
                heading.level,
                js_string(&heading.title),
                js_string(&heading.id)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let recent_files = view
        .preferences
        .recent_files
        .iter()
        .map(|path| js_string(&path.display().to_string()))
        .collect::<Vec<_>>()
        .join(",");

    format!(
        "{{tabs:[{tabs}],activeTabId:{active_tab_id},activeHtml:{},activeSource:{},headings:[{headings}],preferences:{{theme:{},sidebarVisible:{},autoRefresh:{},recentFiles:[{recent_files}]}}}}",
        js_string(&view.active_html),
        js_string(&view.active_source),
        js_string(view.preferences.theme.as_str()),
        view.preferences.sidebar_visible,
        view.preferences.auto_refresh
    )
}

fn js_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('"');
    for ch in value.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            '<' => escaped.push_str("\\u003c"),
            '>' => escaped.push_str("\\u003e"),
            '&' => escaped.push_str("\\u0026"),
            '\u{2028}' => escaped.push_str("\\u2028"),
            '\u{2029}' => escaped.push_str("\\u2029"),
            ch if (ch as u32) < 0x20 => {
                escaped.push_str(&format!("\\u{:04x}", ch as u32));
            }
            _ => escaped.push(ch),
        }
    }
    escaped.push('"');
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn identifies_external_http_links() {
        assert!(is_external_url("https://example.com"));
        assert!(is_external_url("http://example.com"));
        assert!(!is_external_url("file:///tmp/readme.md"));
        assert!(!is_external_url("#intro"));
    }

    #[test]
    fn identifies_markdown_drop_paths() {
        assert!(is_markdown_path(Path::new("README.md")));
        assert!(is_markdown_path(Path::new("guide.MARKDOWN")));
        assert!(is_markdown_path(Path::new("notes.mdown")));
        assert!(!is_markdown_path(Path::new("notes.txt")));
        assert!(!is_markdown_path(Path::new("README")));
    }

    #[test]
    fn app_shell_includes_tab_overflow_helpers() {
        let mut model = AppModel::new();
        model.open_untitled("one", "# One".to_owned());
        model.open_untitled("two", "# Two".to_owned());

        let html = app_shell_html(&app_view_with_preferences(
            &model,
            GuiPreferences::default(),
        ));

        assert!(html.contains("flex: 0 0 190px"));
        assert!(html.contains("tab-count"));
        assert!(html.contains("scrollIntoView"));
        assert!(html.contains("${next.tabs.length} open"));
    }

    #[test]
    fn app_shell_includes_tab_context_menu() {
        let mut model = AppModel::new();
        model.open_untitled("one", "# One".to_owned());

        let html = app_shell_html(&app_view_with_preferences(
            &model,
            GuiPreferences::default(),
        ));

        assert!(html.contains(r#"id="tab-context-menu""#));
        assert!(html.contains("button.oncontextmenu"));
        assert!(html.contains("showTabContextMenu(tab.id, event.clientX, event.clientY)"));
        assert!(html.contains("'Close Others'"));
        assert!(html.contains("'Close to the Left'"));
        assert!(html.contains("'Close to the Right'"));
        assert!(html.contains("'Reload'"));
        assert!(html.contains("'Export as HTML...'"));
        assert!(html.contains("close-others:"));
        assert!(html.contains("close-left:"));
        assert!(html.contains("close-right:"));
        assert!(html.contains("reload-tab:"));
        assert!(html.contains("export-tab-html:"));
    }

    #[test]
    fn app_shell_preserves_scroll_inside_document_pane() {
        let mut model = AppModel::new();
        model.open_untitled("one", "# One".to_owned());

        let html = app_shell_html(&app_view_with_preferences(
            &model,
            GuiPreferences::default(),
        ));

        assert!(html.contains("scrollPositions: new Map()"));
        assert!(html.contains("this.scrollPositions.set(previousId, scroller.scrollTop)"));
        assert!(html.contains("const restoreY = this.scrollPositions.get(next.activeTabId) || 0"));
        assert!(html.contains("scroller.scrollTop = restoreY"));
        assert!(html.contains("document.getElementById('scroll-root')"));
    }

    #[test]
    fn app_shell_tunes_print_theme() {
        let html = app_shell_html(&app_view_with_preferences(
            &AppModel::new(),
            GuiPreferences::default(),
        ));

        assert!(html.contains("@page"));
        assert!(html.contains("a[href^=\"http\"]::after"));
        assert!(html.contains("break-after: avoid"));
        assert!(html.contains(".syntax-keyword, .syntax-comment"));
        assert!(html.contains("input[type=\"checkbox\"]"));
    }

    #[test]
    fn app_shell_includes_native_feeling_shortcuts() {
        let html = app_shell_html(&app_view_with_preferences(
            &AppModel::new(),
            GuiPreferences::default(),
        ));

        assert!(html.contains("event.key.toLowerCase() === 'q'"));
        assert!(html.contains("window.ipc.postMessage('quit')"));
        assert!(html.contains("event.key.toLowerCase() === 'w'"));
        assert!(html.contains("event.key.toLowerCase() === 'r'"));
        assert!(html.contains("window.ipc.postMessage('refresh')"));
        assert!(
            html.contains("window.ipc.postMessage(`close:${window.markview.state.activeTabId}`)")
        );
        assert!(html.contains("event.shiftKey && event.key.toLowerCase() === 'e'"));
        assert!(html.contains("window.ipc.postMessage('export-html')"));
        assert!(html.contains("!event.shiftKey && event.key.toLowerCase() === 'e'"));
        assert!(html.contains("window.ipc.postMessage('toggle-edit')"));
        assert!(html.contains("event.key.toLowerCase() === 's'"));
        assert!(html.contains("window.ipc.postMessage('save')"));
    }

    #[test]
    fn app_shell_includes_edit_and_save_toolbar_buttons() {
        let html = app_shell_html(&app_view_with_preferences(
            &AppModel::new(),
            GuiPreferences::default(),
        ));

        assert!(html.contains(r#"id="edit-toggle""#));
        assert!(html.contains("data-tooltip=\"Toggle edit mode\""));
        assert!(html.contains("onclick=\"window.ipc.postMessage('save')\""));
    }

    #[test]
    fn app_shell_renders_editor_textarea_for_editing_tabs_and_skips_rerender() {
        let mut model = AppModel::new();
        let id = model.open_untitled("draft", "# Draft".to_owned());
        model.toggle_editing(id);

        let html = app_shell_html(&app_view_with_preferences(
            &model,
            GuiPreferences::default(),
        ));

        assert!(html.contains("textarea.className = 'editor'"));
        assert!(html.contains(
            "!existingEditor || existingEditor.dataset.tabId !== String(next.activeTabId)"
        ));
        assert!(
            html.contains("window.ipc.postMessage(`edit:${next.activeTabId}:${textarea.value}`)")
        );
        assert!(html.contains("(tab.dirty ? ' dirty' : '')"));
    }

    #[test]
    fn toolbar_buttons_have_distinct_tooltips() {
        let html = app_shell_html(&app_view_with_preferences(
            &AppModel::new(),
            GuiPreferences::default(),
        ));

        assert!(html.contains(".tool-button[data-tooltip]:hover::after"));
        assert!(html.contains("data-tooltip=\"Refresh active document\""));
        assert!(html.contains("data-tooltip=\"Auto-refresh on file changes\""));
        assert!(html.contains("Disable auto-refresh on file changes"));
        assert!(html.contains("Enable auto-refresh on file changes"));
        assert!(html.contains("<circle cx=\"12\" cy=\"12\" r=\"8\"></circle>"));
    }

    #[test]
    fn stdin_tabs_are_only_created_for_pipe_or_file_stdin() {
        assert!(should_read_stdin(StdinSource::Pipe));
        assert!(should_read_stdin(StdinSource::File));
        assert!(!should_read_stdin(StdinSource::Terminal));
        assert!(!should_read_stdin(StdinSource::Other));
    }

    #[test]
    fn opened_events_only_accept_file_urls() {
        let file_url = url::Url::from_file_path("/tmp/markview.md").expect("file URL");
        let web_url = url::Url::parse("https://example.com/markview.md").expect("web URL");

        assert_eq!(
            opened_url_file_path(&file_url),
            Some(PathBuf::from("/tmp/markview.md"))
        );
        assert_eq!(opened_url_file_path(&web_url), None);
    }

    #[test]
    fn js_string_escapes_json_control_characters() {
        assert_eq!(js_string("bad\x01name\nok"), r#""bad\u0001name\nok""#);
    }

    #[test]
    fn view_js_includes_editing_dirty_and_active_source() {
        let mut model = AppModel::new();
        let id = model.open_untitled("draft", "# Draft".to_owned());
        model.toggle_editing(id);
        model.update_source(id, "# Draft edited".to_owned());

        let script = view_js(&app_view_with_preferences(
            &model,
            GuiPreferences::default(),
        ));

        assert!(script.contains("editing:true"));
        assert!(script.contains("dirty:true"));
        assert!(script.contains("activeSource:\"# Draft edited\""));
    }

    #[test]
    fn classifies_file_events_that_should_refresh() {
        assert!(is_refresh_event(&EventKind::Create(
            notify::event::CreateKind::Any
        )));
        assert!(is_refresh_event(&EventKind::Modify(
            notify::event::ModifyKind::Any
        )));
        assert!(is_refresh_event(&EventKind::Remove(
            notify::event::RemoveKind::Any
        )));
        assert!(!is_refresh_event(&EventKind::Access(
            notify::event::AccessKind::Any
        )));
        assert!(!is_refresh_event(&EventKind::Other));
    }

    #[test]
    fn detects_dirty_tabs_outside_a_kept_tab() {
        let mut model = AppModel::new();
        let kept = model.open_untitled("kept", "# Kept".to_owned());
        let other = model.open_untitled("other", "# Other".to_owned());

        assert!(!other_tabs_dirty(&model, kept));

        model.toggle_editing(other);
        model.update_source(other, "# Other edited".to_owned());

        assert!(other_tabs_dirty(&model, kept));
        assert!(!other_tabs_dirty(&model, other));
        assert!(!other_tabs_dirty(&model, 99));
    }

    #[test]
    fn detects_dirty_tabs_to_the_left_and_right() {
        let mut model = AppModel::new();
        let left = model.open_untitled("left", "# Left".to_owned());
        let middle = model.open_untitled("middle", "# Middle".to_owned());
        let right = model.open_untitled("right", "# Right".to_owned());

        assert!(!tabs_to_left_dirty(&model, middle));
        assert!(!tabs_to_right_dirty(&model, middle));

        model.toggle_editing(left);
        model.update_source(left, "# Left edited".to_owned());
        assert!(tabs_to_left_dirty(&model, middle));
        assert!(!tabs_to_right_dirty(&model, middle));

        model.toggle_editing(right);
        model.update_source(right, "# Right edited".to_owned());
        assert!(tabs_to_right_dirty(&model, middle));
    }

    #[test]
    fn detects_any_dirty_tab() {
        let mut model = AppModel::new();
        let id = model.open_untitled("draft", "# Draft".to_owned());

        assert!(!any_tab_dirty(&model));

        model.toggle_editing(id);
        model.update_source(id, "# Draft edited".to_owned());

        assert!(any_tab_dirty(&model));
    }
}
