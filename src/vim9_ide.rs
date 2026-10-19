//! Real Vim, rendered by Frost's native GPU terminal, with a typed IDE sidebar.
//!
//! This application is deliberately separate from `Frost::new`: it never takes
//! the terminal instance lock, restores shell tabs, or persists editor argv.
//! In particular, an authenticated bridge disconnect is NOT a child exit.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use iced::advanced::input_method;
use iced::advanced::widget::{self, operation, Id};
use iced::widget::{button, column, container, mouse_area, row, scrollable, stack, text, text_input, Space};
use iced::{keyboard, Element, Length, Size, Subscription, Task};
use jterm_core::pty_input::{self, PasteModes, PastePolicy, UnbracketedMultiline};
use jterm_core::vim9::{self, ProjectEntry, Vim9Command, Vim9Session, Vim9Snapshot};

use super::{config, native_enter, terminal, terminal_ime, terminal_view};
use super::{Config, KeyboardEnhancements, Metrics, MouseButton, MouseInput, Session, TermWidget, Theme};

const HEADER: f32 = 38.0;
const TOOLS: f32 = 36.0;
const BUFFERS: f32 = 34.0;
const STATUS: f32 = 28.0;
const SIDEBAR: f32 = 250.0;
const DOCK: f32 = 200.0;
const INPUT_ID: &str = "frost-vim9-input";
const MAX_INPUT: usize = 16 * 1024;

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
    let rest = if rest.first().is_some_and(|s| s == "--") { &rest[1..] } else {
        if rest.first().is_some_and(|s| s.to_string_lossy().starts_with('-')) {
            return Err("Use frost --ide -- PATH for a filename beginning with '-'".into());
        }
        rest
    };
    if rest.len() > 1 {
        return Err("Vim9 IDE accepts one project directory or file".into());
    }
    Ok(Some(rest.first().map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))))
}

pub(super) fn run_if_requested() -> Option<iced::Result> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let target = match target_from_args(&args) {
        Ok(Some(target)) => target,
        Ok(None) => return None,
        Err(error) => {
            eprintln!("frost: {error}");
            std::process::exit(2);
        }
    };
    let loaded = Config::load_with_diagnostics();
    let mut config = loaded.config;
    // These changes are process-local. There is no config writer in this app.
    config.block_mode = false;
    config.terminal_mode = Some(config::TerminalMode::Normal);
    config.disable_alt_screen = false;
    config.ascii_organism_enabled = false;
    let native = native_enter::install();
    let size = Size::new(config.initial_width.max(1000.0), config.initial_height.max(700.0));
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
        icon: iced::window::icon::from_file_data(super::WINDOW_ICON_PNG, Some(image::ImageFormat::Png)).ok(),
        ..Default::default()
    };
    Some(iced::application(
        move || Ide::new(target.clone(), config.clone(), loaded.diagnostic.clone(), native.clone(), size),
        Ide::update,
        Ide::view,
    )
    .title("Frost - Vim9 IDE")
    .theme(Ide::native_theme)
    .subscription(Ide::subscription)
    .antialiasing(false)
    .window(window)
    .run())
}

/// The ordinary terminal's launcher never injects a command into its shell.
/// Reap the separate process without tying its lifetime to a terminal pane.
pub(super) fn open_from_terminal(app: &mut super::Frost) {
    let target = app.sessions.get(app.active).and_then(|session| {
        if session.managed_remote || session.observed_ssh_command().is_some()
            || matches!(session.fg_proc_cache.as_deref(), Some("ssh" | "mosh" | "telnet")) {
            return None;
        }
        session.cwd().map(PathBuf::from).filter(|path| path.is_dir())
    });
    let Some(target) = target else {
        app.push_toast("Select a local terminal, or run frost --ide /path/to/project", super::ToastKind::Warning);
        return;
    };
    let spawn = std::env::current_exe().and_then(|executable| {
        std::process::Command::new(executable).arg("--ide").arg(&target).current_dir(&target).spawn()
    });
    match spawn {
        Ok(mut child) => {
            let _ = std::thread::Builder::new().name("vim9-window-reaper".into()).spawn(move || { let _ = child.wait(); });
        }
        Err(error) => app.push_toast(format!("Cannot open Vim9 IDE: {error}"), super::ToastKind::Warning),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FormKind { Open, Search, Build, Rename }
impl FormKind {
    fn label(self) -> &'static str {
        match self { Self::Open => "Open file", Self::Search => "Search project", Self::Build => "Build / test", Self::Rename => "Rename symbol" }
    }
    fn help(self) -> &'static str {
        match self {
            Self::Open => "Enter a local filename. New files require an existing parent directory. Vim retains its normal unsaved-buffer checks.",
            Self::Search => "Literal project search using ripgrep. No shell interpretation. Results replace Vim's quickfix list.",
            Self::Build => "Review this shell command before running it. Empty uses Vim's makeprg. Nothing is saved or executed automatically.",
            Self::Rename => "Requires a configured Vim9 LSP provider. Enter the new symbol name.",
        }
    }
}
#[derive(Debug, Clone)]
struct Form { kind: FormKind, value: String }

