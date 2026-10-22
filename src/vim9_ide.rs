//! Real Vim with an independent GUI buffer surface and a typed IDE workbench.
//!
//! Standalone IDE startup never takes the terminal instance lock, restores
//! shell tabs, or persists editor argv. Managed shell entry embeds the same
//! editor in its source pane. Bridge disconnection is not a child exit.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use iced::advanced::input_method;
use iced::advanced::widget::{self, operation, Id};
use iced::widget::{
    button, checkbox, column, container, mouse_area, row, scrollable, stack, text, text_input,
    tooltip, Space,
};
use iced::{keyboard, Element, Length, Size, Subscription, Task};
use jterm_core::pty_input::{self, PasteModes, PastePolicy, UnbracketedMultiline};
use jterm_core::vim9::{
    self, parse_vim9_location as goto_position, ProjectEntry, Vim9Command, Vim9EntryRequest,
    Vim9ProjectFile, Vim9ProjectIndex, Vim9ProjectSnapshot, Vim9SearchCase, Vim9SearchOptions,
    Vim9Session, Vim9Snapshot,
};

use super::{config, native_enter, terminal, terminal_ime, terminal_view};
use super::{
    Config, KeyboardEnhancements, Metrics, MouseButton, MouseInput, Session, TermWidget, Theme,
};

const HEADER: f32 = 38.0;
const TOOLS: f32 = 36.0;
const BUFFERS: f32 = 34.0;
const STATUS: f32 = 28.0;
const SIDEBAR: f32 = 250.0;
const DOCK: f32 = 200.0;
const INPUT_ID: &str = "frost-vim9-input";
const MAX_INPUT: usize = 16 * 1024;
const MAX_QUICK_QUERY: usize = 256;
const QUICK_MATCH_LIMIT: usize = 80;
const QUICK_VISIBLE_ROWS: usize = 8;

struct RenderSubscription {
    session_id: usize,
    interval_ms: u64,
    editor: Vim9Session,
}

impl std::hash::Hash for RenderSubscription {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::hash::Hash::hash(&self.session_id, state);
        std::hash::Hash::hash(&self.editor.entry_id(), state);
        std::hash::Hash::hash(&self.interval_ms, state);
    }
}

fn render_stream(key: &RenderSubscription) -> impl iced::futures::Stream<Item = Message> + use<> {
    use iced::futures::SinkExt;
    let editor = key.editor.clone();
    let interval = Duration::from_millis(key.interval_ms);
    iced::stream::channel(
        1,
        move |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
            let mut previous = None;
            loop {
                let frame = editor.render_snapshot();
                let revision = frame.as_ref().map(|frame| frame.revision);
                if revision != previous {
                    previous = revision;
                    if output
                        .send(Message::RenderFrame {
                            entry_id: editor.entry_id(),
                            frame,
                        })
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                tokio::time::sleep(interval).await;
            }
        },
    )
}

/// Preserve the pre-existing no-IDE launch path. Once --ide is selected, reject
/// extra options instead of quietly running a different command or project.
fn target_from_args(args: &[OsString]) -> Result<Option<PathBuf>, String> {
    if args.first().and_then(|s| s.to_str()) != Some("--ide") {
        if args.iter().any(|s| s == "--ide") {
            return Err("Use frost --ide [DIRECTORY|FILE], with --ide first".into());
        }
        return Ok(None);
    }
    let rest = &args[1..];
    let rest = if rest.first().is_some_and(|s| s == "--") {
        &rest[1..]
    } else {
        if rest
            .first()
            .is_some_and(|s| s.to_string_lossy().starts_with('-'))
        {
            return Err("Use frost --ide -- PATH for a filename beginning with '-'".into());
        }
        rest
    };
    if rest.len() > 1 {
        return Err("Vim9 IDE accepts one project directory or file".into());
    }
    Ok(Some(
        rest.first()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(".")),
    ))
}

pub(super) fn run_if_requested() -> Option<iced::Result> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if let Some(result) = vim9::forward_shell_vim(&args) {
        let status = result.unwrap_or_else(|error| {
            eprintln!("frost: {error}");
            1
        });
        std::process::exit(status);
    }
    let entry_request = match vim9::shell_vim_request(&args).and_then(|request| match request {
        Some(request) => Ok(Some(request)),
        None => target_from_args(&args).map(|target| target.map(Vim9EntryRequest::command_line)),
    }) {
        Ok(Some(request)) => request,
        Ok(None) => return None,
        Err(error) => {
            eprintln!("frost: {error}");
            std::process::exit(2);
        }
    };
    // A Vim invoked from a real shell must finish with Vim's actual status, so
    // `vim file && build` and `:cq` keep their normal command-line semantics.
    let shell_status = entry_request
        .is_shell_command()
        .then(|| std::sync::Arc::new(std::sync::atomic::AtomicI32::new(1)));
    let application_status = shell_status.clone();
    let loaded = Config::load_with_diagnostics();
    let mut config = loaded.config;
    // These changes are process-local. There is no config writer in this app.
    config.block_mode = false;
    config.terminal_mode = Some(config::TerminalMode::Normal);
    config.disable_alt_screen = false;
    config.ascii_organism_enabled = false;
    let native = native_enter::install();
    let size = Size::new(
        config.initial_width.max(1000.0),
        config.initial_height.max(700.0),
    );
    let window = iced::window::Settings {
        size,
        min_size: Some(Size::new(480.0, 320.0)),
        exit_on_close_request: false,
        // A static ASCII window title avoids platform decoration font fallback
        // problems; real filenames are drawn by Iced in the workbench itself.
        decorations: true,
        platform_specific: iced::window::settings::PlatformSpecific {
            application_id: super::WINDOW_APP_ID.into(),
            ..Default::default()
        },
        icon: iced::window::icon::from_file_data(
            super::WINDOW_ICON_PNG,
            Some(image::ImageFormat::Png),
        )
        .ok(),
        ..Default::default()
    };
    let entry = std::sync::Mutex::new(Some(entry_request));
    let result = iced::application(
        move || {
            Ide::new(
                entry
                    .lock()
                    .expect("IDE launch lock")
                    .take()
                    .expect("explicit IDE launch is consumed once"),
                config.clone(),
                loaded.diagnostic.clone(),
                native.clone(),
                size,
                application_status.clone(),
            )
        },
        Ide::update,
        Ide::view,
    )
    .title("Frost - Vim9 IDE")
    .theme(Ide::native_theme)
    .subscription(Ide::subscription)
    .antialiasing(false)
    .window(window)
    .run();
    if result.is_ok() {
        if let Some(status) = shell_status {
            std::process::exit(status.load(std::sync::atomic::Ordering::Relaxed));
        }
    }
    Some(result)
}

/// The ordinary terminal's launcher never injects a command into its shell.
/// Reap the separate process without tying its lifetime to a terminal pane.
pub(super) fn entry_from_terminal(app: &mut super::Frost) -> Option<Vim9EntryRequest> {
    let target = app.sessions.get(app.active).and_then(|session| {
        if session.managed_remote
            || session.observed_ssh_command().is_some()
            || matches!(
                session.fg_proc_cache.as_deref(),
                Some("ssh" | "mosh" | "telnet")
            )
        {
            return None;
        }
        session
            .cwd()
            .map(PathBuf::from)
            .filter(|path| path.is_dir())
    });
    let Some(target) = target else {
        app.push_toast(
            "Select a local terminal, or run frost --ide /path/to/project",
            super::ToastKind::Warning,
        );
        return None;
    };
    Some(Vim9EntryRequest::user_action(target))
}

pub(super) fn open_from_terminal(app: &mut super::Frost, request: Vim9EntryRequest) {
    let spawn = std::env::current_exe().and_then(|executable| {
        std::process::Command::new(executable)
            .arg("--ide")
            .arg(request.target())
            .current_dir(request.target())
            .spawn()
    });
    match spawn {
        Ok(mut child) => {
            let _ = std::thread::Builder::new()
                .name("vim9-window-reaper".into())
                .spawn(move || {
                    let _ = child.wait();
                });
        }
        Err(error) => app.push_toast(
            format!("Cannot open Vim9 IDE: {error}"),
            super::ToastKind::Warning,
        ),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FormKind {
    QuickOpen,
    Goto,
    Open,
    Search,
    Build,
    SaveAndBuild,
    Rename,
}
impl FormKind {
    fn label(self) -> &'static str {
        match self {
            Self::QuickOpen => "Quick open",
            Self::Goto => "Go to line",
            Self::Open => "Open file",
            Self::Search => "Search project",
            Self::Build => "Build / test",
            Self::SaveAndBuild => "Save all and build",
            Self::Rename => "Rename symbol",
        }
    }
    fn help(self) -> &'static str {
        match self {
            Self::QuickOpen => "Find file[:line[:column]], or :line[:column] in the current buffer. Columns are Vim byte columns. ↑ / ↓ selects; Enter opens; Esc returns to Vim.",
            Self::Goto => "Enter a line or line:column. The column is a Vim byte column. The current buffer stays open.",
            Self::Open => "Enter a local filename. New files require an existing parent directory. Vim retains its normal unsaved-buffer checks.",
            Self::Search => "Search project text using ripgrep. Choose literal or regular expression matching and an optional file glob. Results replace Vim's quickfix list.",
            Self::Build => "Review this shell command before running it. Empty uses Vim's makeprg. Nothing is saved or executed automatically.",
            Self::SaveAndBuild => "Save all buffers, then run this reviewed build command. If saving fails, the build will not start.",
            Self::Rename => "Requires a configured Vim9 LSP provider. Enter the new symbol name.",
        }
    }
}
#[derive(Debug, Clone)]
struct Form {
    kind: FormKind,
    value: String,
    selected: usize,
    error: Option<String>,
}

#[derive(Debug, Clone)]
pub(super) enum Message {
    Terminal(Box<super::Message>),
    Tick,
    ScrollFrame,
    RenderFrame {
        entry_id: uuid::Uuid,
        frame: Option<std::sync::Arc<vim9::Vim9RenderFrame>>,
    },
    Drain,
    Keyboard(iced::window::Id, keyboard::Event, bool),
    Modifiers(keyboard::Modifiers),
    Ime(input_method::Event),
    CancelledCommit,
    Focus(bool),
    Resize(Size),
    WindowResize(Size),
    Close,
    NativeWheel(crate::vim9_buffer::NativeWheel),
    NativeMouse(crate::vim9_buffer::NativeInput),
    Command(Vim9Command),
    ShowForm(FormKind),
    FormInput(String),
    SearchCase(Vim9SearchCase),
    SearchRegex(bool),
    SearchWholeWord(bool),
    SearchGlob(String),
    SubmitForm,
    CancelForm,
    RefreshIndex,
    QuickOpenFile {
        path: PathBuf,
        query: String,
    },
    MoveQuickSelection(isize),
    Browse(PathBuf),
    Directory(u64, Result<(PathBuf, Vec<ProjectEntry>), String>),
    OpenFile(PathBuf),
    Pasted(u64, Option<String>),
    ToggleFiles,
    ToggleDock,
    OutputTab(bool),
    DismissError,
}