#[derive(Debug, Clone)]
enum Message {
    Terminal(super::Message),
    Tick,
    Drain,
    Keyboard(iced::window::Id, keyboard::Event, bool),
    Modifiers(keyboard::Modifiers),
    Ime(input_method::Event),
    CancelledCommit,
    Focus(bool),
    Resize(Size),
    Close,
    Mouse(MouseInput),
    Command(Vim9Command),
    ShowForm(FormKind),
    FormInput(String),
    SubmitForm,
    CancelForm,
    Browse(PathBuf),
    Directory(u64, Result<(PathBuf, Vec<ProjectEntry>), String>),
    OpenFile(PathBuf),
    Pasted(u64, Option<String>),
    ToggleFiles,
    ToggleDock,
    OutputTab(bool),
    DismissError,
}

struct Ide {
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
    build_command: String,
    search_query: String,
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
}

fn chrome(size: Size, files_open: bool, dock_open: bool) -> (f32, f32) {
    let sidebar = if files_open && size.width >= 820.0 { SIDEBAR } else { 0.0 };
    let dock = if dock_open && size.height >= 580.0 { DOCK } else { 0.0 };
    (sidebar, HEADER + TOOLS + BUFFERS + STATUS + dock)
}

fn visible_label(value: &str) -> String {
    let mut result = String::new();
    for character in value.chars().take(240) {
        if character.is_control() || matches!(character, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{206f}' | '\u{feff}') {
            result.push_str(&format!("\\u{{{:x}}}", character as u32));
        } else { result.push(character); }
    }
    if value.chars().count() > 240 { result.push('…'); }
    result
}

fn bounded_input(mut value: String) -> String {
    if value.len() > MAX_INPUT {
        let mut end = MAX_INPUT;
        while !value.is_char_boundary(end) { end -= 1; }
        value.truncate(end);
    }
    value
}

fn editor_session(editor: &Vim9Session, config: &Config, clean: bool) -> anyhow::Result<Session> {
    let launch = editor.launch();
    let mut argv = vec![launch.program.clone()];
    if clean { argv.extend(["-u", "NONE", "-i", "NONE", "-n"].map(str::to_owned)); }
    argv.extend(launch.args.iter().cloned());
    let env: Vec<_> = launch.env.iter().filter_map(|entry| entry.split_once('=')).collect();
    Session::spawn_argv_env(config, 1, 100, 30, editor.root().to_str(), Some(&argv), &env)
}

impl Ide {
    fn new(target: PathBuf, config: Config, diagnostic: Option<String>, native_enter: native_enter::SharedOwnership, size: Size) -> (Self, Task<Message>) {
        let theme = Theme::get_theme(&config.theme).unwrap_or_default();
        let mono = super::resolve_mono_font(&config.font_family);
        let metrics = Metrics::with_font(mono, Config::clamp_font_size(config.font_size), config.line_spacing, config.padding, false);
        let mut error = diagnostic;
        let mut session = None;
        let mut editor = None;
        let mut root = target;
        match Vim9Session::start(&root) {
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
        let build_command = if root.join("Cargo.toml").is_file() { "cargo check --message-format=short".into() } else { String::new() };
        let mut state = Self {
            config, theme, metrics, mono,
            cjk: super::resolve_optional_font(Config::cjk_monospace_font_family()),
            symbol: super::resolve_optional_font(Config::symbol_monospace_font_family()),
            math: super::resolve_optional_font(Config::math_symbol_font_family()),
            nerd: super::resolve_optional_font(Config::nerd_symbol_font_family()),
            session, editor, snapshot: Vim9Snapshot::default(), directory: root.clone(), root: root.clone(), entries: Vec::new(),
            scan_busy: false, scan_epoch: 0, queued_scan: None, files_open: true, dock_open: true, output_tab: false,
            form: None, build_command, search_query: String::new(), error, size, focused: true,
            modifiers: keyboard::Modifiers::default(), native_enter, ime: terminal_ime::Ownership::default(),
            input_epoch: 0, gestures: [None; 3], blink_started: Instant::now(), blink_on: true, child_exited: false,
        };
        state.resize();
        state.sync_ime();
        let task = if state.session.is_some() { state.browse(root) } else { Task::none() };
        (state, task)
    }

    fn native_theme(&self) -> iced::Theme {
        let [r, g, b] = self.theme.terminal.background;
        if u32::from(r) + u32::from(g) + u32::from(b) > 384 { iced::Theme::Light } else { iced::Theme::Dark }
    }
    fn input_active(&self) -> bool { self.focused && self.form.is_none() && !self.child_exited && self.session.is_some() }
    fn invalidate_input(&mut self) { self.input_epoch = self.input_epoch.wrapping_add(1); }
    fn sync_ime(&mut self) {
        let owner = self.input_active().then(|| self.session.as_ref()).flatten().map(|s| terminal_ime::Owner { session: s.id, instance: s.ime_identity.clone() });
        if self.ime.sync(owner) {
            if let Some(s) = &mut self.session { s.terminal.preedit_text.clear(); s.terminal.preedit_selection = None; }
        }
    }
    fn retire_gestures(&mut self) {
        for gesture in &mut self.gestures {
            if let Some(previous) = gesture.take().filter(|g| g.report_to_app) {
                if let (Some(s), Some((col, row))) = (&mut self.session, previous.last_app_cell) {
                    if let Some(bytes) = s.terminal.get_mouse_release_report(super::btn_code(previous.button), col, row) { s.write_pty(&bytes); }
                }
            }
        }
    }
    fn resize(&mut self) {
        self.retire_gestures();
        let (sidebar, vertical) = chrome(self.size, self.files_open, self.dock_open);
        let (cols, rows) = self.metrics.grid_size((self.size.width - sidebar - terminal_view::SCROLLBAR_WIDTH).max(1.0), (self.size.height - vertical).max(1.0));
        if let Some(s) = &mut self.session {
            s.terminal.set_viewport_pixel_size((cols as f32 * self.metrics.cell_w).round() as u32, (rows as f32 * self.metrics.cell_h).round() as u32);
            if s.terminal.get_dimensions() != (cols, rows) {
                s.terminal.on_resize(cols, rows);
                if let Err(error) = s.pty.resize(cols, rows) { self.error = Some(format!("Cannot resize Vim: {error}")); }
            }
            s.refresh();
        }
    }
    fn send(&mut self, command: Vim9Command) {
        self.invalidate_input();
        self.error = None;
        let result = self.editor.as_ref().ok_or_else(|| "Vim did not start".to_string()).and_then(|editor| editor.send(command).map(|_| ()).map_err(|e| e.to_string()));
        if let Err(error) = result { self.error = Some(error); }
    }
    fn close(&mut self) -> Task<Message> {
        if self.session.is_none() || self.child_exited { return iced::exit(); }
        self.send(Vim9Command::Quit);
        if !self.snapshot.connected {
            self.error = Some("The bridge is disconnected. Vim is still running: save and use :qa inside the editor. This window will not force-kill it.".into());
        }
        Task::none()
    }
    fn form(&mut self, kind: FormKind) -> Task<Message> {
        self.retire_gestures();
        self.invalidate_input();
        self.native_enter.lock().claim_pressed();
        let value = match kind {
            FormKind::Open => format!("{}/", self.directory.display()),
            FormKind::Search => self.search_query.clone(),
            FormKind::Build => self.build_command.clone(),
            FormKind::Rename => String::new(),
        };
        self.form = Some(Form { kind, value });
        widget::operate(operation::focusable::focus(Id::new(INPUT_ID)))
    }
    fn submit(&mut self) {
        let Some(form) = self.form.take() else { return; };
        // Ownership is physical, not a timer or an artificial Escape sequence.
        self.native_enter.lock().claim_pressed();
        self.invalidate_input();
        let action = match form.kind {
            FormKind::Open => {
                if form.value.is_empty() || form.value.contains(['\r', '\n', '\0']) {
                    self.error = Some("Enter a single-line filename".into());
                    self.form = Some(form);
                    return;
                }
                let path = PathBuf::from(&form.value);
                let path = if path.is_absolute() { path } else { self.directory.join(path) };
                Vim9Command::Open { path: path.to_string_lossy().into_owned(), line: None, column: None }
            }
            FormKind::Search => { self.search_query.clone_from(&form.value); self.output_tab = false; Vim9Command::Search { query: form.value } }
            FormKind::Build => { self.build_command.clone_from(&form.value); self.output_tab = true; Vim9Command::Build { command: form.value } }
            FormKind::Rename => Vim9Command::Rename { name: form.value },
        };
        self.dock_open = true;
        self.resize();
        self.send(action);
    }

    fn browse(&mut self, path: PathBuf) -> Task<Message> {
        let Some(epoch) = self.scan_epoch.checked_add(1) else { return Task::none(); };
        self.scan_epoch = epoch;
        if self.scan_busy { self.queued_scan = Some((epoch, path)); return Task::none(); }
        self.scan_task(epoch, path)
    }
    fn scan_task(&mut self, epoch: u64, path: PathBuf) -> Task<Message> {
        self.scan_busy = true;
        let root = self.root.clone();
        Task::perform(async move {
            let result = tokio::task::spawn_blocking(move || {
                let path = std::fs::canonicalize(path).map_err(|e| e.to_string())?;
                if !path.starts_with(&root) { return Err("Directory navigation stays inside the project root".to_string()); }
                let entries = vim9::list_project_directory(&path).map_err(|e| e.to_string())?;
                Ok((path, entries))
            }).await.unwrap_or_else(|e| Err(format!("Directory worker failed: {e}")));
            (epoch, result)
        }, |(epoch, result)| Message::Directory(epoch, result))
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        self.sync_ime();
        let task = self.reduce(message);
        self.sync_ime();
        task
    }
    fn reduce(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Tick => {
                if let Some(editor) = &self.editor { self.snapshot = editor.snapshot(); }
                self.blink_on = (self.blink_started.elapsed().as_millis() / 500).is_multiple_of(2);
            }
            Message::Drain => { if let Some(s) = &mut self.session { s.flush_write_queue(); } }
            Message::Terminal(super::Message::PtyOutput(id, fd, bytes, _)) => {
                if let Some(s) = &mut self.session {
                    if s.id != id || s.master_fd != fd || self.child_exited { return Task::none(); }
                    s.terminal.process_batch(&bytes);
                    // IDE commands travel on the authenticated channel, never OSC.
                    // Host clipboard reads/writes remain explicitly user initiated.
                    s.terminal.take_osc52_clipboard_set();
                    while let Some(terminator) = s.terminal.take_osc52_clipboard_query() { s.terminal.respond_osc52_clipboard("", terminator); }
                    for request in s.terminal.take_clipboard_read_requests() {
                        super::service_osc5522_read(&mut s.terminal, &mut s.clipboard_read_in_flight, &mut s.osc5522_pending_refusals, false, true, request.kind);
                    }
                    s.terminal.pending_notifications.clear();
                    s.terminal.take_completed_commands();
                    s.flush_responses();
                    s.refresh();
                }
            }
            Message::Terminal(super::Message::PtyExited(id, fd, status)) => {
                if self.session.as_ref().is_some_and(|s| s.id == id && s.master_fd == fd) {
                    self.child_exited = true;
                    self.invalidate_input();
                    if status == 0 { return iced::exit(); }
                    self.error = Some(format!("Vim exited with status {status}. The final screen is retained; Close now dismisses the workbench."));
                }
            }
            Message::Terminal(_) => {}
            Message::Keyboard(id, event, captured) => return self.key(id, event, captured),
            Message::Modifiers(modifiers) => self.modifiers = modifiers,
            Message::Ime(event) => {
                if !self.ime.admit(&event) { return Task::none(); }
                if let Some(s) = &mut self.session {
                    match event {
                        input_method::Event::Preedit(value, selection) => { s.terminal.preedit_text = value; s.terminal.preedit_selection = selection; }
                        input_method::Event::Commit(value) => {
                            self.native_enter.lock().claim_pressed();
                            s.terminal.preedit_text.clear(); s.terminal.preedit_selection = None;
                            if !s.write_pty(value.as_bytes()) { self.error = Some("Vim input queue is full".into()); }
                        }
                        input_method::Event::Closed => { s.terminal.preedit_text.clear(); s.terminal.preedit_selection = None; }
                        input_method::Event::Opened => {}
                    }
                }
            }
            Message::CancelledCommit => self.ime.retire_cancelled_commit(),
            Message::Focus(focused) => { self.retire_gestures(); self.focused = focused; self.invalidate_input(); }
            Message::Resize(size) => { self.size = size; self.resize(); }
            Message::Close => return self.close(),
            Message::Mouse(input) => return self.mouse(input),
            Message::Command(action) => self.send(action),
            Message::ShowForm(kind) => return self.form(kind),
            Message::FormInput(value) => { if let Some(form) = &mut self.form { form.value = bounded_input(value); } }
            Message::SubmitForm => self.submit(),
            Message::CancelForm => { self.form = None; self.invalidate_input(); self.native_enter.lock().claim_pressed(); }
            Message::Browse(path) => return self.browse(path),
            Message::Directory(epoch, result) => {
                self.scan_busy = false;
                if epoch == self.scan_epoch {
                    match result {
                        Ok((path, entries)) => { self.directory = path; self.entries = entries; }
                        Err(error) => self.error = Some(error),
                    }
                }
                if let Some((next, path)) = self.queued_scan.take() { return self.scan_task(next, path); }
            }
            Message::OpenFile(path) => self.send(Vim9Command::Open { path: path.to_string_lossy().into_owned(), line: None, column: None }),
            Message::Pasted(epoch, value) => {
                if epoch != self.input_epoch || !self.input_active() { return Task::none(); }
                if let (Some(s), Some(value)) = (&mut self.session, value) {
                    let paste = pty_input::encode_prompt_insert(&value, PasteModes { bracketed: s.terminal.is_bracketed_paste_enabled() }, PastePolicy::prompt_insert(UnbracketedMultiline::FirstLineOnly), false);
                    if !paste.is_empty() && !s.write_pty(&paste.bytes) { self.error = Some("Vim input queue is full; paste was refused".into()); }
                    else if paste.risk.truncated_to_first_line { self.error = Some("Only the first line was pasted because Vim has bracketed paste disabled".into()); }
                    else if paste.is_empty() && !value.is_empty() { self.error = Some("Paste refused by the terminal input safety policy".into()); }
                }
            }
            Message::ToggleFiles => { self.files_open = !self.files_open; self.resize(); }
            Message::ToggleDock => { self.dock_open = !self.dock_open; self.resize(); }
            Message::OutputTab(output) => self.output_tab = output,
            Message::DismissError => { self.error = None; if let Some(editor) = &self.editor { editor.clear_error(); } }
        }
        Task::none()
    }