pub(super) struct Ide {
    embedded: bool,
    completion: Option<vim9::Vim9ShellCompletion>,
    config: Config,
    theme: Theme,
    metrics: Metrics,
    mono: iced::Font,
    cjk: Option<iced::Font>,
    symbol: Option<iced::Font>,
    math: Option<iced::Font>,
    nerd: Option<iced::Font>,
    session: Option<Session>,
    editor: Option<Vim9Session>,
    snapshot: Vim9Snapshot,
    render_frame: Option<std::sync::Arc<vim9::Vim9RenderFrame>>,
    render_subscribed: bool,
    scroll: std::cell::RefCell<crate::vim9_buffer::ScrollState>,
    root: PathBuf,
    directory: PathBuf,
    entries: Vec<ProjectEntry>,
    scan_busy: bool,
    scan_epoch: u64,
    queued_scan: Option<(u64, PathBuf)>,
    files_open: bool,
    dock_open: bool,
    output_tab: bool,
    form: Option<Form>,
    project_index: Option<Vim9ProjectIndex>,
    project: Vim9ProjectSnapshot,
    quick_matches: Vec<Vim9ProjectFile>,
    build_command: String,
    search_query: String,
    search_options: Vim9SearchOptions,
    error: Option<String>,
    size: Size,
    focused: bool,
    modifiers: keyboard::Modifiers,
    native_enter: native_enter::SharedOwnership,
    ime: terminal_ime::Ownership,
    input_epoch: u64,
    gestures: [Option<super::TerminalMouseGesture>; 3],
    blink_started: Instant,
    blink_on: bool,
    child_exited: bool,
    shell_status: Option<std::sync::Arc<std::sync::atomic::AtomicI32>>,
}

fn accepts_render_entry(active: Option<uuid::Uuid>, incoming: uuid::Uuid, exited: bool) -> bool {
    !exited && active == Some(incoming)
}

impl Drop for Ide {
    fn drop(&mut self) {
        if let Some(editor) = &self.editor {
            editor.revoke();
        }
    }
}

fn chrome(
    size: Size,
    presentation: &vim9::Vim9UiConfig,
    files_open: bool,
    dock_open: bool,
) -> (f32, f32) {
    let sidebar = if files_open && size.width >= 820.0 {
        SIDEBAR
    } else {
        0.0
    };
    let dock = if dock_open && size.height >= 580.0 {
        DOCK
    } else {
        0.0
    };
    let vertical = if presentation.toolbar {
        HEADER + TOOLS
    } else {
        0.0
    } + if presentation.buffer_tabs {
        BUFFERS
    } else {
        0.0
    } + if presentation.status_bar { STATUS } else { 0.0 }
        + dock;
    (sidebar, vertical)
}