    fn key(&mut self, id: iced::window::Id, event: keyboard::Event, captured: bool) -> Task<Message> {
        if captured {
            self.native_enter.lock().acknowledge_captured(id, &event);
            if self.form.is_some() && matches!(event, keyboard::Event::KeyPressed { key: keyboard::Key::Named(keyboard::key::Named::Escape), .. }) {
                self.form = None; self.invalidate_input();
            }
            return Task::none();
        }
        if self.native_enter.lock().acknowledge(id, &event) { return Task::none(); }
        if self.ime.claim_enter(&event) { self.native_enter.lock().claim_pressed(); return Task::none(); }
        let keyboard::Event::KeyPressed { key, location, modifiers, text: committed, .. } = event else { return Task::none(); };
        self.modifiers = modifiers;
        if self.form.is_some() {
            if key == keyboard::Key::Named(keyboard::key::Named::Escape) { self.form = None; self.invalidate_input(); }
            return Task::none();
        }
        if !self.input_active() { return Task::none(); }
        if modifiers.control() && modifiers.shift() && !modifiers.alt() && !modifiers.logo() {
            if let keyboard::Key::Character(value) = &key {
                match value.to_ascii_lowercase().as_str() {
                    "o" => return self.form(FormKind::Open),
                    "f" => return self.form(FormKind::Search),
                    "b" => return self.form(FormKind::Build),
                    "s" => { self.send(Vim9Command::SaveAll); return Task::none(); }
                    "v" => { let epoch = self.input_epoch; return iced::clipboard::read().map(move |value| Message::Pasted(epoch, value)); }
                    "c" => {
                        if let Some(value) = self.session.as_ref().and_then(|s| s.terminal.copy_selection()) {
                            if !super::block_mode::clipboard_multiline_is_unsafe(&value) { return iced::clipboard::write(value); }
                            self.error = Some("Selection contains terminal control or visual-spoofing characters".into());
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
            if let Some(bytes) = super::encode_key(&key, location, modifiers, committed.as_deref(), s.terminal.is_application_cursor_keys(), enhancements) {
                if !s.write_pty(&bytes) { self.error = Some("Vim input queue is full".into()); }
                s.scroll_to_bottom();
                s.terminal.clear_text_selection();
                s.refresh();
            }
        }
        Task::none()
    }

    fn mouse(&mut self, input: MouseInput) -> Task<Message> {
        if !self.input_active() { return Task::none(); }
        let Some(s) = &mut self.session else { return Task::none(); };
        match input {
            MouseInput::Press { col, row, button, shift, alt, count, app_eligible, .. } => {
                let app = s.terminal.is_mouse_enabled() && !shift && app_eligible;
                let mut gesture = super::TerminalMouseGesture { session_id: s.id, button, report_to_app: app, last_app_cell: None, consumed: false };
                if app {
                    if let Some((x, y)) = super::projected_live_grid_cell(&s.terminal, &s.projection, row, col) {
                        if let Some(bytes) = s.terminal.get_mouse_report(super::btn_code(button), x, y) {
                            if s.write_pty(&bytes) { gesture.last_app_cell = Some((x, y)); }
                        }
                    }
                } else {
                    match button {
                        MouseButton::Left => match count {
                            2 => s.terminal.select_word_in_projection(&s.projection, row, col),
                            n if n >= 3 => s.terminal.select_line_in_projection(&s.projection, row),
                            _ => s.terminal.start_selection_in_projection(&s.projection, (row, col), if alt { terminal::SelectionMode::Block } else { terminal::SelectionMode::Normal }),
                        },
                        MouseButton::Middle => { let epoch = self.input_epoch; return iced::clipboard::read_primary().map(move |value| Message::Pasted(epoch, value)); }
                        MouseButton::Right => {}
                    }
                }
                self.gestures[button.slot()] = Some(gesture);
            }
            MouseInput::Drag { col, row, count } => {
                let Some(gesture) = &mut self.gestures[MouseButton::Left.slot()] else { return Task::none(); };
                if gesture.report_to_app {
                    if s.terminal.is_mouse_motion_enabled() {
                        if let Some((x, y)) = super::projected_live_grid_cell(&s.terminal, &s.projection, row, col) {
                            if let Some(bytes) = s.terminal.get_mouse_report(32, x, y) { if s.write_pty(&bytes) { gesture.last_app_cell = Some((x, y)); } }
                        }
                    }
                } else {
                    match count {
                        2 => s.terminal.extend_word_selection_in_projection(&s.projection, row, col),
                        n if n >= 3 => s.terminal.extend_line_selection_in_projection(&s.projection, row),
                        _ => s.terminal.update_selection_in_projection(&s.projection, (row, col)),
                    }
                }
            }
            MouseInput::Release { col, row, button } => {
                let Some(gesture) = self.gestures[button.slot()].take() else { return Task::none(); };
                if gesture.report_to_app {
                    let cell = super::projected_live_grid_cell(&s.terminal, &s.projection, row, col).or(gesture.last_app_cell);
                    if let Some((x, y)) = cell {
                        if let Some(bytes) = s.terminal.get_mouse_release_report(super::btn_code(button), x, y) { s.write_pty(&bytes); }
                    }
                }
            }
            MouseInput::Wheel { col, row, up, ctrl, shift, lines, app_eligible, .. } => {
                if ctrl {
                    let step = if up { 1.0 } else { -1.0 };
                    self.config.font_size = Config::clamp_font_size(self.config.font_size + step);
                    self.metrics = Metrics::with_font(self.mono, self.config.font_size, self.config.line_spacing, self.config.padding, false);
                    self.resize();
                    return Task::none();
                }
                if s.terminal.is_mouse_enabled() && !shift && app_eligible {
                    if let Some((x, y)) = super::projected_live_grid_cell(&s.terminal, &s.projection, row, col) {
                        for _ in 0..lines.clamp(1, 32) {
                            if let Some(bytes) = s.terminal.get_mouse_report(if up { 64 } else { 65 }, x, y) { s.write_pty(&bytes); }
                        }
                    }
                } else {
                    let step = self.config.scroll_speed.max(1) as isize * lines.clamp(1, 32) as isize;
                    s.scroll(if up { step } else { -step });
                }
            }
            MouseInput::ScrollTo { offset } => s.set_scroll_offset(offset),
        }
        if let Some(s) = &mut self.session { s.refresh(); }
        Task::none()
    }

    fn subscription(&self) -> Subscription<Message> {
        let mut subscriptions = vec![
            iced::event::listen_with(|event, status, id| match event {
                iced::Event::Keyboard(keyboard::Event::ModifiersChanged(value)) => Some(Message::Modifiers(value)),
                iced::Event::Keyboard(event) => Some(Message::Keyboard(id, event, status == iced::event::Status::Captured)),
                iced::Event::InputMethod(input_method::Event::Commit(_)) if status == iced::event::Status::Captured => Some(Message::CancelledCommit),
                iced::Event::InputMethod(_) if status == iced::event::Status::Captured => None,
                iced::Event::InputMethod(event) => Some(Message::Ime(event)),
                iced::Event::Window(iced::window::Event::Resized(size)) => Some(Message::Resize(size)),
                iced::Event::Window(iced::window::Event::Focused) => Some(Message::Focus(true)),
                iced::Event::Window(iced::window::Event::Unfocused) => Some(Message::Focus(false)),
                iced::Event::Window(iced::window::Event::CloseRequested) => Some(Message::Close),
                _ => None,
            }),
            iced::time::every(Duration::from_millis(if self.focused { 80 } else { 250 })).map(|_| Message::Tick),
        ];
        if let Some(s) = self.session.as_ref().filter(|_| !self.child_exited) {
            subscriptions.push(super::pty_subscription(super::PtySubscriptionKey {
                id: s.id, master_fd: s.master_fd, reader_fd: s.reader_fd.clone(), watch_source: s.watch_source.clone(), child_pid: s.pty.get_child_pid(),
            }).map(Message::Terminal));
            if s.has_pending_write() { subscriptions.push(iced::time::every(Duration::from_millis(8)).map(|_| Message::Drain)); }
        }
        Subscription::batch(subscriptions)
    }

    fn command_button(&self, label: &'static str, command: Vim9Command) -> Element<'_, Message> {
        button(text(label).size(13)).padding([4, 8]).on_press_maybe(self.snapshot.connected.then_some(Message::Command(command))).into()
    }
    fn optional_button(&self, label: &'static str, capability: &str, command: Vim9Command) -> Element<'_, Message> {
        button(text(label).size(13)).padding([4, 8]).on_press_maybe((self.snapshot.connected && self.snapshot.capabilities.iter().any(|c| c == capability)).then_some(Message::Command(command))).into()
    }
    fn strip<'a>(&self, content: Element<'a, Message>, height: f32) -> Element<'a, Message> {
        container(scrollable(content).direction(scrollable::Direction::Horizontal(Default::default())))
            .padding([3, 6]).height(Length::Fixed(height)).width(Length::Fill).into()
    }
    fn terminal_view(&self) -> Element<'_, Message> {
        let Some(s) = &self.session else {
            return container(text(self.error.as_deref().unwrap_or("Vim could not be launched. Install Vim 9 with +channel, +job, +timers and +quickfix."))).padding(24).center_x(Length::Fill).center_y(Length::Fill).into();
        };
        let selection = if s.terminal.has_text_selection() {
            (0..s.projection.cells().len()).map(|row| s.terminal.row_selection_cols_in_projection(&s.projection, row)).collect()
        } else { Vec::new() };
        TermWidget::new(s.projection.cells(), s.cursor, s.cursor_visible, s.terminal.cursor_shape, self.input_active(), &self.theme, self.metrics, self.mono, self.cjk, self.symbol, self.math, self.nerd, selection, s.projection.scroll_offset(), s.projection.max_scroll_offset())
            .interaction_mode(config::TerminalMode::Normal, 0)
            .modifiers(self.modifiers.shift(), self.modifiers.alt(), self.modifiers.control())
            .scrollbar_always(true)
            .app_mouse_full_grid(true)
            .dynamic_palette(&s.terminal.dynamic_palette)
            .dynamic_defaults(s.terminal.dynamic_fg, s.terminal.dynamic_bg, s.terminal.dynamic_cursor_color)
            .preedit((self.input_active() && !s.terminal.preedit_text.is_empty()).then(|| (s.terminal.preedit_text.clone(), s.terminal.preedit_selection.clone())))
            .blink_on(self.blink_on)
            .opacity(1.0)
            .on_mouse(Message::Mouse)
            .into()
    }
    fn files(&self) -> Element<'_, Message> {
        let parent = self.directory.parent().filter(|path| path.starts_with(&self.root)).map(Path::to_path_buf);
        let heading = row![text("PROJECT").size(13), Space::new().width(Length::Fill), button(text("Up").size(12)).on_press_maybe(parent.map(Message::Browse)), button(text("↻").size(13)).on_press(Message::Browse(self.directory.clone()))].spacing(4);
        let mut listing = column![heading, text(visible_label(&self.directory.to_string_lossy())).size(11)].spacing(4);
        if self.scan_busy { listing = listing.push(text("Loading…").size(12)); }
        for entry in &self.entries {
            let action = if entry.is_dir { Message::Browse(entry.path.clone()) } else { Message::OpenFile(entry.path.clone()) };
            let name = format!("{} {}", if entry.is_dir { "+" } else { " " }, visible_label(&entry.name));
            listing = listing.push(button(text(name).size(13)).width(Length::Fill).padding([3, 5]).style(button::text).on_press(action));
        }
        if self.entries.len() >= 1024 { listing = listing.push(text("Directory listing capped at 1,024 entries").size(11)); }
        container(scrollable(listing)).padding(8).width(Length::Fixed(SIDEBAR)).height(Length::Fill).into()
    }
    fn dock(&self) -> Element<'_, Message> {
        let tabs = row![button(text(format!("Problems ({}){}", self.snapshot.diagnostics.len(), if self.snapshot.diagnostics_truncated { "+" } else { "" })).size(12)).on_press(Message::OutputTab(false)), button(text("Build output").size(12)).on_press(Message::OutputTab(true))].spacing(5);
        let mut contents = column![].spacing(3);
        if let Some(error) = self.error.as_deref().or(self.snapshot.error.as_deref()) {
            contents = contents.push(row![text(visible_label(error)).size(12), button(text("Dismiss").size(12)).on_press(Message::DismissError)].spacing(8));
        }
        if self.output_tab {
            let build = &self.snapshot.build;
            contents = contents.push(text(format!("{}  {}", if build.running { "Running".to_owned() } else { build.exit_code.map(|v| format!("Exit {v}")).unwrap_or_else(|| "No build yet".into()) }, visible_label(&build.command))).size(12));
            for line in &build.output { contents = contents.push(text(visible_label(line)).font(iced::Font::MONOSPACE).size(12)); }
        } else {
            if self.snapshot.search.running { contents = contents.push(text("Searching project…").size(12)); }
            else if !self.snapshot.search.query.is_empty() { contents = contents.push(text(format!("Search: {} retained, {} omitted", self.snapshot.search.count, self.snapshot.search.truncated)).size(12)); }
            for issue in &self.snapshot.diagnostics {
                let title = format!("{} {}:{}:{}  {}", issue.kind, visible_label(&issue.filename), issue.line, issue.column, visible_label(&issue.text));
                let action = (!issue.filename.is_empty()).then_some(Message::Command(Vim9Command::Open { path: issue.filename.clone(), line: Some(issue.line.max(1)), column: Some(issue.column.max(1)) }));
                contents = contents.push(button(text(title).size(12)).padding([3, 5]).style(button::text).on_press_maybe(action));
            }
            if self.snapshot.diagnostics.is_empty() { contents = contents.push(text("No diagnostics. Build or search to populate Vim quickfix.").size(12)); }
        }
        container(column![tabs, scrollable(contents).height(Length::Fill)].spacing(5)).padding([5, 8]).height(Length::Fixed(DOCK)).width(Length::Fill).into()
    }
    fn view(&self) -> Element<'_, Message> {
        let header = row![
            text("FROST / VIM9").size(15),
            button(text("Files").size(13)).on_press(Message::ToggleFiles),
            button(text("Open…").size(13)).on_press(Message::ShowForm(FormKind::Open)),
            self.command_button("Save", Vim9Command::Save), self.command_button("Save all", Vim9Command::SaveAll),
            button(text("Search…").size(13)).on_press(Message::ShowForm(FormKind::Search)),
            button(text("Build…").size(13)).on_press(Message::ShowForm(FormKind::Build)),
            self.command_button("Stop build", Vim9Command::CancelBuild),
            button(text("Problems / output").size(13)).on_press(Message::ToggleDock),
            button(text("Close").size(13)).on_press(Message::Close)
        ].spacing(5).align_y(iced::Alignment::Center);
        let tools = row![
            self.optional_button("Definition", "definition", Vim9Command::Definition),
            self.optional_button("Hover", "hover", Vim9Command::Hover),
            self.optional_button("References", "references", Vim9Command::References),
            button(text("Rename…").size(13)).padding([4,8]).on_press_maybe((self.snapshot.connected && self.snapshot.capabilities.iter().any(|c| c == "rename")).then_some(Message::ShowForm(FormKind::Rename))),
            self.optional_button("Format", "format", Vim9Command::Format),
            self.command_button("Previous problem", Vim9Command::PreviousDiagnostic),
            self.command_button("Next problem", Vim9Command::NextDiagnostic),
            text("Vim owns editing, mappings and undo").size(12)
        ].spacing(5).align_y(iced::Alignment::Center);
        let mut buffers = row![].spacing(4);
        for buffer in &self.snapshot.buffers {
            let name = Path::new(&buffer.name).file_name().and_then(|s| s.to_str()).filter(|s| !s.is_empty()).unwrap_or("[No Name]");
            let label = format!("{}{}{}", if buffer.current { "> " } else { "" }, visible_label(name), if buffer.modified { " *" } else { "" });
            buffers = buffers.push(button(text(label).size(12)).on_press(Message::Command(Vim9Command::Buffer { buffer: buffer.id })));
        }
        if self.snapshot.buffers_truncated { buffers = buffers.push(text("More buffers in :ls").size(12)); }
        let (side, vertical) = chrome(self.size, self.files_open, self.dock_open);
        let mut middle = row![];
        if side > 0.0 { middle = middle.push(self.files()); }
        middle = middle.push(container(self.terminal_view()).width(Length::Fill).height(Length::Fill));
        let mut body = column![self.strip(header.into(), HEADER), self.strip(tools.into(), TOOLS), self.strip(buffers.into(), BUFFERS), middle.height(Length::Fill)];
        if vertical > HEADER + TOOLS + BUFFERS + STATUS { body = body.push(self.dock()); }
        let state = if self.child_exited { "EXITED" } else if self.snapshot.connected { "CONNECTED" } else { "BRIDGE OFFLINE" };
        let status = format!("{state} | {} | {}{} | {}:{} | {} pending{}", visible_label(&self.snapshot.mode), visible_label(&self.snapshot.file), if self.snapshot.modified { " *" } else { "" }, self.snapshot.line, self.snapshot.column, self.snapshot.pending, if self.error.is_some() || self.snapshot.error.is_some() { " | Error: open Problems / output" } else { "" });
        body = body.push(container(text(status).size(12)).padding([4, 8]).height(Length::Fixed(STATUS)).width(Length::Fill).clip(true));
        let base: Element<'_, Message> = container(body).width(Length::Fill).height(Length::Fill).into();
        let Some(form) = &self.form else { return base; };
        let field = text_input(form.kind.label(), &form.value).id(INPUT_ID).on_input(Message::FormInput).on_submit(Message::SubmitForm).size(15).padding(8);
        let dialog = container(column![text(form.kind.label()).size(20), text(form.kind.help()).size(13), field, row![button("Apply").on_press(Message::SubmitForm), button("Cancel").on_press(Message::CancelForm)].spacing(8)].spacing(12))
            .padding(20).width(Length::Fixed((self.size.width - 32.0).clamp(1.0, 580.0))).style(container::rounded_box);
        let scrim = mouse_area(container(Space::new()).width(Length::Fill).height(Length::Fill)).on_press(Message::CancelForm);
        stack![base, Element::from(scrim), container(dialog).center_x(Length::Fill).center_y(Length::Fill)].into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(values: &[&str]) -> Vec<OsString> { values.iter().map(OsString::from).collect() }
    #[test]
    fn launch_contract_preserves_plain_terminal_and_literal_paths() {
        assert_eq!(target_from_args(&[]).unwrap(), None);
        assert_eq!(target_from_args(&args(&["--ide"])).unwrap(), Some(PathBuf::from(".")));
        assert_eq!(target_from_args(&args(&["--ide", "a path|with $syntax.rs"])).unwrap(), Some(PathBuf::from("a path|with $syntax.rs")));
        assert_eq!(target_from_args(&args(&["--ide", "--", "-file"])).unwrap(), Some(PathBuf::from("-file")));
        for values in [vec!["--ide", "a", "b"], vec!["--ide", "--execute", "sh"], vec!["--mode", "block", "--ide"]] { assert!(target_from_args(&args(&values)).is_err()); }
    }
    #[test]
    fn chrome_reserves_only_visible_panels_and_keeps_small_windows_usable() {
        assert_eq!(chrome(Size::new(480.0, 320.0), true, true), (0.0, 136.0));
        assert_eq!(chrome(Size::new(1000.0, 700.0), true, true), (250.0, 336.0));
        assert_eq!(chrome(Size::new(1000.0, 700.0), false, false), (0.0, 136.0));
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
    fn startup_and_closing_do_not_own_shell_persistence_or_force_editor_exit() {
        let source = include_str!("vim9_ide.rs").split("#[cfg(test)]").next().unwrap();
        assert!(!source.contains("save_session_snapshot("));
        assert!(!source.contains("try_acquire_instance_lock("));
        assert!(!source.contains("Frost::new("));
        assert!(source.contains("self.session.is_none() || self.child_exited"));
        assert!(source.contains("self.send(Vim9Command::Quit)"));
        assert!(!source.contains("qa!"));
    }
}