fn visible_label(value: &str) -> String {
    let mut result = String::new();
    for character in value.chars().take(240) {
        if character.is_control()
            || matches!(character, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{206f}' | '\u{feff}')
        {
            result.push_str(&format!("\\u{{{:x}}}", character as u32));
        } else {
            result.push(character);
        }
    }
    if value.chars().count() > 240 {
        result.push('…');
    }
    result
}

fn bounded_input(value: String) -> String {
    bounded_text(value, MAX_INPUT)
}

/// Quick Open shares the four-terminal character limit, independently of
/// the byte budget used by command/search forms.
fn bounded_quick_query(mut value: String) -> String {
    if let Some((end, _)) = value.char_indices().nth(MAX_QUICK_QUERY) {
        value.truncate(end);
    }
    value
}

fn search_options_summary(options: &Vim9SearchOptions) -> String {
    let matching = if options.regex { "Regex" } else { "Literal" };
    let case = match options.case {
        Vim9SearchCase::Smart => "Smart case",
        Vim9SearchCase::Sensitive => "Match case",
        Vim9SearchCase::Ignore => "Ignore case",
    };
    format!(
        "{matching} · {case}{} · Files: {}",
        if options.whole_word {
            " · Whole word"
        } else {
            ""
        },
        if options.glob.is_empty() {
            "all eligible files".to_owned()
        } else {
            visible_label(&options.glob)
        }
    )
}

fn bounded_text(mut value: String, limit: usize) -> String {
    if value.len() > limit {
        let mut end = limit;
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        value.truncate(end);
    }
    value
}

/// Revalidate the current query and selected path before either keyboard or
/// pointer confirmation. A previous list must never supply a stale target.
fn quick_open_command(
    project: &Vim9ProjectSnapshot,
    value: &str,
    selected_path: Option<&Path>,
) -> Result<Vim9Command, &'static str> {
    let query = project.quick_open_query(value)?;
    if query.current_buffer {
        return query.command(None);
    }
    let matches = project.matches(&query.file_query, QUICK_MATCH_LIMIT);
    let file = matches
        .iter()
        .find(|file| Some(file.path.as_path()) == selected_path);
    query.command(file)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shortcut {
    Form(FormKind),
    Save,
    SaveAll,
    NextBuffer,
    PreviousBuffer,
    CloseCurrentBuffer,
    NextProblem,
    PreviousProblem,
    NavigateBack,
    NavigateForward,
    ToggleFiles,
    ToggleDock,
}

fn native_shortcut(
    key: &keyboard::Key,
    modifiers: keyboard::Modifiers,
    tool_panel_visible: bool,
) -> Option<Shortcut> {
    use keyboard::key::Named;
    if modifiers.alt() && !modifiers.control() && !modifiers.shift() && !modifiers.logo() {
        return match key {
            keyboard::Key::Named(Named::ArrowLeft) => Some(Shortcut::NavigateBack),
            keyboard::Key::Named(Named::ArrowRight) => Some(Shortcut::NavigateForward),
            _ => None,
        };
    }
    if modifiers.alt() || modifiers.logo() {
        return None;
    }
    if tool_panel_visible && *key == keyboard::Key::Named(Named::F8) && !modifiers.control() {
        return Some(if modifiers.shift() {
            Shortcut::PreviousProblem
        } else {
            Shortcut::NextProblem
        });
    }
    if !modifiers.control() {
        return None;
    }
    if !modifiers.shift() {
        return matches!(key, keyboard::Key::Character(value) if value.eq_ignore_ascii_case("s"))
            .then_some(Shortcut::Save);
    }
    match key {
        keyboard::Key::Named(Named::PageUp) => Some(Shortcut::PreviousBuffer),
        keyboard::Key::Named(Named::PageDown) => Some(Shortcut::NextBuffer),
        keyboard::Key::Character(value) => match value.to_ascii_lowercase().as_str() {
            "p" => Some(Shortcut::Form(FormKind::QuickOpen)),
            "g" => Some(Shortcut::Form(FormKind::Goto)),
            "o" => Some(Shortcut::Form(FormKind::Open)),
            "f" => Some(Shortcut::Form(FormKind::Search)),
            "b" => Some(Shortcut::Form(FormKind::Build)),
            "s" => Some(Shortcut::SaveAll),
            "w" => Some(Shortcut::CloseCurrentBuffer),
            "e" => Some(Shortcut::ToggleFiles),
            "j" => Some(Shortcut::ToggleDock),
            _ => None,
        },
        _ => None,
    }
}

fn moved_selection(current: usize, delta: isize, count: usize) -> usize {
    if count == 0 {
        return 0;
    }
    current.saturating_add_signed(delta).min(count - 1)
}

fn quick_window(selected: usize, count: usize, rows: usize) -> std::ops::Range<usize> {
    let rows = rows.max(1);
    let start = selected
        .saturating_sub(rows / 2)
        .min(count.saturating_sub(rows));
    start..(start + rows).min(count)
}

fn editor_session(editor: &Vim9Session, config: &Config, clean: bool) -> anyhow::Result<Session> {
    let launch = editor.launch();
    let mut argv = vec![launch.program.clone()];
    if clean {
        argv.extend(["-u", "NONE", "-i", "NONE", "-n"].map(str::to_owned));
    }
    argv.extend(launch.args.iter().cloned());
    let env: Vec<_> = launch
        .env
        .iter()
        .filter_map(|entry| entry.split_once('='))
        .collect();
    Session::spawn_argv_env(
        config,
        1,
        100,
        30,
        editor.root().to_str(),
        Some(&argv),
        &env,
    )
}

impl Ide {
    pub(super) fn embedded(
        invocation: vim9::Vim9ShellInvocation,
        mut config: Config,
        native: native_enter::SharedOwnership,
        size: Size,
    ) -> (Self, Task<Message>) {
        config.block_mode = false;
        config.terminal_mode = Some(config::TerminalMode::Normal);
        config.disable_alt_screen = false;
        config.ascii_organism_enabled = false;
        let (mut editor, task) = Self::new(invocation.request, config, None, native, size, None);
        editor.embedded = true;
        editor.completion = Some(invocation.completion);
        if editor.session.is_none() {
            editor.child_exited = true;
            editor.completion.take().unwrap().finish(1);
        }
        (editor, task)
    }

    pub(super) fn finished(&self) -> bool {
        self.child_exited
    }

    pub(super) fn focus_embedded(&mut self, focused: bool) {
        if focused != self.focused {
            self.focused = focused;
            self.retire_gestures();
            self.scroll.borrow_mut().clear();
            self.invalidate_input();
            self.sync_ime();
        }
    }

    pub(super) fn embedded_view(&self) -> Element<'_, Message> {
        iced::widget::sensor(self.view())
            .on_show(Message::Resize)
            .on_resize(Message::Resize)
            .into()
    }

    fn new(
        request: Vim9EntryRequest,
        config: Config,
        diagnostic: Option<String>,
        native_enter: native_enter::SharedOwnership,
        size: Size,
        shell_status: Option<std::sync::Arc<std::sync::atomic::AtomicI32>>,
    ) -> (Self, Task<Message>) {
        let theme = Theme::get_theme(&config.theme).unwrap_or_default();
        let mono = crate::terminal_view::resolve_native_editor_font(super::resolve_mono_font(
            &config.font_family,
        ));
        let metrics = Metrics::with_font(
            mono,
            Config::clamp_font_size(config.font_size),
            config.line_spacing,
            config.padding,
            false,
        );
        let mut error = diagnostic;
        let mut session = None;
        let mut editor = None;
        let mut root = request.target().to_owned();
        match Vim9Session::start(request) {
            Ok(controller) => {
                root = controller.root().to_owned();
                match editor_session(&controller, &config, false) {
                    Ok(mut terminal) => {
                        super::apply_theme_colors(&mut terminal.terminal, &theme);
                        terminal.refresh();
                        session = Some(terminal);
                        editor = Some(controller);
                    }
                    Err(failure) => error = Some(failure.to_string()),
                }
            }
            Err(failure) => error = Some(failure.to_string()),
        }
        let build_command = if root.join("Cargo.toml").is_file() {
            "cargo check --message-format=short".into()
        } else {
            String::new()
        };
        let files_open = config.vim9.project_panel;
        let dock_open = config.vim9.tool_panel;
        let mut state = Self {
            embedded: false,
            completion: None,
            config,
            theme,
            metrics,
            mono,
            cjk: super::resolve_optional_font(Config::cjk_monospace_font_family()),
            symbol: super::resolve_optional_font(Config::symbol_monospace_font_family()),
            math: super::resolve_optional_font(Config::math_symbol_font_family()),
            nerd: super::resolve_optional_font(Config::nerd_symbol_font_family()),
            session,
            editor,
            snapshot: Vim9Snapshot::default(),
            render_frame: None,
            render_subscribed: false,
            scroll: std::cell::RefCell::new(crate::vim9_buffer::ScrollState::default()),
            directory: root.clone(),
            root: root.clone(),
            entries: Vec::new(),
            scan_busy: false,
            scan_epoch: 0,
            queued_scan: None,
            files_open,
            dock_open,
            output_tab: false,
            form: None,
            project_index: None,
            project: Vim9ProjectSnapshot::default(),
            quick_matches: Vec::new(),
            build_command,
            search_query: String::new(),
            search_options: Vim9SearchOptions::default(),
            error,
            size,
            focused: true,
            modifiers: keyboard::Modifiers::default(),
            native_enter,
            ime: terminal_ime::Ownership::default(),
            input_epoch: 0,
            gestures: [None; 3],
            blink_started: Instant::now(),
            blink_on: true,
            child_exited: false,
            shell_status,
        };
        state.resize();
        state.sync_ime();
        let task = if state.session.is_none() && state.shell_status.is_some() {
            if let Some(error) = &state.error {
                eprintln!("frost: {error}");
            }
            iced::exit()
        } else if state.session.is_some() && state.files_open {
            state.browse(root)
        } else {
            Task::none()
        };
        (state, task)
    }

    fn native_theme(&self) -> iced::Theme {
        let [r, g, b] = self.theme.terminal.background;
        if u32::from(r) + u32::from(g) + u32::from(b) > 384 {
            iced::Theme::Light
        } else {
            iced::Theme::Dark
        }
    }
    fn input_active(&self) -> bool {
        self.focused && self.form.is_none() && !self.child_exited && self.session.is_some()
    }
    fn invalidate_input(&mut self) {
        self.input_epoch = self.input_epoch.wrapping_add(1);
    }
    fn sync_ime(&mut self) {
        let owner = self
            .input_active()
            .then_some(self.session.as_ref())
            .flatten()
            .map(|s| terminal_ime::Owner {
                session: s.id,
                instance: s.ime_identity.clone(),
            });
        if self.ime.sync(owner) {
            if let Some(s) = &mut self.session {
                s.terminal.preedit_text.clear();
                s.terminal.preedit_selection = None;
            }
        }
    }
    fn retire_gestures(&mut self) {
        for gesture in &mut self.gestures {
            if let Some(previous) = gesture.take().filter(|g| g.report_to_app) {
                if let (Some(s), Some((col, row))) = (&mut self.session, previous.last_app_cell) {
                    if let Some(bytes) = s.terminal.get_mouse_release_report(
                        super::btn_code(previous.button),
                        col,
                        row,
                    ) {
                        s.write_pty(&bytes);
                    }
                }
            }
        }
    }
    fn resize(&mut self) {
        self.scroll.borrow_mut().clear();
        self.retire_gestures();
        let (sidebar, vertical) = chrome(
            self.size,
            &self.config.vim9,
            self.files_open,
            self.dock_open,
        );
        let (cols, rows) = self.metrics.grid_size(
            (self.size.width - sidebar - terminal_view::SCROLLBAR_WIDTH).max(1.0),
            (self.size.height - vertical).max(1.0),
        );
        if let Some(s) = &mut self.session {
            s.terminal.set_viewport_pixel_size(
                (cols as f32 * self.metrics.cell_w).round() as u32,
                (rows as f32 * self.metrics.cell_h).round() as u32,
            );
            if s.terminal.get_dimensions() != (cols, rows) {
                s.terminal.on_resize(cols, rows);
                if let Err(error) = s.pty.resize(cols, rows) {
                    self.error = Some(format!("Cannot resize Vim: {error}"));
                }
            }
            s.refresh();
        }
    }
    fn send(&mut self, command: Vim9Command) {
        self.scroll.borrow_mut().clear();
        self.invalidate_input();
        self.error = None;
        let result = self
            .editor
            .as_ref()
            .ok_or_else(|| "Vim did not start".to_string())
            .and_then(|editor| editor.send(command).map(|_| ()).map_err(|e| e.to_string()));
        if let Err(error) = result {
            self.error = Some(error);
        }
    }
    fn native_frame(&self) -> Option<&vim9::Vim9RenderFrame> {
        let session = self.session.as_ref()?;
        self.render_frame.as_deref().filter(|frame| {
            self.config.vim9.native_buffer
                && self.snapshot.connected
                && session.projection.scroll_offset() == 0
                && !session.terminal.has_text_selection()
                && crate::vim9_buffer::usable(
                    frame,
                    session.projection.cells().len(),
                    session.projection.cells().first().map_or(0, Vec::len),
                )
        })
    }

    fn refresh_native_input(&mut self) {
        // Frame acknowledgement and request completion must come from the
        // same locked bridge observation; independent reads can lose input.
        let observed = self
            .editor
            .as_ref()
            .map(Vim9Session::input_snapshot)
            .unwrap_or_default();
        if observed.frame.is_none()
            || observed.frame.as_ref().is_some_and(|new| {
                self.render_frame
                    .as_ref()
                    .is_none_or(|old| new.revision >= old.revision)
            })
        {
            self.render_frame = observed.frame.clone();
        }
        let enabled = self.input_active()
            && self
                .session
                .as_ref()
                .is_some_and(|s| s.terminal.preedit_text.is_empty());
        self.scroll.borrow_mut().observe(
            self.native_frame(),
            crate::vim9_buffer::local_metrics(self.metrics),
            enabled,
            |id| observed.command_pending(id),
        );
    }

    fn flush_native_input(&mut self) {
        self.refresh_native_input();
        let Some(editor) = self.editor.as_ref() else {
            return;
        };
        let commands = if self
            .native_frame()
            .is_some_and(|frame| matches!(frame.mode.as_str(), "n" | "i"))
        {
            self.scroll.borrow().scroll_commands()
        } else {
            Vec::new()
        };
        for (window, command) in commands {
            match editor.send(command) {
                Ok(id) => self.scroll.borrow_mut().scroll_submitted(window, id),
                Err(error) => {
                    self.scroll.borrow_mut().scroll_failed(window);
                    self.error = Some(error.to_string());
                }
            }
        }
        let command = self.scroll.borrow().pointer_command();
        if let Some(command) = command {
            match editor.send(command) {
                Ok(id) => self.scroll.borrow_mut().pointer_submitted(id),
                Err(error) => {
                    self.scroll.borrow_mut().pointer_failed();
                    self.error = Some(error.to_string());
                }
            }
        }
    }

    pub(super) fn close(&mut self) -> Task<Message> {
        if self.session.is_none() || self.child_exited {
            if let Some(editor) = self.editor.take() {
                editor.revoke();
            }
            self.render_frame = None;
            return if self.embedded {
                Task::none()
            } else {
                iced::exit()
            };
        }
        self.send(Vim9Command::Quit);
        if !self.snapshot.connected {
            self.error = Some("The bridge is disconnected. Vim is still running: save and use :qa inside the editor. This window will not force-kill it.".into());
        }
        Task::none()
    }
    fn form(&mut self, kind: FormKind) -> Task<Message> {
        self.scroll.borrow_mut().clear();
        self.retire_gestures();
        self.invalidate_input();
        self.native_enter.lock().claim_pressed();
        let value = match kind {
            FormKind::QuickOpen => String::new(),
            FormKind::Goto => self.snapshot.line.max(1).to_string(),
            FormKind::Open => format!("{}/", self.directory.display()),
            FormKind::Search => self.search_query.clone(),
            FormKind::Build | FormKind::SaveAndBuild => self.build_command.clone(),
            FormKind::Rename => String::new(),
        };
        self.form = Some(Form {
            kind,
            value,
            selected: 0,
            error: None,
        });
        if kind == FormKind::QuickOpen {
            if self.project_index.is_none() {
                self.refresh_index();
            }
            self.refresh_quick_matches(false);
        }
        widget::operate(operation::focusable::focus(Id::new(INPUT_ID)))
    }
    fn search_form_open(&self) -> bool {
        self.form
            .as_ref()
            .is_some_and(|form| form.kind == FormKind::Search)
    }
    fn submit(&mut self) {
        let Some(form) = self.form.take() else {
            return;
        };
        // Ownership is physical, not a timer or an artificial Escape sequence.
        self.native_enter.lock().claim_pressed();
        self.invalidate_input();
        let action = match form.kind {
            FormKind::QuickOpen => {
                let selected = self
                    .quick_matches
                    .get(form.selected)
                    .map(|file| file.path.as_path());
                match quick_open_command(&self.project, &form.value, selected) {
                    Ok(command) => command,
                    Err(error) => {
                        let mut form = form;
                        form.error = Some(error.into());
                        self.form = Some(form);
                        return;
                    }
                }
            }
            FormKind::Goto => match goto_position(&form.value) {
                Ok((line, column)) => Vim9Command::Goto { line, column },
                Err(error) => {
                    let mut form = form;
                    form.error = Some(error.into());
                    self.form = Some(form);
                    return;
                }
            },
            FormKind::Open => {
                if form.value.is_empty() || form.value.contains(['\r', '\n', '\0']) {
                    let mut form = form;
                    form.error = Some("Enter a single-line filename".into());
                    self.form = Some(form);
                    return;
                }
                let path = PathBuf::from(&form.value);
                let path = if path.is_absolute() {
                    path
                } else {
                    self.directory.join(path)
                };
                Vim9Command::Open {
                    path: path.to_string_lossy().into_owned(),
                    line: None,
                    column: None,
                }
            }
            FormKind::Search => {
                self.search_query.clone_from(&form.value);
                self.output_tab = false;
                Vim9Command::SearchWithOptions {
                    query: form.value,
                    options: self.search_options.clone(),
                }
            }
            FormKind::Build | FormKind::SaveAndBuild => {
                self.build_command.clone_from(&form.value);
                self.output_tab = true;
                if form.kind == FormKind::SaveAndBuild {
                    Vim9Command::SaveAndBuild {
                        command: form.value,
                    }
                } else {
                    Vim9Command::Build {
                        command: form.value,
                    }
                }
            }
            FormKind::Rename => Vim9Command::Rename { name: form.value },
        };
        if matches!(
            action,
            Vim9Command::Search { .. }
                | Vim9Command::SearchWithOptions { .. }
                | Vim9Command::Build { .. }
                | Vim9Command::SaveAndBuild { .. }
        ) {
            self.dock_open = true;
            self.resize();
        }
        self.send(action);
    }

    fn refresh_index(&mut self) {
        self.project_index = Some(Vim9ProjectIndex::start(&self.root));
        if let Some(index) = &self.project_index {
            self.project = index.snapshot();
        }
        self.refresh_quick_matches(false);
    }

    fn refresh_quick_matches(&mut self, preserve_selection: bool) {
        let Some(form) = self
            .form
            .as_mut()
            .filter(|form| form.kind == FormKind::QuickOpen)
        else {
            return;
        };
        let selected_path = preserve_selection
            .then(|| {
                self.quick_matches
                    .get(form.selected)
                    .map(|file| file.path.clone())
            })
            .flatten();
        match self.project.quick_open_query(&form.value) {
            Ok(query) => {
                self.quick_matches = if query.current_buffer {
                    Vec::new()
                } else {
                    self.project.matches(&query.file_query, QUICK_MATCH_LIMIT)
                };
                form.error = None;
            }
            Err(error) => {
                self.quick_matches.clear();
                form.error = Some(error.into());
            }
        }
        form.selected = selected_path
            .and_then(|path| self.quick_matches.iter().position(|file| file.path == path))
            .unwrap_or(0);
    }

    fn move_quick_selection(&mut self, delta: isize) {
        if let Some(form) = self
            .form
            .as_mut()
            .filter(|form| form.kind == FormKind::QuickOpen)
        {
            form.selected = moved_selection(form.selected, delta, self.quick_matches.len());
            form.error = self
                .project
                .quick_open_query(&form.value)
                .err()
                .map(str::to_owned);
        }
    }

    fn shortcut(&mut self, shortcut: Shortcut) -> Task<Message> {
        let command = match shortcut {
            Shortcut::Form(kind) => return self.form(kind),
            Shortcut::Save => Vim9Command::Save,
            Shortcut::SaveAll => Vim9Command::SaveAll,
            Shortcut::NextBuffer => Vim9Command::NextBuffer,
            Shortcut::PreviousBuffer => Vim9Command::PreviousBuffer,
            // Resolve current inside Vim, after any earlier queued navigation.
            // The UI snapshot can still identify the previous buffer here.
            Shortcut::CloseCurrentBuffer => Vim9Command::CloseCurrentBuffer,
            Shortcut::NextProblem => Vim9Command::NextDiagnostic,
            Shortcut::PreviousProblem => Vim9Command::PreviousDiagnostic,
            Shortcut::NavigateBack => Vim9Command::NavigateBack,
            Shortcut::NavigateForward => Vim9Command::NavigateForward,
            Shortcut::ToggleFiles => return self.reduce(Message::ToggleFiles),
            Shortcut::ToggleDock => return self.reduce(Message::ToggleDock),
        };
        self.send(command);
        Task::none()
    }

    fn browse(&mut self, path: PathBuf) -> Task<Message> {
        let Some(epoch) = self.scan_epoch.checked_add(1) else {
            return Task::none();
        };
        self.scan_epoch = epoch;
        if self.scan_busy {
            self.queued_scan = Some((epoch, path));
            return Task::none();
        }
        self.scan_task(epoch, path)
    }
    fn scan_task(&mut self, epoch: u64, path: PathBuf) -> Task<Message> {
        self.scan_busy = true;
        let root = self.root.clone();
        Task::perform(
            async move {
                let result = tokio::task::spawn_blocking(move || {
                    let path = std::fs::canonicalize(path).map_err(|e| e.to_string())?;
                    if !path.starts_with(&root) {
                        return Err(
                            "Directory navigation stays inside the project root".to_string()
                        );
                    }
                    let entries = vim9::list_project_directory(&path).map_err(|e| e.to_string())?;
                    Ok((path, entries))
                })
                .await
                .unwrap_or_else(|e| Err(format!("Directory worker failed: {e}")));
                (epoch, result)
            },
            |(epoch, result)| Message::Directory(epoch, result),
        )
    }

    pub(super) fn update(&mut self, message: Message) -> Task<Message> {
        self.sync_ime();
        let task = self.reduce(message);
        self.sync_ime();
        task
    }
    fn reduce(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::RenderFrame { entry_id, frame } => {
                let active_entry = self
                    .editor
                    .as_ref()
                    .filter(|editor| editor.is_active())
                    .map(Vim9Session::entry_id);
                if accepts_render_entry(active_entry, entry_id, self.child_exited) {
                    self.render_frame = frame;
                    self.flush_native_input();
                }
            }
            Message::Tick => {
                if let Some(editor) = &self.editor {
                    self.snapshot = editor.snapshot();
                    if !self.snapshot.connected {
                        self.render_subscribed = false;
                        self.render_frame = None;
                        self.scroll.borrow_mut().clear();
                    } else if !self.render_subscribed {
                        self.render_subscribed = editor
                            .send(Vim9Command::SetRender {
                                enabled: self.config.vim9.native_buffer,
                            })
                            .is_ok();
                    }
                }
                if let Some(index) = &self.project_index {
                    let snapshot = index.snapshot();
                    let changed = !std::sync::Arc::ptr_eq(&snapshot.files, &self.project.files);
                    self.project = snapshot;
                    if changed {
                        self.refresh_quick_matches(true);
                    }
                }
                self.flush_native_input();
                self.blink_on = (self.blink_started.elapsed().as_millis() / 500).is_multiple_of(2);
            }
            Message::Drain => {
                if let Some(s) = &mut self.session {
                    s.flush_write_queue();
                }
            }
            Message::ScrollFrame => self.flush_native_input(),
            Message::Terminal(message) => match *message {
                super::Message::PtyOutput(id, fd, bytes, _) => {
                    if let Some(s) = &mut self.session {
                        if s.id != id || s.master_fd != fd || self.child_exited {
                            return Task::none();
                        }
                        s.terminal.process_batch(&bytes);
                        // IDE commands travel on the authenticated channel, never OSC.
                        // Host clipboard reads/writes remain explicitly user initiated.
                        s.terminal.take_osc52_clipboard_set();
                        while let Some(terminator) = s.terminal.take_osc52_clipboard_query() {
                            s.terminal.respond_osc52_clipboard("", terminator);
                        }
                        for request in s.terminal.take_clipboard_read_requests() {
                            super::service_osc5522_read(
                                &mut s.terminal,
                                &mut s.clipboard_read_in_flight,
                                &mut s.osc5522_pending_refusals,
                                false,
                                true,
                                request.kind,
                            );
                        }
                        s.terminal.pending_notifications.clear();
                        s.terminal.take_completed_commands();
                        s.flush_responses();
                        s.refresh();
                    }
                }
                super::Message::PtyExited(id, fd, status)
                    if self
                        .session
                        .as_ref()
                        .is_some_and(|s| s.id == id && s.master_fd == fd) =>
                {
                    self.child_exited = true;
                    if let Some(completion) = self.completion.take() {
                        completion.finish(status);
                    }
                    if let Some(shell_status) = &self.shell_status {
                        shell_status
                            .store(status.clamp(0, 255), std::sync::atomic::Ordering::Relaxed);
                    }
                    if let Some(editor) = self.editor.take() {
                        editor.revoke();
                    }
                    self.render_frame = None;
                    self.scroll.borrow_mut().clear();
                    self.render_subscribed = false;
                    self.snapshot = Vim9Snapshot::default();
                    self.form = None;
                    self.invalidate_input();
                    if self.embedded {
                        return Task::none();
                    }
                    if status == 0 || self.shell_status.is_some() {
                        return iced::exit();
                    }
                    self.error = Some(format!("Vim exited with status {status}. The final screen is retained; Close now dismisses the workbench."));
                }
                _ => {}
            },
            Message::Keyboard(id, event, captured) => {
                if matches!(event, keyboard::Event::KeyPressed { .. }) {
                    self.scroll.borrow_mut().clear();
                }
                return self.key(id, event, captured);
            }
            Message::Modifiers(modifiers) => self.modifiers = modifiers,
            Message::Ime(event) => {
                if !self.ime.admit(&event) {
                    return Task::none();
                }
                if let Some(s) = &mut self.session {
                    match event {
                        input_method::Event::Preedit(value, selection) => {
                            if !value.is_empty() || !s.terminal.preedit_text.is_empty() {
                                self.scroll.borrow_mut().clear();
                            }
                            s.terminal.preedit_text = value;
                            s.terminal.preedit_selection = selection;
                        }
                        input_method::Event::Commit(value) => {
                            self.scroll.borrow_mut().clear();
                            self.native_enter.lock().claim_pressed();
                            s.terminal.preedit_text.clear();
                            s.terminal.preedit_selection = None;
                            if !s.write_pty(value.as_bytes()) {
                                self.error = Some("Vim input queue is full".into());
                            }
                        }
                        input_method::Event::Closed => {
                            if !s.terminal.preedit_text.is_empty() {
                                self.scroll.borrow_mut().clear();
                            }
                            s.terminal.preedit_text.clear();
                            s.terminal.preedit_selection = None;
                        }
                        input_method::Event::Opened => {}
                    }
                }
            }
            Message::CancelledCommit => self.ime.retire_cancelled_commit(),
            Message::Focus(focused) => {
                self.scroll.borrow_mut().clear();
                self.retire_gestures();
                self.focused = focused;
                self.invalidate_input();
            }
            Message::WindowResize(_) if self.embedded => {}
            Message::Resize(size) | Message::WindowResize(size) => {
                self.size = size;
                self.resize();
            }
            Message::Close => return self.close(),
            Message::NativeWheel(wheel) => {
                if self.input_active() && self.snapshot.connected {
                    self.invalidate_input();
                    self.refresh_native_input();
                    if let Some(frame) = self.native_frame() {
                        self.scroll.borrow_mut().wheel(
                            frame,
                            crate::vim9_buffer::local_metrics(self.metrics),
                            wheel,
                        );
                    }
                    self.flush_native_input();
                }
            }
            Message::NativeMouse(input) => match input {
                crate::vim9_buffer::NativeInput::Compatibility(input) => return self.mouse(input),
                crate::vim9_buffer::NativeInput::Handled => {
                    self.invalidate_input();
                    self.flush_native_input();
                }
            },
            Message::Command(action) => {
                self.scroll.borrow_mut().clear();
                self.send(action);
            }
            Message::ShowForm(kind) => return self.form(kind),
            Message::FormInput(value) => {
                if let Some(form) = &mut self.form {
                    form.value = if form.kind == FormKind::QuickOpen {
                        bounded_quick_query(value)
                    } else {
                        bounded_input(value)
                    };
                    form.error = None;
                    form.selected = 0;
                }
                self.refresh_quick_matches(false);
            }
            Message::SearchCase(value) => {
                if self.search_form_open() {
                    self.search_options.case = value;
                }
            }
            Message::SearchRegex(value) => {
                if self.search_form_open() {
                    self.search_options.regex = value;
                }
            }
            Message::SearchWholeWord(value) => {
                if self.search_form_open() {
                    self.search_options.whole_word = value;
                }
            }
            Message::SearchGlob(value) => {
                if self.search_form_open() {
                    self.search_options.glob = bounded_input(value);
                }
            }
            Message::SubmitForm => self.submit(),
            Message::RefreshIndex => self.refresh_index(),
            Message::QuickOpenFile { path, query } => {
                // Bind a pointer row to its displayed query as well as its path:
                // a delayed click cannot acquire newly typed coordinates.
                if let Some(form) = self
                    .form
                    .as_ref()
                    .filter(|form| form.kind == FormKind::QuickOpen && form.value == query)
                {
                    match quick_open_command(&self.project, &form.value, Some(&path)) {
                        Ok(command) => {
                            self.form = None;
                            self.native_enter.lock().claim_pressed();
                            self.invalidate_input();
                            self.send(command);
                        }
                        Err(error) => {
                            if let Some(form) = &mut self.form {
                                form.error = Some(error.into());
                            }
                        }
                    }
                }
            }
            Message::MoveQuickSelection(delta) => self.move_quick_selection(delta),
            Message::CancelForm => {
                self.form = None;
                self.invalidate_input();
                self.native_enter.lock().claim_pressed();
            }
            Message::Browse(path) => return self.browse(path),
            Message::Directory(epoch, result) => {
                self.scan_busy = false;
                if epoch == self.scan_epoch {
                    match result {
                        Ok((path, entries)) => {
                            self.directory = path;
                            self.entries = entries;
                        }
                        Err(error) => self.error = Some(error),
                    }
                }
                if let Some((next, path)) = self.queued_scan.take() {
                    return self.scan_task(next, path);
                }
            }
            Message::OpenFile(path) => self.send(Vim9Command::Open {
                path: path.to_string_lossy().into_owned(),
                line: None,
                column: None,
            }),
            Message::Pasted(epoch, value) => {
                self.scroll.borrow_mut().clear();
                if epoch != self.input_epoch || !self.input_active() {
                    return Task::none();
                }
                if let (Some(s), Some(value)) = (&mut self.session, value) {
                    let paste = pty_input::encode_prompt_insert(
                        &value,
                        PasteModes {
                            bracketed: s.terminal.is_bracketed_paste_enabled(),
                        },
                        PastePolicy::prompt_insert(UnbracketedMultiline::FirstLineOnly),
                        false,
                    );
                    if !paste.is_empty() && !s.write_pty(&paste.bytes) {
                        self.error = Some("Vim input queue is full; paste was refused".into());
                    } else if paste.risk.truncated_to_first_line {
                        self.error = Some("Only the first line was pasted because Vim has bracketed paste disabled".into());
                    } else if paste.is_empty() && !value.is_empty() {
                        self.error =
                            Some("Paste refused by the terminal input safety policy".into());
                    }
                }
            }
            Message::ToggleFiles => {
                self.files_open = !self.files_open;
                self.resize();
                if self.files_open {
                    return self.browse(self.directory.clone());
                }
            }
            Message::ToggleDock => {
                self.dock_open = !self.dock_open;
                self.resize();
            }
            Message::OutputTab(output) => self.output_tab = output,
            Message::DismissError => {
                self.error = None;
                if let Some(editor) = &self.editor {
                    editor.clear_error();
                }
            }
        }
        Task::none()
    }

    fn key(
        &mut self,
        id: iced::window::Id,
        event: keyboard::Event,
        captured: bool,
    ) -> Task<Message> {
        if self
            .form
            .as_ref()
            .is_some_and(|form| form.kind == FormKind::QuickOpen)
        {
            if let keyboard::Event::KeyPressed { key, modifiers, .. } = &event {
                if !modifiers.control() && !modifiers.alt() && !modifiers.logo() {
                    let movement = match key {
                        keyboard::Key::Named(keyboard::key::Named::ArrowUp) => Some(-1),
                        keyboard::Key::Named(keyboard::key::Named::ArrowDown) => Some(1),
                        keyboard::Key::Named(keyboard::key::Named::PageUp) => {
                            Some(-(QUICK_VISIBLE_ROWS as isize))
                        }
                        keyboard::Key::Named(keyboard::key::Named::PageDown) => {
                            Some(QUICK_VISIBLE_ROWS as isize)
                        }
                        _ => None,
                    };
                    if let Some(delta) = movement {
                        return self.reduce(Message::MoveQuickSelection(delta));
                    }
                }
            }
        }
        if captured {
            self.native_enter.lock().acknowledge_captured(id, &event);
            if self.form.is_some()
                && matches!(
                    event,
                    keyboard::Event::KeyPressed {
                        key: keyboard::Key::Named(keyboard::key::Named::Escape),
                        ..
                    }
                )
            {
                self.form = None;
                self.invalidate_input();
            }
            return Task::none();
        }
        if self.native_enter.lock().acknowledge(id, &event) {
            return Task::none();
        }
        if self.ime.claim_enter(&event) {
            self.native_enter.lock().claim_pressed();
            return Task::none();
        }
        let keyboard::Event::KeyPressed {
            key,
            location,
            modifiers,
            text: committed,
            ..
        } = event
        else {
            return Task::none();
        };
        self.modifiers = modifiers;
        if self.form.is_some() {
            if key == keyboard::Key::Named(keyboard::key::Named::Escape) {
                self.form = None;
                self.invalidate_input();
            }
            return Task::none();
        }
        if !self.input_active() {
            return Task::none();
        }
        if let Some(shortcut) = native_shortcut(&key, modifiers, self.dock_open) {
            return self.shortcut(shortcut);
        }
        if modifiers.control() && modifiers.shift() && !modifiers.alt() && !modifiers.logo() {
            if let keyboard::Key::Character(value) = &key {
                match value.to_ascii_lowercase().as_str() {
                    "v" => {
                        let epoch = self.input_epoch;
                        return iced::clipboard::read()
                            .map(move |value| Message::Pasted(epoch, value));
                    }
                    "c" => {
                        if let Some(value) = self
                            .session
                            .as_ref()
                            .and_then(|s| s.terminal.copy_selection())
                        {
                            if !super::block_mode::clipboard_multiline_is_unsafe(&value) {
                                return iced::clipboard::write(value);
                            }
                            self.error = Some(
                                "Selection contains terminal control or visual-spoofing characters"
                                    .into(),
                            );
                        }
                        return Task::none();
                    }
                    _ => {}
                }
            }
        }
        self.invalidate_input();
        if let Some(s) = &mut self.session {
            let enhancements = KeyboardEnhancements {
                kitty_flags: s.terminal.keyboard_enhancement_flags(),
                modify_other_keys: s.terminal.xterm_modify_other_keys(),
                format_other_keys: s.terminal.xterm_format_other_keys(),
                report_all_keys: s.terminal.is_report_all_keys_enabled(),
                application_keypad: s.terminal.is_application_keypad(),
            };
            if let Some(bytes) = super::encode_key(
                &key,
                location,
                modifiers,
                committed.as_deref(),
                s.terminal.is_application_cursor_keys(),
                enhancements,
            ) {
                if !s.write_pty(&bytes) {
                    self.error = Some("Vim input queue is full".into());
                }
                s.scroll_to_bottom();
                s.terminal.clear_text_selection();
                s.refresh();
            }
        }
        Task::none()
    }

    fn mouse(&mut self, input: MouseInput) -> Task<Message> {
        if !self.input_active() {
            return Task::none();
        }
        let Some(s) = &mut self.session else {
            return Task::none();
        };
        match input {
            MouseInput::Press {
                col,
                row,
                button,
                shift,
                alt,
                count,
                app_eligible,
                ..
            } => {
                let app = s.terminal.is_mouse_enabled() && !shift && app_eligible;
                let mut gesture = super::TerminalMouseGesture {
                    session_id: s.id,
                    button,
                    report_to_app: app,
                    last_app_cell: None,
                    consumed: false,
                };
                if app {
                    if let Some((x, y)) =
                        super::projected_live_grid_cell(&s.terminal, &s.projection, row, col)
                    {
                        if let Some(bytes) =
                            s.terminal.get_mouse_report(super::btn_code(button), x, y)
                        {
                            if s.write_pty(&bytes) {
                                gesture.last_app_cell = Some((x, y));
                            }
                        }
                    }
                } else {
                    match button {
                        MouseButton::Left => match count {
                            2 => s
                                .terminal
                                .select_word_in_projection(&s.projection, row, col),
                            n if n >= 3 => s.terminal.select_line_in_projection(&s.projection, row),
                            _ => s.terminal.start_selection_in_projection(
                                &s.projection,
                                (row, col),
                                if alt {
                                    terminal::SelectionMode::Block
                                } else {
                                    terminal::SelectionMode::Normal
                                },
                            ),
                        },
                        MouseButton::Middle => {
                            let epoch = self.input_epoch;
                            return iced::clipboard::read_primary()
                                .map(move |value| Message::Pasted(epoch, value));
                        }
                        MouseButton::Right => {}
                    }
                }
                self.gestures[button.slot()] = Some(gesture);
            }
            MouseInput::Drag { col, row, count } => {
                let Some(gesture) = &mut self.gestures[MouseButton::Left.slot()] else {
                    return Task::none();
                };
                if gesture.report_to_app {
                    if s.terminal.is_mouse_motion_enabled() {
                        if let Some((x, y)) =
                            super::projected_live_grid_cell(&s.terminal, &s.projection, row, col)
                        {
                            if let Some(bytes) = s.terminal.get_mouse_report(32, x, y) {
                                if s.write_pty(&bytes) {
                                    gesture.last_app_cell = Some((x, y));
                                }
                            }
                        }
                    }
                } else {
                    match count {
                        2 => {
                            s.terminal
                                .extend_word_selection_in_projection(&s.projection, row, col)
                        }
                        n if n >= 3 => s
                            .terminal
                            .extend_line_selection_in_projection(&s.projection, row),
                        _ => s
                            .terminal
                            .update_selection_in_projection(&s.projection, (row, col)),
                    }
                }
            }
            MouseInput::Release { col, row, button } => {
                let Some(gesture) = self.gestures[button.slot()].take() else {
                    return Task::none();
                };
                if gesture.report_to_app {
                    let cell =
                        super::projected_live_grid_cell(&s.terminal, &s.projection, row, col)
                            .or(gesture.last_app_cell);
                    if let Some((x, y)) = cell {
                        if let Some(bytes) =
                            s.terminal
                                .get_mouse_release_report(super::btn_code(button), x, y)
                        {
                            s.write_pty(&bytes);
                        }
                    }
                }
            }
            MouseInput::Wheel {
                col,
                row,
                up,
                ctrl,
                shift,
                lines,
                app_eligible,
                ..
            } => {
                if ctrl {
                    let step = if up { 1.0 } else { -1.0 };
                    self.config.font_size = Config::clamp_font_size(self.config.font_size + step);
                    self.metrics = Metrics::with_font(
                        self.mono,
                        self.config.font_size,
                        self.config.line_spacing,
                        self.config.padding,
                        false,
                    );
                    self.resize();
                    return Task::none();
                }
                if s.terminal.is_mouse_enabled() && !shift && app_eligible {
                    if let Some((x, y)) =
                        super::projected_live_grid_cell(&s.terminal, &s.projection, row, col)
                    {
                        for _ in 0..lines.clamp(1, 32) {
                            if let Some(bytes) =
                                s.terminal.get_mouse_report(if up { 64 } else { 65 }, x, y)
                            {
                                s.write_pty(&bytes);
                            }
                        }
                    }
                } else {
                    let step =
                        self.config.scroll_speed.max(1) as isize * lines.clamp(1, 32) as isize;
                    s.scroll(if up { step } else { -step });
                }
            }
            MouseInput::ScrollTo { offset } => s.set_scroll_offset(offset),
        }
        if let Some(s) = &mut self.session {
            s.refresh();
        }
        Task::none()
    }

    pub(super) fn subscription(&self) -> Subscription<Message> {
        let events = iced::event::listen_with(|event, status, id| match event {
            iced::Event::Keyboard(keyboard::Event::ModifiersChanged(value)) => {
                Some(Message::Modifiers(value))
            }
            iced::Event::Keyboard(event) => Some(Message::Keyboard(
                id,
                event,
                status == iced::event::Status::Captured,
            )),
            iced::Event::InputMethod(input_method::Event::Commit(_))
                if status == iced::event::Status::Captured =>
            {
                Some(Message::CancelledCommit)
            }
            iced::Event::InputMethod(_) if status == iced::event::Status::Captured => None,
            iced::Event::InputMethod(event) => Some(Message::Ime(event)),
            iced::Event::Window(iced::window::Event::Resized(size)) => {
                Some(Message::WindowResize(size))
            }
            iced::Event::Window(iced::window::Event::Focused) => Some(Message::Focus(true)),
            iced::Event::Window(iced::window::Event::Unfocused) => Some(Message::Focus(false)),
            iced::Event::Window(iced::window::Event::CloseRequested) => Some(Message::Close),
            _ => None,
        });
        let mut subscriptions =
            vec![
                iced::time::every(Duration::from_millis(if self.focused { 80 } else { 250 }))
                    .map(|_| Message::Tick),
            ];
        if !self.embedded || self.focused {
            subscriptions.push(events);
        }
        if self.config.vim9.native_buffer {
            if self.focused && self.scroll.borrow().needs_frame() {
                subscriptions.push(iced::window::frames().map(|_| Message::ScrollFrame));
            }
            if let (Some(editor), Some(session)) = (&self.editor, &self.session) {
                subscriptions.push(Subscription::run_with(
                    RenderSubscription {
                        session_id: session.id,
                        interval_ms: if self.focused { 16 } else { 100 },
                        editor: editor.clone(),
                    },
                    render_stream,
                ));
            }
        }
        if let Some(s) = self.session.as_ref().filter(|_| !self.child_exited) {
            subscriptions.push(
                super::pty_subscription(super::PtySubscriptionKey {
                    id: s.id,
                    master_fd: s.master_fd,
                    reader_fd: s.reader_fd.clone(),
                    watch_source: s.watch_source.clone(),
                    child_pid: s.pty.get_child_pid(),
                })
                .map(|message| Message::Terminal(Box::new(message))),
            );
            if s.has_pending_write() {
                subscriptions
                    .push(iced::time::every(Duration::from_millis(8)).map(|_| Message::Drain));
            }
        }
        Subscription::batch(subscriptions)
    }

    fn command_button(&self, label: &'static str, command: Vim9Command) -> Element<'_, Message> {
        button(text(label).size(13))
            .padding([4, 8])
            .on_press_maybe(self.snapshot.connected.then_some(Message::Command(command)))
            .into()
    }
    fn navigation_button(
        &self,
        label: &'static str,
        help: &'static str,
        available: bool,
        command: Vim9Command,
    ) -> Element<'_, Message> {
        tooltip(
            button(text(label).size(14)).padding([4, 8]).on_press_maybe(
                (self.snapshot.connected && available).then_some(Message::Command(command)),
            ),
            text(help).size(12),
            tooltip::Position::Bottom,
        )
        .into()
    }
    fn optional_button(
        &self,
        label: &'static str,
        capability: &str,
        command: Vim9Command,
    ) -> Element<'_, Message> {
        button(text(label).size(13))
            .padding([4, 8])
            .on_press_maybe(
                (self.snapshot.connected
                    && self.snapshot.capabilities.iter().any(|c| c == capability))
                .then_some(Message::Command(command)),
            )
            .into()
    }
    fn strip<'a>(&self, content: Element<'a, Message>, height: f32) -> Element<'a, Message> {
        container(
            scrollable(content).direction(scrollable::Direction::Horizontal(Default::default())),
        )
        .padding([3, 6])
        .height(Length::Fixed(height))
        .width(Length::Fill)
        .into()
    }
    fn terminal_view(&self) -> Element<'_, Message> {
        let Some(s) = &self.session else {
            return container(text(self.error.as_deref().unwrap_or("Vim could not be launched. Install Vim 9 with +channel, +job, +timers and +quickfix."))).padding(24).center_x(Length::Fill).center_y(Length::Fill).into();
        };
        let native_frame = self.native_frame();
        let selection = if s.terminal.has_text_selection() {
            (0..s.projection.cells().len())
                .map(|row| {
                    s.terminal
                        .row_selection_cols_in_projection(&s.projection, row)
                })
                .collect()
        } else {
            Vec::new()
        };
        let terminal = TermWidget::new(
            s.projection.cells(),
            native_frame
                .filter(|frame| frame.cursor.visible)
                .map(|frame| (frame.cursor.row, frame.cursor.column))
                .unwrap_or(s.cursor),
            s.cursor_visible,
            s.terminal.cursor_shape,
            self.input_active(),
            &self.theme,
            self.metrics,
            self.mono,
            self.cjk,
            self.symbol,
            self.math,
            self.nerd,
            selection,
            s.projection.scroll_offset(),
            s.projection.max_scroll_offset(),
        )
        .interaction_mode(config::TerminalMode::Normal, 0)
        .modifiers(
            self.modifiers.shift(),
            self.modifiers.alt(),
            self.modifiers.control(),
        )
        .scrollbar_always(true)
        .app_mouse_full_grid(true)
        .dynamic_palette(&s.terminal.dynamic_palette)
        .dynamic_defaults(
            s.terminal.dynamic_fg,
            s.terminal.dynamic_bg,
            s.terminal.dynamic_cursor_color,
        )
        .preedit(
            (self.input_active() && !s.terminal.preedit_text.is_empty()).then(|| {
                (
                    s.terminal.preedit_text.clone(),
                    s.terminal.preedit_selection.clone(),
                )
            }),
        )
        .blink_on(self.blink_on)
        .opacity(1.0)
        .on_mouse(move |input| {
            if let Some(frame) = native_frame {
                Message::NativeMouse(self.scroll.borrow_mut().route_mouse(
                    frame,
                    crate::vim9_buffer::local_metrics(self.metrics),
                    self.modifiers,
                    input,
                ))
            } else {
                Message::NativeMouse(self.scroll.borrow_mut().compatibility_mouse(input).map_or(
                    crate::vim9_buffer::NativeInput::Handled,
                    crate::vim9_buffer::NativeInput::Compatibility,
                ))
            }
        });
        crate::vim9_buffer::NativeBuffer::new(
            terminal,
            native_frame.and(self.render_frame.as_ref()),
            self.metrics,
            self.mono,
            self.input_active(),
            self.blink_on,
        )
        .scroll(
            &self.scroll,
            self.modifiers,
            self.config.vim9.smooth_scroll,
            Message::NativeWheel,
        )
        .into()
    }
    fn files(&self) -> Element<'_, Message> {
        let parent = self
            .directory
            .parent()
            .filter(|path| path.starts_with(&self.root))
            .map(Path::to_path_buf);
        let heading = row![
            text("PROJECT").size(13),
            Space::new().width(Length::Fill),
            button(text("Up").size(12)).on_press_maybe(parent.map(Message::Browse)),
            button(text("↻").size(13)).on_press(Message::Browse(self.directory.clone())),
            button(text("×").size(13)).on_press(Message::ToggleFiles)
        ]
        .spacing(4);
        let mut listing = column![
            heading,
            text(visible_label(&self.directory.to_string_lossy())).size(11)
        ]
        .spacing(4);
        if self.scan_busy {
            listing = listing.push(text("Loading…").size(12));
        }
        for entry in &self.entries {
            let action = if entry.is_dir {
                Message::Browse(entry.path.clone())
            } else {
                Message::OpenFile(entry.path.clone())
            };
            let name = format!(
                "{} {}",
                if entry.is_dir { "+" } else { " " },
                visible_label(&entry.name)
            );
            listing = listing.push(
                button(text(name).size(13))
                    .width(Length::Fill)
                    .padding([3, 5])
                    .style(button::text)
                    .on_press(action),
            );
        }
        if self.entries.len() >= 1024 {
            listing = listing.push(text("Directory listing capped at 1,024 entries").size(11));
        }
        container(scrollable(listing))
            .padding(8)
            .width(Length::Fixed(SIDEBAR))
            .height(Length::Fill)
            .into()
    }
    fn dock(&self) -> Element<'_, Message> {
        let tabs = row![
            button(
                text(format!(
                    "Problems ({}){}",
                    self.snapshot.diagnostics.len(),
                    if self.snapshot.diagnostics_truncated {
                        "+"
                    } else {
                        ""
                    }
                ))
                .size(12)
            )
            .on_press(Message::OutputTab(false)),
            button(text("Build output").size(12)).on_press(Message::OutputTab(true)),
            Space::new().width(Length::Fill),
            button(text("×").size(13)).on_press(Message::ToggleDock)
        ]
        .spacing(5);
        let mut contents = column![].spacing(3);
        if let Some(error) = self.error.as_deref().or(self.snapshot.error.as_deref()) {
            contents = contents.push(
                row![
                    text(visible_label(error)).size(12),
                    button(text("Dismiss").size(12)).on_press(Message::DismissError)
                ]
                .spacing(8),
            );
        }
        if self.output_tab {
            let build = &self.snapshot.build;
            contents = contents.push(
                text(format!(
                    "{}  {}",
                    if build.running && build.cancelled {
                        "Stopping…".to_owned()
                    } else if build.running {
                        "Running".to_owned()
                    } else if build.cancelled {
                        "Cancelled".to_owned()
                    } else {
                        build
                            .exit_code
                            .map(|v| format!("Exit {v}"))
                            .unwrap_or_else(|| "No build yet".into())
                    },
                    visible_label(&build.command)
                ))
                .size(12),
            );
            if build.truncated > 0 {
                contents = contents
                    .push(text(format!("{} output lines omitted", build.truncated)).size(11));
            }
            for line in &build.output {
                contents = contents.push(
                    text(visible_label(line))
                        .font(iced::Font::MONOSPACE)
                        .size(12),
                );
            }
        } else {
            let search = &self.snapshot.search;
            if search.running {
                contents = contents.push(
                    row![
                        text(if search.cancelled {
                            "Stopping project search…"
                        } else {
                            "Searching project…"
                        })
                        .size(12),
                        button(text("Stop search").size(12)).on_press_maybe(
                            (self.snapshot.connected && !search.cancelled)
                                .then_some(Message::Command(Vim9Command::CancelSearch))
                        ),
                    ]
                    .spacing(8),
                );
            } else if !search.query.is_empty() {
                contents = contents.push(
                    text(format!(
                        "Last project search{}: {} · {} collected, {} omitted",
                        if search.cancelled {
                            " (cancelled; previous quickfix kept)"
                        } else if !(0..=1).contains(&search.exit_code) {
                            " (failed; previous quickfix kept)"
                        } else {
                            ""
                        },
                        visible_label(&search.query),
                        search.count,
                        search.truncated
                    ))
                    .size(12),
                );
            }
            if !search.query.is_empty() {
                contents = contents.push(
                    text(format!(
                        "Last search options: {}",
                        search_options_summary(&search.options)
                    ))
                    .size(11),
                );
            }
            let quickfix = &self.snapshot.quickfix;
            if !quickfix.title.is_empty() || quickfix.count > 0 {
                contents = contents.push(
                    text(format!(
                        "Vim quickfix · {} · {} / {} · {}",
                        visible_label(&quickfix.source),
                        quickfix.index,
                        quickfix.count,
                        visible_label(&quickfix.title)
                    ))
                    .size(12),
                );
            }
            for issue in &self.snapshot.diagnostics {
                let title = format!(
                    "{} {}:{}:{}  {}",
                    issue.kind,
                    visible_label(&issue.filename),
                    issue.line,
                    issue.column,
                    visible_label(&issue.text)
                );
                let action =
                    (!issue.filename.is_empty()).then_some(Message::Command(Vim9Command::Open {
                        path: issue.filename.clone(),
                        line: Some(issue.line.max(1)),
                        column: Some(issue.column.max(1)),
                    }));
                contents = contents.push(
                    button(text(title).size(12))
                        .padding([3, 5])
                        .style(button::text)
                        .on_press_maybe(action),
                );
            }
            for error in &self.snapshot.search.errors {
                contents = contents.push(text(visible_label(error)).size(12));
            }
            if self.snapshot.diagnostics.is_empty() {
                contents = contents.push(
                    text("No diagnostics. Build or search to populate Vim quickfix.").size(12),
                );
            }
        }
        container(column![tabs, scrollable(contents).height(Length::Fill)].spacing(5))
            .padding([5, 8])
            .height(Length::Fixed(DOCK))
            .width(Length::Fill)
            .into()
    }
    fn view(&self) -> Element<'_, Message> {
        let mut body = column![];
        if self.config.vim9.toolbar {
            let header = row![
                text("FROST / VIM9").size(15),
                self.navigation_button(
                    "←",
                    "Back to previous location (Alt+Left)",
                    self.snapshot.navigation.can_go_back,
                    Vim9Command::NavigateBack,
                ),
                self.navigation_button(
                    "→",
                    "Forward to next location (Alt+Right)",
                    self.snapshot.navigation.can_go_forward,
                    Vim9Command::NavigateForward,
                ),
                button(text("Quick open…").size(13))
                    .on_press(Message::ShowForm(FormKind::QuickOpen)),
                button(text("Files").size(13)).on_press(Message::ToggleFiles),
                button(text("Open…").size(13)).on_press(Message::ShowForm(FormKind::Open)),
                self.command_button("Save", Vim9Command::Save),
                self.command_button("Save all", Vim9Command::SaveAll),
                button(text("Search…").size(13)).on_press(Message::ShowForm(FormKind::Search)),
                button(text("Build…").size(13)).on_press(Message::ShowForm(FormKind::Build)),
                button(text("Save + build…").size(13)).on_press_maybe(
                    (!self.snapshot.build.running)
                        .then_some(Message::ShowForm(FormKind::SaveAndBuild))
                ),
                button(text("Stop build").size(13)).on_press_maybe(
                    (self.snapshot.connected && self.snapshot.build.running)
                        .then_some(Message::Command(Vim9Command::CancelBuild))
                ),
                button(text("Problems / output").size(13)).on_press(Message::ToggleDock),
                button(text("Close").size(13)).on_press(Message::Close)
            ]
            .spacing(5)
            .align_y(iced::Alignment::Center);
            let tools = row![
                button(text("Go to…").size(13)).on_press(Message::ShowForm(FormKind::Goto)),
                self.command_button("Vertical split", Vim9Command::Split { vertical: true }),
                self.command_button("Horizontal split", Vim9Command::Split { vertical: false }),
                self.optional_button("Definition", "definition", Vim9Command::Definition),
                self.optional_button("Hover", "hover", Vim9Command::Hover),
                self.optional_button("References", "references", Vim9Command::References),
                button(text("Rename…").size(13))
                    .padding([4, 8])
                    .on_press_maybe(
                        (self.snapshot.connected
                            && self.snapshot.capabilities.iter().any(|c| c == "rename"))
                        .then_some(Message::ShowForm(FormKind::Rename))
                    ),
                self.optional_button("Format", "format", Vim9Command::Format),
                self.command_button("Previous problem ⇧F8", Vim9Command::PreviousDiagnostic),
                self.command_button("Next problem F8", Vim9Command::NextDiagnostic),
                text("Vim owns editing, mappings and undo").size(12)
            ]
            .spacing(5)
            .align_y(iced::Alignment::Center);
            body = body
                .push(self.strip(header.into(), HEADER))
                .push(self.strip(tools.into(), TOOLS));
        }
        if self.config.vim9.buffer_tabs {
            let mut buffers = row![].spacing(4);
            for buffer in &self.snapshot.buffers {
                let name = Path::new(&buffer.name)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .filter(|s| !s.is_empty())
                    .unwrap_or("[No Name]");
                let label = format!(
                    "{}{}{}",
                    if buffer.current { "> " } else { "" },
                    visible_label(name),
                    if buffer.modified { " *" } else { "" }
                );
                let tab = button(text(label).size(12))
                    .on_press_maybe(
                        self.snapshot
                            .connected
                            .then_some(Message::Command(Vim9Command::Buffer { buffer: buffer.id })),
                    )
                    .style(if buffer.current {
                        button::primary
                    } else {
                        button::secondary
                    });
                let close =
                    button(text("×").size(12)).padding([4, 5]).on_press_maybe(
                        self.snapshot.connected.then_some(Message::Command(
                            Vim9Command::CloseBuffer { buffer: buffer.id },
                        )),
                    );
                buffers = buffers.push(
                    row![
                        tooltip(
                            tab,
                            text(visible_label(&buffer.name)),
                            tooltip::Position::Bottom
                        ),
                        tooltip(
                            close,
                            text("Close buffer (refuses unsaved changes)"),
                            tooltip::Position::Bottom
                        )
                    ]
                    .spacing(1),
                );
            }
            if self.snapshot.buffers_truncated {
                buffers = buffers.push(text("More buffers in :ls").size(12));
            }
            body = body.push(self.strip(buffers.into(), BUFFERS));
        }
        let (side, _) = chrome(
            self.size,
            &self.config.vim9,
            self.files_open,
            self.dock_open,
        );
        let mut middle = row![];
        if side > 0.0 {
            middle = middle.push(self.files());
        }
        middle = middle.push(
            container(self.terminal_view())
                .width(Length::Fill)
                .height(Length::Fill),
        );
        body = body.push(middle.height(Length::Fill));
        if self.dock_open && self.size.height >= 580.0 {
            body = body.push(self.dock());
        }
        if self.config.vim9.status_bar {
            let state = if self.child_exited {
                "EXITED"
            } else if self.snapshot.connected && self.snapshot.waiting_for_input {
                "WAITING FOR VIM INPUT"
            } else if self.snapshot.connected {
                "CONNECTED"
            } else {
                "BRIDGE OFFLINE"
            };
            let render_state = if !self.config.vim9.native_buffer {
                "Compatible view (configured)".to_owned()
            } else {
                self.render_frame
                    .as_ref()
                    .map(|frame| {
                        if frame.supported {
                            "GUI buffer · RGB".to_owned()
                        } else {
                            format!("Compatible view: {}", frame.fallback_reason)
                        }
                    })
                    .unwrap_or_else(|| "Connecting GUI buffer".to_owned())
            };
            let status = format!(
            "{state} | {render_state} | {} | {}{} | {}:{} / {} | {} · {} | {} split(s) | {} pending{}",
            visible_label(&self.snapshot.mode),
            visible_label(&self.snapshot.file),
            if self.snapshot.modified { " *" } else { "" },
            self.snapshot.line,
            self.snapshot.column,
            self.snapshot.line_count,
            visible_label(&self.snapshot.filetype),
            visible_label(&self.snapshot.encoding),
            self.snapshot.window_count,
            self.snapshot.pending,
            if self.error.is_some() || self.snapshot.error.is_some() {
                " | Error: open Problems / output"
            } else {
                ""
            }
        );
            let status = self
                .error
                .as_deref()
                .or(self.snapshot.error.as_deref())
                .map(|error| format!("Error: {} · {}", visible_label(error), status))
                .unwrap_or(status);
            body = body.push(
                container(text(status).size(12).wrapping(text::Wrapping::None))
                    .padding([4, 8])
                    .height(Length::Fixed(STATUS))
                    .width(Length::Fill)
                    .clip(true),
            );
        }
        let mut base: Element<'_, Message> = container(body)
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
        // Failures must remain discoverable when all persistent chrome is off.
        // This transient overlay does not take rows from Vim's viewport.
        if !self.config.vim9.status_bar {
            if let Some(error) = self.error.as_deref().or(self.snapshot.error.as_deref()) {
                let notice = container(
                    row![
                        text(visible_label(error)).size(12),
                        button(text("Dismiss").size(12)).on_press(Message::DismissError),
                    ]
                    .spacing(8)
                    .align_y(iced::Alignment::Center),
                )
                .padding(8)
                .style(container::rounded_box);
                base = stack![
                    base,
                    container(notice)
                        .align_bottom(Length::Fill)
                        .center_x(Length::Fill)
                        .padding(8),
                ]
                .into();
            }
        }
        let Some(form) = &self.form else {
            return base;
        };
        let field = text_input(form.kind.label(), &form.value)
            .id(INPUT_ID)
            .on_input(Message::FormInput)
            .on_submit(Message::SubmitForm)
            .size(15)
            .padding(8);
        let mut dialog_body = column![
            text(form.kind.label()).size(20),
            text(form.kind.help()).size(13),
            field,
        ]
        .spacing(10);
        if let Some(error) = &form.error {
            dialog_body = dialog_body.push(
                text(visible_label(error))
                    .size(12)
                    .color(iced::Color::from_rgb8(226, 129, 114)),
            );
        }
        let quick = form.kind == FormKind::QuickOpen;
        let quick_query = quick.then(|| self.project.quick_open_query(&form.value));
        let current_buffer = quick_query
            .as_ref()
            .is_some_and(|query| query.as_ref().is_ok_and(|query| query.current_buffer));
        let quick_valid = quick_query.as_ref().is_some_and(Result::is_ok);
        if let Some(Ok(query)) = &quick_query {
            if let Some((line, column)) = query.location {
                let target = if query.current_buffer {
                    "Current buffer"
                } else {
                    "Selected file"
                };
                dialog_body = dialog_body.push(
                    text(format!(
                        "{target} · line {line}{}",
                        column
                            .map(|column| format!(" · byte column {column}"))
                            .unwrap_or_default()
                    ))
                    .size(13),
                );
            }
        }
        if form.kind == FormKind::Search {
            let mut cases = row![text("Case:").size(13)]
                .spacing(6)
                .align_y(iced::Alignment::Center);
            for (label, case) in [
                ("Smart", Vim9SearchCase::Smart),
                ("Match", Vim9SearchCase::Sensitive),
                ("Ignore", Vim9SearchCase::Ignore),
            ] {
                cases = cases.push(
                    button(text(label).size(13))
                        .style(if self.search_options.case == case {
                            button::primary
                        } else {
                            button::secondary
                        })
                        .on_press(Message::SearchCase(case)),
                );
            }
            dialog_body = dialog_body
                .push(cases)
                .push(
                    row![
                        checkbox(self.search_options.regex)
                            .label("Regular expression")
                            .text_size(13)
                            .on_toggle(Message::SearchRegex),
                        checkbox(self.search_options.whole_word)
                            .label("Whole word")
                            .text_size(13)
                            .on_toggle(Message::SearchWholeWord),
                    ]
                    .spacing(20),
                )
                .push(
                    text_input("Files: one glob, e.g. *.rs or src/**", &self.search_options.glob)
                        .on_input(Message::SearchGlob)
                        .on_submit(Message::SubmitForm)
                        .size(13)
                        .padding(8),
                )
                .push(
                    text("Smart case matches case only when the query contains uppercase. An empty file glob searches all eligible files; spaces belong to the glob.")
                        .size(12),
                );
        }
        if quick && !current_buffer && quick_valid {
            let index_state = if self.project.scanning {
                "Indexing project files…".to_string()
            } else {
                format!(
                    "{} files indexed · showing {} matches{}",
                    self.project.files.len(),
                    self.quick_matches.len(),
                    if self.project.truncated {
                        " · index limit reached"
                    } else {
                        ""
                    }
                )
            };
            dialog_body = dialog_body.push(
                row![
                    text(index_state).size(12),
                    Space::new().width(Length::Fill),
                    button(text("Refresh").size(12))
                        .on_press_maybe((!self.project.scanning).then_some(Message::RefreshIndex)),
                ]
                .spacing(6)
                .align_y(iced::Alignment::Center),
            );
            if let Some(error) = &self.project.error {
                dialog_body = dialog_body.push(text(visible_label(error)).size(12));
            }
            let rows = (((self.size.height - 280.0).max(44.0) / 44.0) as usize)
                .clamp(1, QUICK_VISIBLE_ROWS);
            for index in quick_window(form.selected, self.quick_matches.len(), rows) {
                let file = &self.quick_matches[index];
                let name = file
                    .path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or(&file.relative);
                dialog_body = dialog_body.push(
                    button(
                        column![
                            text(visible_label(name))
                                .size(14)
                                .wrapping(text::Wrapping::None),
                            text(visible_label(&file.relative))
                                .size(11)
                                .wrapping(text::Wrapping::None),
                        ]
                        .spacing(1),
                    )
                    .width(Length::Fill)
                    .padding([5, 8])
                    .style(if index == form.selected {
                        button::primary
                    } else {
                        button::text
                    })
                    .on_press(Message::QuickOpenFile {
                        path: file.path.clone(),
                        query: form.value.clone(),
                    }),
                );
            }
            if self.quick_matches.is_empty() && !self.project.scanning {
                dialog_body = dialog_body.push(
                    text("No matching files. Try fewer letters or use Open… for an explicit path.")
                        .size(13),
                );
            }
        }
        let mut actions = row![
            button(if current_buffer {
                "Go"
            } else if quick {
                "Open"
            } else {
                "Apply"
            })
            .on_press_maybe(
                (!quick || (quick_valid && (current_buffer || !self.quick_matches.is_empty())))
                    .then_some(Message::SubmitForm)
            ),
            button("Cancel").on_press(Message::CancelForm),
        ]
        .spacing(8);
        if form.kind == FormKind::Search {
            actions = actions.push(
                button("Stop search").on_press_maybe(
                    (self.snapshot.connected
                        && self.snapshot.search.running
                        && !self.snapshot.search.cancelled)
                        .then_some(Message::Command(Vim9Command::CancelSearch)),
                ),
            );
        }
        if quick && !current_buffer && quick_valid {
            actions = actions
                .push(Space::new().width(Length::Fill))
                .push(button("↑").on_press(Message::MoveQuickSelection(-1)))
                .push(button("↓").on_press(Message::MoveQuickSelection(1)))
                .push(
                    text(format!(
                        "{} / {}",
                        if self.quick_matches.is_empty() {
                            0
                        } else {
                            form.selected + 1
                        },
                        self.quick_matches.len()
                    ))
                    .size(12),
                );
        }
        dialog_body = dialog_body.push(actions);
        let dialog = container(scrollable(dialog_body).height(Length::Shrink))
            .padding(20)
            .max_height((self.size.height - 32.0).max(1.0))
            .width(Length::Fixed(
                (self.size.width - 32.0).clamp(1.0, if quick { 680.0 } else { 580.0 }),
            ))
            .style(container::rounded_box);
        let scrim = mouse_area(
            container(Space::new())
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .on_press(Message::CancelForm);
        stack![
            base,
            Element::from(scrim),
            container(dialog)
                .center_x(Length::Fill)
                .center_y(Length::Fill)
        ]
        .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }
    #[test]
    fn launch_contract_preserves_plain_terminal_and_literal_paths() {
        assert_eq!(target_from_args(&[]).unwrap(), None);
        assert_eq!(
            target_from_args(&args(&["--ide"])).unwrap(),
            Some(PathBuf::from("."))
        );
        assert_eq!(
            target_from_args(&args(&["--ide", "a path|with $syntax.rs"])).unwrap(),
            Some(PathBuf::from("a path|with $syntax.rs"))
        );
        assert_eq!(
            target_from_args(&args(&["--ide", "--", "-file"])).unwrap(),
            Some(PathBuf::from("-file"))
        );
        for values in [
            vec!["--ide", "a", "b"],
            vec!["--ide", "--execute", "sh"],
            vec!["--mode", "block", "--ide"],
        ] {
            assert!(target_from_args(&args(&values)).is_err());
        }
    }
    #[test]
    fn chrome_reserves_only_visible_panels_and_keeps_small_windows_usable() {
        let minimal = vim9::Vim9UiConfig::default();
        assert_eq!(
            chrome(Size::new(480.0, 320.0), &minimal, false, false),
            (0.0, 0.0)
        );
        assert_eq!(
            chrome(Size::new(1000.0, 700.0), &minimal, false, false),
            (0.0, 0.0)
        );
        // An explicitly revealed panel reserves only its own area.
        assert_eq!(
            chrome(Size::new(1000.0, 700.0), &minimal, true, false),
            (250.0, 0.0)
        );
        assert_eq!(
            chrome(Size::new(1000.0, 700.0), &minimal, false, true),
            (0.0, 200.0)
        );
        assert_eq!(
            chrome(Size::new(480.0, 320.0), &minimal, true, true),
            (0.0, 0.0)
        );

        let workbench = vim9::Vim9UiConfig {
            toolbar: true,
            buffer_tabs: true,
            status_bar: true,
            ..Default::default()
        };
        assert_eq!(
            chrome(Size::new(480.0, 320.0), &workbench, true, true),
            (0.0, 136.0)
        );
        assert_eq!(
            chrome(Size::new(1000.0, 700.0), &workbench, true, true),
            (250.0, 336.0)
        );
        assert_eq!(
            chrome(Size::new(1000.0, 700.0), &workbench, false, false),
            (0.0, 136.0)
        );
        let tabs_only = vim9::Vim9UiConfig {
            buffer_tabs: true,
            ..Default::default()
        };
        assert_eq!(
            chrome(Size::new(1000.0, 700.0), &tabs_only, false, false),
            (0.0, 34.0)
        );
    }
    #[test]
    fn labels_cannot_hide_controls_and_inputs_keep_utf8_boundaries() {
        assert_eq!(visible_label("文件.rs"), "文件.rs");
        assert_eq!(visible_label("a\x1b\u{202e}b"), "a\\u{1b}\\u{202e}b");
        let text = bounded_input("界".repeat(MAX_INPUT));
        assert!(text.len() <= MAX_INPUT);
        assert!(text.ends_with('界'));
    }
    #[test]
    fn goto_validates_coordinates_and_never_accepts_ex_commands() {
        assert_eq!(goto_position("42"), Ok((42, None)));
        assert_eq!(goto_position(" 42 : 8 "), Ok((42, Some(8))));
        for invalid in [
            "",
            "0",
            "-1",
            "+2",
            "2:",
            ":2",
            "2:0",
            "2:3:4",
            "２",
            "1|quit",
            "4294967296",
            "999999999999999999999999999999",
        ] {
            assert!(goto_position(invalid).is_err(), "accepted {invalid:?}");
        }
    }
    #[test]
    fn native_shortcuts_leave_vim_control_navigation_available() {
        use keyboard::{key::Named, Key, Modifiers};
        let chord = Modifiers::CTRL | Modifiers::SHIFT;
        let native_shortcut = |key: &Key, modifiers| super::native_shortcut(key, modifiers, true);
        assert_eq!(
            native_shortcut(&Key::Character("P".into()), chord),
            Some(Shortcut::Form(FormKind::QuickOpen))
        );
        assert_eq!(
            native_shortcut(&Key::Character("g".into()), chord),
            Some(Shortcut::Form(FormKind::Goto))
        );
        assert_eq!(
            native_shortcut(&Key::Character("e".into()), chord),
            Some(Shortcut::ToggleFiles)
        );
        assert_eq!(
            native_shortcut(&Key::Character("j".into()), chord),
            Some(Shortcut::ToggleDock)
        );
        assert_eq!(
            native_shortcut(&Key::Character("s".into()), Modifiers::CTRL),
            Some(Shortcut::Save)
        );
        assert_eq!(
            native_shortcut(&Key::Named(Named::F8), Modifiers::SHIFT),
            Some(Shortcut::PreviousProblem)
        );
        assert_eq!(
            native_shortcut(&Key::Named(Named::F8), Modifiers::empty()),
            Some(Shortcut::NextProblem)
        );
        assert_eq!(
            native_shortcut(&Key::Named(Named::PageDown), chord),
            Some(Shortcut::NextBuffer)
        );
        assert_eq!(
            native_shortcut(&Key::Named(Named::ArrowLeft), Modifiers::ALT),
            Some(Shortcut::NavigateBack)
        );
        assert_eq!(
            native_shortcut(&Key::Named(Named::ArrowRight), Modifiers::ALT),
            Some(Shortcut::NavigateForward)
        );
        for modifiers in [
            Modifiers::CTRL,
            Modifiers::empty(),
            Modifiers::ALT | Modifiers::CTRL,
            Modifiers::ALT | Modifiers::SHIFT,
            Modifiers::ALT | Modifiers::LOGO,
        ] {
            for direction in [Named::ArrowLeft, Named::ArrowRight] {
                assert_eq!(native_shortcut(&Key::Named(direction), modifiers), None);
            }
        }
        for key in ["p", "o", "w", "g", "f", "b", "e", "j"] {
            assert_eq!(
                native_shortcut(&Key::Character(key.into()), Modifiers::CTRL),
                None
            );
        }
        assert_eq!(
            native_shortcut(&Key::Character("p".into()), chord | Modifiers::ALT),
            None
        );
        assert_eq!(
            native_shortcut(&Key::Character("p".into()), chord | Modifiers::LOGO),
            None
        );
        assert_eq!(
            native_shortcut(&Key::Named(Named::F8), Modifiers::CTRL),
            None
        );
    }
    #[test]
    fn diagnostic_keys_belong_to_vim_while_the_tool_dock_is_hidden() {
        use keyboard::{key::Named, Key, Modifiers};
        for modifiers in [Modifiers::empty(), Modifiers::SHIFT] {
            assert_eq!(
                native_shortcut(&Key::Named(Named::F8), modifiers, false),
                None
            );
            assert!(native_shortcut(&Key::Named(Named::F8), modifiers, true).is_some());
        }
        assert_eq!(
            native_shortcut(&Key::Named(Named::F5), Modifiers::empty(), false),
            None
        );
        assert_eq!(
            native_shortcut(&Key::Character("s".into()), Modifiers::CTRL, false),
            Some(Shortcut::Save)
        );
        assert_eq!(
            native_shortcut(
                &Key::Character("j".into()),
                Modifiers::CTRL | Modifiers::SHIFT,
                false
            ),
            Some(Shortcut::ToggleDock)
        );
    }

    #[test]
    fn quick_open_locations_validate_pointer_targets_and_current_buffer_navigation() {
        let project = Vim9ProjectSnapshot {
            files: std::sync::Arc::new(
                ["src/main.rs", "src/other.rs", "notes:42"]
                    .into_iter()
                    .map(|relative| Vim9ProjectFile {
                        path: PathBuf::from("/project").join(relative),
                        relative: relative.to_owned(),
                    })
                    .collect(),
            ),
            ..Default::default()
        };
        let main = Path::new("/project/src/main.rs");
        assert!(
            matches!(quick_open_command(&project, "main:42:8", Some(main)), Ok(Vim9Command::Open { path, line: Some(42), column: Some(8) }) if path == main.to_str().unwrap())
        );
        assert!(
            quick_open_command(&project, "other:7", Some(main)).is_err(),
            "a stale clicked row opened after the query changed"
        );
        assert!(quick_open_command(&project, "main:0", Some(main)).is_err());
        assert!(
            matches!(quick_open_command(&project, "notes:42", Some(Path::new("/project/notes:42"))), Ok(Vim9Command::Open { path, line: None, column: None }) if path == "/project/notes:42")
        );
        let unavailable_index = Vim9ProjectSnapshot {
            scanning: true,
            error: Some("index unavailable".into()),
            ..Default::default()
        };
        assert!(matches!(
            quick_open_command(&unavailable_index, ":19:3", None),
            Ok(Vim9Command::Goto {
                line: 19,
                column: Some(3)
            })
        ));
        assert!(quick_open_command(&unavailable_index, "main:19", Some(main)).is_err());
    }

    #[test]
    fn quick_open_navigation_stays_visible_and_empty_lists_are_safe() {
        assert_eq!(moved_selection(0, -1, 0), 0);
        assert_eq!(moved_selection(0, -1, 80), 0);
        assert_eq!(moved_selection(79, 1, 80), 79);
        assert_eq!(moved_selection(4, 8, 80), 12);
        for count in 1..=QUICK_MATCH_LIMIT {
            for selected in 0..count {
                for rows in 1..=QUICK_VISIBLE_ROWS {
                    let range = quick_window(selected, count, rows);
                    assert!(range.contains(&selected));
                    assert!(range.end <= count);
                    assert!(range.len() <= rows);
                }
            }
        }
        assert!(quick_window(0, 0, 8).is_empty());
        let query = bounded_quick_query("界".repeat(MAX_QUICK_QUERY + 1));
        assert_eq!(query.chars().count(), MAX_QUICK_QUERY);
        assert_eq!(query, "界".repeat(MAX_QUICK_QUERY));
        let name = format!("{}文件.rs", "目录/".repeat(30));
        let project = Vim9ProjectSnapshot {
            files: std::sync::Arc::new(vec![Vim9ProjectFile {
                path: PathBuf::from("/project").join(&name),
                relative: name.clone(),
            }]),
            ..Default::default()
        };
        let location = bounded_quick_query(format!("{name}:42:8"));
        assert!(
            matches!(
                quick_open_command(&project, &location, Some(&project.files[0].path)),
                Ok(Vim9Command::Open {
                    line: Some(42),
                    column: Some(8),
                    ..
                })
            ),
            "a valid Unicode path must not truncate the destination by UTF-8 byte count"
        );
    }
    #[test]
    fn startup_and_closing_do_not_own_shell_persistence_or_force_editor_exit() {
        let source = include_str!("vim9_ide.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap();
        assert!(!source.contains("save_session_snapshot("));
        assert!(!source.contains("try_acquire_instance_lock("));
        assert!(!source.contains("Frost::new("));
        assert!(source.contains("self.session.is_none() || self.child_exited"));
        assert!(source.contains("self.send(Vim9Command::Quit)"));
        assert!(!source.contains("qa!"));
    }
}

#[cfg(test)]
#[path = "vim9_ide_native_tests.rs"]
mod native_tests;

#[cfg(test)]
mod explicit_entry_tests {
    #[test]
    fn queued_paint_cannot_revive_or_cross_an_entry() {
        let old = uuid::Uuid::new_v4();
        let next = uuid::Uuid::new_v4();
        assert!(super::accepts_render_entry(Some(old), old, false));
        assert!(!super::accepts_render_entry(None, old, false));
        assert!(!super::accepts_render_entry(Some(old), old, true));
        assert!(!super::accepts_render_entry(Some(next), old, false));
        assert!(super::accepts_render_entry(Some(next), next, false));
    }
}
