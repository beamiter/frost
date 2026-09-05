use crate::persistence::{self, AtomicWriteError, FileRevision};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const MAX_CONFIG_BYTES: u64 = 1024 * 1024;
const MAX_CONFIG_NAME_BYTES: usize = 256;
const MAX_CONFIG_VALUE_BYTES: usize = 4 * 1024;
const MAX_REMOTE_SSH_ARGS: usize = 64;
const MAX_REMOTE_PROFILE_BYTES: usize = 64 * 1024;
pub(crate) const MAX_REMOTE_HOSTS: usize = 128;
/// Bounded rendering budget. The stored list remains untouched, and entry 129
/// stays visible with the active-limit diagnostic.
pub(crate) const MAX_REMOTE_HOST_UI_ROWS: usize = 256;

/// A configuration load outcome keeps the usable value separate from any
/// diagnostic. This lets the application start with safe defaults while also
/// preventing a malformed user file from being silently overwritten.
#[derive(Debug, Clone)]
pub struct ConfigLoad {
    pub config: Config,
    pub diagnostic: Option<String>,
    /// Exact bytes the usable/default value was loaded against. `None` means
    /// the file could not be inspected, so optimistic writes must stay off.
    pub revision: Option<FileRevision>,
}

// Nerd Font priority list
const NERD_FONT_CANDIDATES: &[&str] = &[
    "SauceCodePro Nerd Font",
    "SauceCodePro Nerd Font Mono",
    "Monokoi Nerd Font",
    "Monokoi Nerd Font Mono",
    "JetBrains Mono Nerd Font",
    "JetBrains Mono NF",
    "JetBrainsMono Nerd Font",
    "FiraCode Nerd Font",
];

const NERD_FONT_FALLBACK_CANDIDATES: &[&str] = &[
    "SauceCodePro Nerd Font Mono",
    "JetBrainsMono Nerd Font Mono",
    "JetBrains Mono Nerd Font",
    "JetBrainsMono Nerd Font",
    "SauceCodePro Nerd Font",
    "Monokoi Nerd Font Mono",
    "Monokoi Nerd Font",
    "FiraCode Nerd Font",
];

const MATH_SYMBOL_FONT_CANDIDATES: &[&str] =
    &["Noto Sans Math", "Noto Sans Symbols2", "OpenSymbol"];

static MONOSPACE_FONTS: Lazy<Vec<String>> = Lazy::new(|| {
    eprintln!("[Config] Scanning monospace fonts (one-time)...");
    detect_fonts_by_query(&[":spacing=100"])
});

static CJK_MONOSPACE_FONT: Lazy<Option<String>> = Lazy::new(|| {
    eprintln!("[Config] Resolving CJK monospace fallback font...");
    detect_font_by_match(&["monospace:lang=zh-cn"])
});

static SYMBOL_MONOSPACE_FONT: Lazy<Option<String>> = Lazy::new(|| {
    eprintln!("[Config] Resolving terminal symbol fallback font...");
    detect_font_by_match(&["monospace:charset=2303"])
});

static MATH_SYMBOL_FONT: Lazy<Option<String>> = Lazy::new(|| {
    eprintln!("[Config] Resolving math symbol fallback font...");
    detect_preferred_font(MATH_SYMBOL_FONT_CANDIDATES)
        .or_else(|| detect_font_by_match(&["monospace:charset=1D7CF"]))
});

static NERD_SYMBOL_FONT: Lazy<Option<String>> = Lazy::new(|| {
    eprintln!("[Config] Resolving Nerd Font symbol fallback...");
    detect_preferred_font(NERD_FONT_FALLBACK_CANDIDATES)
});

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum FontBackendType {
    #[default]
    Fontdue,
    AbGlyph,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum AppRendererType {
    #[default]
    Glow,
    Wgpu,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum ScrollbarVisibility {
    Auto,
    #[default]
    Always,
}

/// Where the session tab strip is rendered.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum TabPosition {
    /// Horizontal tab strip across the top of the window.
    #[default]
    Top,
    /// Vertical tab list docked in the left sidebar.
    Side,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// AI features master switch. Off by default: nothing leaves the machine
    /// unless the user opts in.
    #[serde(default)]
    pub ai_enabled: bool,

    /// Remote destinations for the host picker (Ctrl+Shift+S). Grammar,
    /// validation and the argv a tab runs are the family-shared
    /// `jterm_core::jsh_remote::RemoteHostConfig`. A file with no key at all
    /// gets [`default_remote_hosts`] — two worked entries to copy; an explicit
    /// list, `[]` included, is taken as written.
    #[serde(default = "default_remote_hosts")]
    pub remote_hosts: Vec<jterm_core::jsh_remote::RemoteHostConfig>,

    /// AI provider: "anthropic", "openai-compatible", or "ollama".
    #[serde(default = "default_ai_provider")]
    pub ai_provider: String,

    #[serde(default = "default_ai_base_url")]
    pub ai_base_url: String,

    #[serde(default = "default_ai_model")]
    pub ai_model: String,

    #[serde(default = "default_ai_max_tokens")]
    pub ai_max_tokens: u32,

    /// 采样温度（None 使用 provider 默认；有效范围 0.0..=2.0）
    #[serde(default)]
    pub ai_temperature: Option<f32>,

    /// Scrub high-confidence secrets from AI-bound text (default on).
    #[serde(default = "default_ai_redact_secrets")]
    pub ai_redact_secrets: bool,

    /// Stream model replies into the Agent panel as they are generated
    /// (default on). Off falls back to one blocking request per turn; the
    /// recorded transcript is identical either way.
    #[serde(default = "default_ai_stream")]
    pub ai_stream: bool,

    /// Optional path to a 0600 file holding the provider API key, so the key
    /// never has to live in the process environment or this config file.
    #[serde(default)]
    pub ai_api_key_file: Option<String>,

    /// Turn budget for one Agent-mode session.
    #[serde(default = "default_agent_max_turns")]
    pub agent_max_turns: u32,

    /// Explicit consent for sending semantic terminal context (command, cwd,
    /// and captured output) to a non-local AI provider. Local Ollama requests
    /// sent directly to a loopback Ollama endpoint do not require this
    /// opt-in; an inherited HTTP proxy disables that exemption.
    #[serde(default)]
    pub ai_share_command_context: bool,

    /// Offer a review-first correction card when a Block-mode command fails
    /// with a narrowly classified cause (default off; requires `ai_enabled`).
    /// Verified local evidence never needs a provider; the strict-JSON AI
    /// fallback is skipped entirely when none is configured.
    #[serde(default)]
    pub command_correction_enabled: bool,

    /// Show the provider-neutral Tasks dashboard. This is independent from
    /// cloud-AI consent because local Agent CLIs and task bookkeeping do not
    /// inherently send terminal context off-machine.
    #[serde(default)]
    pub experimental_task_sidebar: bool,

    #[serde(default = "default_font_size")]
    pub font_size: f32,

    #[serde(default = "default_font_family")]
    pub font_family: String,

    #[serde(default = "default_font_weight")]
    pub font_weight: f32,

    #[serde(default = "default_font_sharpness")]
    pub font_sharpness: f32,

    #[serde(default)]
    pub font_backend: FontBackendType,

    #[serde(default = "default_padding")]
    pub padding: f32,

    #[serde(default = "default_line_spacing")]
    pub line_spacing: f32,

    #[serde(default)]
    pub scrollbar_visibility: ScrollbarVisibility,

    #[serde(default)]
    pub tab_position: TabPosition,

    #[serde(default = "default_scrollback_lines")]
    pub scrollback_lines: usize,

    #[serde(default = "default_initial_width")]
    pub initial_width: f32,

    #[serde(default = "default_initial_height")]
    pub initial_height: f32,

    #[serde(default = "default_cols")]
    pub cols: usize,

    #[serde(default = "default_rows")]
    pub rows: usize,

    #[serde(default = "default_theme")]
    pub theme: String,

    #[serde(default = "default_restore_session")]
    pub restore_session: bool,

    #[serde(default)]
    pub session_history_file: Option<PathBuf>,

    #[serde(default = "default_opacity")]
    pub opacity: f32,

    #[serde(default = "default_gpu_rendering")]
    pub gpu_rendering: bool,

    #[serde(default)]
    pub app_renderer: AppRendererType,

    #[serde(default = "default_scroll_speed")]
    pub scroll_speed: u32,

    #[serde(default)]
    pub disable_alt_screen: bool,

    #[serde(default)]
    pub ui_scale: Option<f32>,

    #[serde(default = "default_subpixel_rendering")]
    pub subpixel_rendering: bool,

    /// Explicit shell path (overrides auto-detection). Useful when PATH is stripped by launchers like wofi.
    #[serde(default)]
    pub shell: Option<String>,

    /// When to look for a newer jsh: "startup", "daily" (default) or "never".
    /// The check only decides whether the offer appears; installing always
    /// stays an explicit choice.
    #[serde(default = "default_jsh_update_check")]
    pub jsh_update_check: String,

    /// Permit applications running in the PTY to read the host clipboard via
    /// OSC 52 / OSC 5522. Disabled by default because this crosses the local /
    /// remote-shell trust boundary.
    #[serde(default)]
    pub allow_clipboard_read: bool,

    /// Permit applications running in the PTY to overwrite the host clipboard
    /// via an OSC 52 SET (`\e]52;c;<base64>\e\\`).
    ///
    /// Off by default, matching anvil and forge's `allow_remote_clipboard_write`
    /// and ember's `osc52_clipboard_write`. frost was the one app of the four
    /// that let PTY output replace the clipboard unconditionally: any program
    /// in the terminal, including one on the far side of an ssh connection,
    /// could silently swap what the user's next paste would produce — into a
    /// shell, into a password field, anywhere. Reads were already gated; a
    /// write crosses the same trust boundary in the other direction.
    #[serde(default)]
    pub allow_remote_clipboard_write: bool,

    /// Post a desktop notification when a command tracked via OSC 133 runs
    /// longer than `notify_long_block_threshold_ms` and finishes while the
    /// user is not watching that pane (window unfocused or pane inactive).
    #[serde(default = "default_notify_long_blocks")]
    pub notify_long_blocks: bool,

    /// Threshold (in milliseconds) above which `notify_long_blocks` fires.
    #[serde(default = "default_notify_long_block_threshold_ms")]
    pub notify_long_block_threshold_ms: u64,

    /// Show each pane's git branch and dirty marker in its header strip.
    #[serde(default = "default_show_repo_strip")]
    pub show_repo_strip: bool,

    /// Show the family-wide bottom status bar (cwd, git, last command, grid
    /// size, tabs). Same `bottom_bar` key and default in every jterm.
    #[serde(default = "default_bottom_bar")]
    pub bottom_bar: bool,

    /// A plain click in the terminal places the shell's edit cursor there.
    /// Same `click_moves_cursor` key and default in every jterm; the move
    /// itself is computed by `jterm_core::click_cursor`.
    #[serde(default = "default_click_moves_cursor")]
    pub click_moves_cursor: bool,

    /// Draw theme-relative command cards over OSC 133 zones: rounded/tinted
    /// surfaces, outcome stripes, selection outlines and first-row status /
    /// duration badges. Same `block_mode` key and default as the family.
    #[serde(default = "default_block_mode")]
    pub block_mode: bool,

    /// Paint the same Block cards with the family's compact 4px inset / 6px
    /// radius. Frost's continuous terminal grid does not change dimensions;
    /// this is a presentation-only density switch.
    #[serde(default)]
    pub block_compact: bool,

    /// Append each OSC 133 completed command to the family-shared JSONL
    /// history index (same keys and file format as anvil/forge), so the
    /// Ctrl+Shift+H picker can recall commands across restarts. Only the
    /// command line, cwd, exit code, and end time are stored — never output.
    #[serde(default = "default_command_history_enabled")]
    pub command_history_enabled: bool,

    /// History index location. Defaults to the XDG state directory
    /// (`~/.local/state/frost/history.jsonl`); point siblings at one file to
    /// share history between them.
    #[serde(default)]
    pub command_history_path: Option<PathBuf>,

    /// Entries kept when the index compacts.
    #[serde(default = "default_command_history_max_entries")]
    pub command_history_max_entries: u32,
}

fn default_ai_provider() -> String {
    "anthropic".to_string()
}

fn default_ai_base_url() -> String {
    "https://api.anthropic.com".to_string()
}

fn default_ai_model() -> String {
    "claude-sonnet-4-6".to_string()
}

fn default_ai_max_tokens() -> u32 {
    1_024
}

fn default_ai_redact_secrets() -> bool {
    true
}

fn default_ai_stream() -> bool {
    true
}

fn default_agent_max_turns() -> u32 {
    20
}

/// Two worked entries a new destination can be copied from: one ssh target and
/// one running container. They exist because the two mistakes the grammar
/// cannot forgive are invisible in an empty list — the port belongs in
/// `ssh_args`, never as `host:port`, and the login belongs in `user`, never as
/// a `user@host` string that ssh would take literally as a hostname.
///
/// Only consulted when the file has no `remote_hosts` key at all. An explicit
/// list — including `remote_hosts = []` — always wins, so hosts deleted in the
/// settings panel (which writes the key back) stay deleted.
pub fn default_remote_hosts() -> Vec<jterm_core::jsh_remote::RemoteHostConfig> {
    vec![
        jterm_core::jsh_remote::RemoteHostConfig {
            name: "dev-60".to_string(),
            host: "10.68.18.60".to_string(),
            user: Some("root".to_string()),
            docker: false,
            remote_shell: "jsh".to_string(),
            session: None,
            // 22 is ssh's default and could be omitted; it is spelled out so a
            // copied entry has the flag to change rather than one to remember.
            ssh_args: vec!["-p".to_string(), "22".to_string()],
            deploy: "persist".to_string(),
            deploy_artifact: None,
        },
        jterm_core::jsh_remote::RemoteHostConfig {
            name: "myubuntu".to_string(),
            host: "myubuntu".to_string(),
            // The container user is `docker exec -u`; unset means the image's.
            user: None,
            docker: true,
            remote_shell: "jsh".to_string(),
            session: None,
            // Meaningless for docker, and the launcher ignores them.
            ssh_args: Vec::new(),
            deploy: "persist".to_string(),
            deploy_artifact: None,
        },
    ]
}

fn default_font_size() -> f32 {
    14.0
}

fn default_font_weight() -> f32 {
    1.0
}

fn default_font_sharpness() -> f32 {
    1.0
}

fn default_line_spacing() -> f32 {
    1.0
}

fn detect_fonts_by_query(extra_args: &[&str]) -> Vec<String> {
    let mut args = Vec::from(extra_args);
    args.push("family");
    if let Ok(output) = jterm_core::helper::fc_list(&args) {
        if !output.status.success() {
            return Vec::new();
        }
        if let Ok(stdout) = String::from_utf8(output.stdout) {
            let mut seen = std::collections::HashSet::new();
            let mut families: Vec<String> = stdout
                .lines()
                .filter_map(|line| {
                    let family = line.split(',').next()?.trim();
                    if family.is_empty() {
                        return None;
                    }
                    if seen.insert(family.to_lowercase()) {
                        Some(family.to_string())
                    } else {
                        None
                    }
                })
                .collect();
            families.sort_by_key(|a| a.to_lowercase());
            return families;
        }
    }
    Vec::new()
}

fn detect_font_by_match(args: &[&str]) -> Option<String> {
    let mut helper_args = Vec::from(args);
    helper_args.push("family");
    let output = jterm_core::helper::fc_match(&helper_args)
        .ok()
        .filter(|output| output.status.success())?;
    let stdout = String::from_utf8(output.stdout).ok()?;
    stdout
        .lines()
        .find_map(|line| {
            line.split(',')
                .next()
                .map(str::trim)
                .filter(|f| !f.is_empty())
        })
        .map(ToOwned::to_owned)
}

fn detect_preferred_font(candidates: &[&str]) -> Option<String> {
    for candidate in candidates {
        let output = jterm_core::helper::fc_match(&["-f", "%{family}\n", candidate])
            .ok()
            .filter(|output| output.status.success())?;
        let stdout = String::from_utf8(output.stdout).ok()?;
        let line = stdout.lines().next()?.trim();
        let line_lower = line.to_lowercase();
        if line_lower
            .split(',')
            .map(str::trim)
            .any(|family| family == candidate.to_lowercase())
        {
            return line.split(',').next().map(str::trim).map(ToOwned::to_owned);
        }
    }
    None
}

fn detect_monospace_fonts() -> &'static Vec<String> {
    &MONOSPACE_FONTS
}

fn default_font_family() -> String {
    // 快速路径：直接使用第一个候选字体，不检测系统字体
    // 这避免了启动时的 fc-list 调用，加快启动速度
    // 字体检测会在用户打开配置面板时延迟进行
    eprintln!(
        "[Config] Using default font (no scan): {}",
        NERD_FONT_CANDIDATES[0]
    );
    NERD_FONT_CANDIDATES[0].to_string()

    // 原有的检测逻辑已移除，避免启动时阻塞
    // 如需验证字体存在性，可在配置面板中按需检测
}

fn default_padding() -> f32 {
    2.0
}

fn default_scrollback_lines() -> usize {
    10000
}

fn default_initial_width() -> f32 {
    1200.0
}

fn default_initial_height() -> f32 {
    600.0
}

fn default_cols() -> usize {
    100
}

fn default_rows() -> usize {
    30
}

fn default_theme() -> String {
    "dark".to_string()
}

fn default_restore_session() -> bool {
    true
}

fn default_opacity() -> f32 {
    1.0
}

fn default_gpu_rendering() -> bool {
    true
}

fn default_scroll_speed() -> u32 {
    3
}

fn default_subpixel_rendering() -> bool {
    true
}

fn default_notify_long_blocks() -> bool {
    true
}

fn default_notify_long_block_threshold_ms() -> u64 {
    10_000
}

fn default_show_repo_strip() -> bool {
    true
}

fn default_bottom_bar() -> bool {
    jterm_core::bottom_bar::ENABLED_BY_DEFAULT
}

fn default_click_moves_cursor() -> bool {
    jterm_core::click_cursor::ENABLED_BY_DEFAULT
}

fn default_block_mode() -> bool {
    true
}

fn default_command_history_enabled() -> bool {
    true
}

fn default_command_history_max_entries() -> u32 {
    10_000
}

fn default_jsh_update_check() -> String {
    jterm_core::jsh_install::UpdateCheck::default()
        .as_str()
        .to_string()
}

impl Default for Config {
    fn default() -> Self {
        Config {
            jsh_update_check: default_jsh_update_check(),
            ai_enabled: false,
            remote_hosts: default_remote_hosts(),
            ai_provider: default_ai_provider(),
            ai_base_url: default_ai_base_url(),
            ai_model: default_ai_model(),
            ai_max_tokens: default_ai_max_tokens(),
            ai_temperature: None,
            ai_redact_secrets: default_ai_redact_secrets(),
            ai_stream: default_ai_stream(),
            ai_api_key_file: None,
            agent_max_turns: default_agent_max_turns(),
            ai_share_command_context: false,
            command_correction_enabled: false,
            experimental_task_sidebar: false,
            font_size: default_font_size(),
            font_family: default_font_family(),
            font_weight: default_font_weight(),
            font_sharpness: default_font_sharpness(),
            font_backend: FontBackendType::default(),
            padding: default_padding(),
            line_spacing: default_line_spacing(),
            scrollbar_visibility: ScrollbarVisibility::default(),
            tab_position: TabPosition::default(),
            scrollback_lines: default_scrollback_lines(),
            initial_width: default_initial_width(),
            initial_height: default_initial_height(),
            cols: default_cols(),
            rows: default_rows(),
            theme: default_theme(),
            restore_session: default_restore_session(),
            session_history_file: None,
            opacity: default_opacity(),
            gpu_rendering: default_gpu_rendering(),
            app_renderer: AppRendererType::default(),
            scroll_speed: default_scroll_speed(),
            disable_alt_screen: false,
            subpixel_rendering: default_subpixel_rendering(),
            ui_scale: None,
            shell: None,
            allow_clipboard_read: false,
            allow_remote_clipboard_write: false,
            notify_long_blocks: default_notify_long_blocks(),
            notify_long_block_threshold_ms: default_notify_long_block_threshold_ms(),
            show_repo_strip: default_show_repo_strip(),
            bottom_bar: default_bottom_bar(),
            click_moves_cursor: default_click_moves_cursor(),
            block_mode: default_block_mode(),
            block_compact: false,
            command_history_enabled: default_command_history_enabled(),
            command_history_path: None,
            command_history_max_entries: default_command_history_max_entries(),
        }
    }
}

impl Config {
    /// Parse and normalize configuration from TOML. Keeping this as the single
    /// path ensures startup and live reload enforce identical bounds.
    pub fn from_toml(content: &str) -> Result<Self, toml::de::Error> {
        toml::from_str::<Config>(content).map(Self::normalized)
    }

    pub(crate) fn normalized(mut self) -> Self {
        self.font_size = Self::clamp_font_size(self.font_size);
        self.line_spacing = Self::clamp_line_spacing(self.line_spacing);
        self.padding = Self::clamp_padding(self.padding);
        self.scrollback_lines = Self::clamp_scrollback_lines(self.scrollback_lines);
        self.scroll_speed = Self::clamp_scroll_speed(self.scroll_speed);
        self.opacity = Self::clamp_opacity(self.opacity);
        self.font_weight = finite_clamp(self.font_weight, default_font_weight(), 0.1, 2.0);
        self.font_sharpness = finite_clamp(self.font_sharpness, default_font_sharpness(), 0.1, 2.0);
        self.initial_width =
            finite_clamp(self.initial_width, default_initial_width(), 320.0, 16_384.0);
        self.initial_height = finite_clamp(
            self.initial_height,
            default_initial_height(),
            200.0,
            16_384.0,
        );
        self.cols = self.cols.clamp(1, crate::terminal::MAX_TERMINAL_COLS);
        self.rows = self.rows.clamp(1, crate::terminal::MAX_TERMINAL_ROWS);
        self.ui_scale = self
            .ui_scale
            .filter(|value| value.is_finite())
            .map(|value| value.clamp(0.5, 4.0));
        self.ai_max_tokens = self.ai_max_tokens.clamp(64, 32_768);
        self.ai_temperature = self
            .ai_temperature
            .filter(|value| value.is_finite() && (0.0..=2.0).contains(value));
        self.agent_max_turns = self.agent_max_turns.clamp(1, 100);
        self.ai_provider =
            normalized_text_or(self.ai_provider, MAX_CONFIG_NAME_BYTES, default_ai_provider);
        self.ai_base_url = normalized_text_or(
            self.ai_base_url,
            MAX_CONFIG_VALUE_BYTES,
            default_ai_base_url,
        );
        self.ai_model = normalized_text_or(self.ai_model, MAX_CONFIG_NAME_BYTES, default_ai_model);
        self.ai_api_key_file =
            normalized_optional_text(self.ai_api_key_file, MAX_CONFIG_VALUE_BYTES);
        self.font_family = self.font_family.trim().to_string();
        if !valid_config_text(&self.font_family, MAX_CONFIG_NAME_BYTES) {
            self.font_family = default_font_family();
        }
        self.theme = self.theme.trim().to_string();
        if !valid_config_text(&self.theme, MAX_CONFIG_NAME_BYTES) {
            self.theme = default_theme();
        }
        self.shell = normalized_optional_text(self.shell, MAX_CONFIG_VALUE_BYTES);
        self.session_history_file = bounded_path(self.session_history_file);
        self.command_history_path = bounded_path(self.command_history_path);
        self.jsh_update_check = jterm_core::jsh_install::UpdateCheck::parse(&self.jsh_update_check)
            .as_str()
            .to_string();
        // Remote-host drafts are intentionally preserved verbatim. Invalid or
        // over-limit entries remain repairable in Settings and round-trip to
        // disk; every runtime consumer applies `validate_remote_host_at`
        // immediately before display/argv/process use.
        // Same retention bounds as anvil/forge apply to their shared index.
        self.command_history_max_entries = self.command_history_max_entries.clamp(100, 1_000_000);
        self
    }

    pub fn load_with_diagnostics() -> ConfigLoad {
        let config_path = match Self::config_path() {
            Ok(path) => path,
            Err(error) => {
                let diagnostic = format!("Cannot locate the frost config directory: {error}");
                eprintln!("[Config] {diagnostic}");
                return ConfigLoad {
                    config: Self::default(),
                    diagnostic: Some(diagnostic),
                    revision: None,
                };
            }
        };

        let revision = match persistence::read_revision(&config_path, MAX_CONFIG_BYTES) {
            Ok(revision) => revision,
            Err(error) => {
                let diagnostic = format!("Cannot read {}: {error}", config_path.display());
                eprintln!("[Config] {diagnostic}");
                return ConfigLoad {
                    config: Self::default(),
                    diagnostic: Some(diagnostic),
                    revision: None,
                };
            }
        };
        if revision == FileRevision::Missing {
            eprintln!("[Config] Using default configuration");
            return ConfigLoad {
                config: Self::default(),
                diagnostic: None,
                revision: Some(revision),
            };
        }

        match Self::from_revision(&config_path, &revision) {
            Ok(config) => {
                eprintln!("[Config] Loaded from {}", config_path.display());
                eprintln!("[Config] Font: {}", config.font_family);
                ConfigLoad {
                    config,
                    diagnostic: None,
                    revision: Some(revision),
                }
            }
            Err(error) => {
                eprintln!("[Config] {error}");
                ConfigLoad {
                    config: Self::default(),
                    diagnostic: Some(error),
                    revision: Some(revision),
                }
            }
        }
    }

    /// Read and validate one configuration file with path-rich errors suitable
    /// for an in-app diagnostic.
    #[cfg(test)]
    pub fn load_path(path: &std::path::Path) -> Result<Self, String> {
        let revision = persistence::read_revision(path, MAX_CONFIG_BYTES)
            .map_err(|error| format!("Cannot read {}: {error}", path.display()))?;
        Self::from_revision(path, &revision)
    }

    /// Parse the exact bytes represented by a polled revision. Keeping parsing
    /// and the revision tied to one byte buffer avoids a check/read race during
    /// hot reload.
    pub fn from_revision(path: &std::path::Path, revision: &FileRevision) -> Result<Self, String> {
        let bytes = revision
            .bytes()
            .ok_or_else(|| format!("Cannot read {}: file does not exist", path.display()))?;
        let content = std::str::from_utf8(bytes)
            .map_err(|error| format!("Cannot read {} as UTF-8: {error}", path.display()))?;
        Self::from_toml(content)
            .map_err(|error| format!("Cannot parse {}: {error}", path.display()))
    }

    fn serialized(&self) -> Result<Vec<u8>, AtomicWriteError> {
        toml::to_string_pretty(&self.clone().normalized())
            .map(String::into_bytes)
            .map_err(|error| AtomicWriteError::Io(format!("serialize configuration: {error}")))
    }

    fn save_path_force(
        &self,
        config_path: &std::path::Path,
    ) -> Result<FileRevision, AtomicWriteError> {
        let content = self.serialized()?;
        persistence::write_atomic_private_force(config_path, &content, MAX_CONFIG_BYTES)
    }

    fn save_path_if_unchanged(
        &self,
        config_path: &std::path::Path,
        expected: Option<&FileRevision>,
    ) -> Result<FileRevision, AtomicWriteError> {
        let content = self.serialized()?;
        persistence::write_atomic_private_if_unchanged(
            config_path,
            &content,
            expected,
            MAX_CONFIG_BYTES,
        )
    }

    /// Unconditional save retained for the explicit default-config creation
    /// path. Interactive writes use `save_if_unchanged`; Reset is the only UI
    /// action allowed to force replacement after a malformed/conflicting file.
    pub fn save(&self) -> Result<(), Box<dyn std::error::Error>> {
        self.save_force_revision()?;
        Ok(())
    }

    pub fn save_force_revision(&self) -> Result<FileRevision, AtomicWriteError> {
        let config_path = Self::config_path()
            .map_err(|error| AtomicWriteError::Io(format!("locate configuration file: {error}")))?;
        let revision = self.save_path_force(&config_path)?;
        eprintln!("[Config] Saved to {}", config_path.display());
        Ok(revision)
    }

    pub fn save_if_unchanged(
        &self,
        expected: Option<&FileRevision>,
    ) -> Result<FileRevision, AtomicWriteError> {
        let config_path = Self::config_path()
            .map_err(|error| AtomicWriteError::Io(format!("locate configuration file: {error}")))?;
        let revision = self.save_path_if_unchanged(&config_path, expected)?;
        eprintln!("[Config] Saved to {}", config_path.display());
        Ok(revision)
    }

    pub fn session_history_path(&self) -> Result<PathBuf, Box<dyn std::error::Error>> {
        let config_dir = dirs::config_dir().ok_or("Failed to determine config directory")?;
        let default = config_dir.join("frost").join("session_history.json");
        let Some(path) = self.session_history_file.as_ref() else {
            return Ok(default);
        };
        if path.is_absolute() {
            return Ok(path.clone());
        }
        if let Ok(rest) = path.strip_prefix("~") {
            if let Some(home) = dirs::home_dir() {
                return Ok(home.join(rest));
            }
        }
        Ok(config_dir.join("frost").join(path))
    }

    /// Where the shared command-history index lives, or `None` while history
    /// recording is disabled. Explicit paths support `~` expansion; relative
    /// paths land beside the default so growing data stays out of the config
    /// directory. The default follows the family's XDG state-dir semantics.
    pub fn resolved_command_history_path(&self) -> Option<PathBuf> {
        if !self.command_history_enabled {
            return None;
        }
        let state_dir = dirs::state_dir()
            .or_else(|| dirs::home_dir().map(|home| home.join(".local/state")))?
            .join("frost");
        let Some(path) = self.command_history_path.as_ref() else {
            return Some(state_dir.join("history.jsonl"));
        };
        if path.is_absolute() {
            return Some(path.clone());
        }
        if let Ok(rest) = path.strip_prefix("~") {
            if let Some(home) = dirs::home_dir() {
                return Some(home.join(rest));
            }
        }
        Some(state_dir.join(path))
    }

    pub fn config_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
        let config_dir = dirs::config_dir().ok_or("Failed to determine config directory")?;
        Ok(config_dir.join("frost").join("config.toml"))
    }

    pub fn config_revision() -> Result<FileRevision, String> {
        let path = Self::config_path().map_err(|error| error.to_string())?;
        persistence::read_revision(&path, MAX_CONFIG_BYTES)
            .map_err(|error| format!("Cannot read {}: {error}", path.display()))
    }

    // 配置值约束方法
    #[allow(dead_code)]
    pub fn clamp_font_size(size: f32) -> f32 {
        finite_clamp(size, default_font_size(), 8.0, 72.0)
    }

    #[allow(dead_code)]
    pub fn clamp_line_spacing(spacing: f32) -> f32 {
        finite_clamp(spacing, default_line_spacing(), 0.8, 3.0)
    }

    #[allow(dead_code)]
    pub fn clamp_padding(padding: f32) -> f32 {
        finite_clamp(padding, default_padding(), 0.0, 20.0)
    }

    #[allow(dead_code)]
    pub fn clamp_scrollback_lines(lines: usize) -> usize {
        lines.clamp(100, 100_000)
    }

    #[allow(dead_code)]
    pub fn clamp_opacity(opacity: f32) -> f32 {
        finite_clamp(opacity, default_opacity(), 0.05, 1.0)
    }

    #[allow(dead_code)]
    pub fn clamp_scroll_speed(speed: u32) -> u32 {
        speed.clamp(1, 10)
    }

    pub fn get_monospace_fonts() -> &'static Vec<String> {
        detect_monospace_fonts()
    }

    pub fn cjk_monospace_font_family() -> Option<&'static str> {
        CJK_MONOSPACE_FONT.as_deref()
    }

    pub fn symbol_monospace_font_family() -> Option<&'static str> {
        SYMBOL_MONOSPACE_FONT.as_deref()
    }

    pub fn math_symbol_font_family() -> Option<&'static str> {
        MATH_SYMBOL_FONT.as_deref()
    }

    pub fn nerd_symbol_font_family() -> Option<&'static str> {
        NERD_SYMBOL_FONT.as_deref()
    }
}

fn valid_config_text(value: &str, max_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_bytes
        && !value.chars().any(char::is_control)
        && !jterm_core::review_input::contains_visual_spoofing(value)
}

fn validate_remote_host_text(value: &str, field: &str, max_bytes: usize) -> Result<(), String> {
    if value.len() > max_bytes {
        return Err(format!("{field} exceeds the {max_bytes}-byte limit"));
    }
    if value.chars().any(char::is_control) {
        return Err(format!("{field} must not contain control characters"));
    }
    if jterm_core::review_input::contains_visual_spoofing(value) {
        return Err(format!(
            "{field} must not contain invisible or direction-changing formatting"
        ));
    }
    Ok(())
}

/// The single application-level safety gate for a configured remote host.
/// Invalid drafts are retained for repair, so every process-producing consumer
/// must call this function or [`validate_remote_host_at`] before use.
pub(crate) fn validate_remote_host(
    host: &jterm_core::jsh_remote::RemoteHostConfig,
) -> Result<(), String> {
    validate_remote_host_text(&host.name, "name", MAX_CONFIG_NAME_BYTES)?;
    validate_remote_host_text(&host.host, "host", MAX_CONFIG_VALUE_BYTES)?;
    if let Some(user) = &host.user {
        validate_remote_host_text(user, "user", MAX_CONFIG_VALUE_BYTES)?;
    }
    validate_remote_host_text(&host.remote_shell, "remote_shell", MAX_CONFIG_VALUE_BYTES)?;
    if let Some(session) = &host.session {
        validate_remote_host_text(session, "session", MAX_CONFIG_VALUE_BYTES)?;
    }
    // Reject a hostile collection before walking it. The config file itself
    // is bounded, but this gate also protects per-frame runtime validation.
    if host.ssh_args.len() > MAX_REMOTE_SSH_ARGS {
        return Err(format!(
            "ssh_args must not contain more than {MAX_REMOTE_SSH_ARGS} entries"
        ));
    }
    for argument in &host.ssh_args {
        validate_remote_host_text(argument, "ssh_args entry", MAX_CONFIG_VALUE_BYTES)?;
    }
    if let Some(artifact) = &host.deploy_artifact {
        validate_remote_host_text(artifact, "deploy_artifact", MAX_CONFIG_VALUE_BYTES)?;
    }
    validate_remote_host_text(&host.deploy, "deploy", MAX_CONFIG_NAME_BYTES)?;

    if !matches!(host.deploy.as_str(), "" | "off" | "persist" | "incognito") {
        return Err("deploy must be off, persist, or incognito".to_string());
    }

    let total = host
        .ssh_args
        .iter()
        .try_fold(
            host.name
                .len()
                .saturating_add(host.host.len())
                .saturating_add(host.user.as_deref().map_or(0, str::len))
                .saturating_add(host.remote_shell.len())
                .saturating_add(host.session.as_deref().map_or(0, str::len))
                .saturating_add(host.deploy.len())
                .saturating_add(host.deploy_artifact.as_deref().map_or(0, str::len)),
            |sum, argument| sum.checked_add(argument.len()),
        )
        .unwrap_or(usize::MAX);
    if total > MAX_REMOTE_PROFILE_BYTES {
        return Err(format!(
            "remote profile exceeds the {MAX_REMOTE_PROFILE_BYTES}-byte limit"
        ));
    }

    // The shared semantic validator may quote a rejected value in its error.
    // Run every app-owned size/control/spoof check first so untrusted draft
    // bytes cannot become an unbounded or direction-changing diagnostic.
    host.validate()
}

/// Resolve one active host while enforcing both the per-entry gate and the
/// first-128 resource boundary. Later entries remain saved but cannot execute.
pub(crate) fn validate_remote_host_at(
    hosts: &[jterm_core::jsh_remote::RemoteHostConfig],
    index: usize,
) -> Result<&jterm_core::jsh_remote::RemoteHostConfig, String> {
    if index >= MAX_REMOTE_HOSTS {
        return Err(format!(
            "entry #{} exceeds the {MAX_REMOTE_HOSTS}-host active limit; it is retained but unavailable",
            index.saturating_add(1)
        ));
    }
    let host = hosts
        .get(index)
        .ok_or_else(|| format!("remote host #{} is not configured", index + 1))?;
    validate_remote_host(host)?;
    Ok(host)
}

pub(crate) fn remote_host_problem_counts(
    hosts: &[jterm_core::jsh_remote::RemoteHostConfig],
) -> (usize, usize) {
    let invalid_active = hosts
        .iter()
        .take(MAX_REMOTE_HOSTS)
        .filter(|host| validate_remote_host(host).is_err())
        .count();
    let inactive_retained = hosts.len().saturating_sub(MAX_REMOTE_HOSTS);
    (invalid_active, inactive_retained)
}

/// Bounded, formatting-safe text for runtime labels. Settings inputs retain
/// the original draft bytes so a rejected entry remains editable.
pub(crate) fn remote_host_display_name(
    host: &jterm_core::jsh_remote::RemoteHostConfig,
    index: usize,
) -> String {
    let label =
        jterm_core::review_input::safe_inline_display(host.display_name(), MAX_CONFIG_NAME_BYTES);
    if label.trim().is_empty() {
        format!("remote host #{}", index + 1)
    } else {
        label
    }
}

/// Neutral bounded label for helpers that do not know the host's stable list
/// index; avoids presenting every private validation failure as host "#1".
pub(crate) fn remote_host_runtime_label(host: &jterm_core::jsh_remote::RemoteHostConfig) -> String {
    let label =
        jterm_core::review_input::safe_inline_display(host.display_name(), MAX_CONFIG_NAME_BYTES);
    if label.trim().is_empty() {
        "remote host".to_string()
    } else {
        label
    }
}

fn normalized_text_or(value: String, max_bytes: usize, fallback: fn() -> String) -> String {
    let value = value.trim().to_string();
    if valid_config_text(&value, max_bytes) {
        value
    } else {
        fallback()
    }
}

fn normalized_optional_text(value: Option<String>, max_bytes: usize) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| valid_config_text(value, max_bytes))
}

fn bounded_path(path: Option<PathBuf>) -> Option<PathBuf> {
    path.filter(|path| {
        let value = path.to_string_lossy();
        !value.is_empty()
            && value.len() <= MAX_CONFIG_VALUE_BYTES
            && !value.chars().any(char::is_control)
            && !jterm_core::review_input::contains_visual_spoofing(&value)
    })
}

fn finite_clamp(value: f32, fallback: f32, min: f32, max: f32) -> f32 {
    if value.is_finite() {
        value.clamp(min, max)
    } else {
        fallback
    }
}

#[allow(dead_code)]
pub fn create_default_config() {
    let config = Config::default();
    if let Err(e) = config.save() {
        eprintln!("[Config] Warning: Could not save default config: {}", e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_private(path: &std::path::Path, contents: impl AsRef<[u8]>) {
        std::fs::write(path, contents).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    }

    struct ScratchDir(std::path::PathBuf);

    impl ScratchDir {
        fn new(label: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("frost-config-{label}-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&path).expect("create config scratch directory");
            Self(path)
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn normalization_bounds_untrusted_numeric_values() {
        let config = Config {
            font_size: f32::NAN,
            line_spacing: f32::INFINITY,
            initial_width: -1.0,
            cols: usize::MAX,
            rows: 0,
            ui_scale: Some(f32::NAN),
            ai_max_tokens: u32::MAX,
            ai_temperature: Some(f32::INFINITY),
            agent_max_turns: u32::MAX,
            ..Config::default()
        };

        let normalized = config.normalized();

        assert_eq!(normalized.font_size, default_font_size());
        assert_eq!(normalized.line_spacing, default_line_spacing());
        assert_eq!(normalized.initial_width, 320.0);
        assert_eq!(normalized.cols, crate::terminal::MAX_TERMINAL_COLS);
        assert_eq!(normalized.rows, 1);
        assert_eq!(normalized.ui_scale, None);
        assert_eq!(normalized.ai_max_tokens, 32_768);
        assert_eq!(normalized.ai_temperature, None);
        assert_eq!(normalized.agent_max_turns, 100);
    }

    #[test]
    fn empty_optional_strings_are_normalized() {
        let config = Config {
            theme: "   ".to_string(),
            shell: Some("  ".to_string()),
            ..Config::default()
        };

        let normalized = config.normalized();

        assert_eq!(normalized.theme, default_theme());
        assert_eq!(normalized.shell, None);
    }

    #[test]
    fn oversized_or_control_bearing_config_strings_never_reach_consumers() {
        let config = Config {
            ai_provider: "p".repeat(MAX_CONFIG_NAME_BYTES + 1),
            ai_base_url: format!(
                "https://example.test/{}",
                "x".repeat(MAX_CONFIG_VALUE_BYTES)
            ),
            ai_model: "model\nspoof".to_string(),
            ai_api_key_file: Some("/tmp/key\0suffix".to_string()),
            font_family: "f".repeat(MAX_CONFIG_NAME_BYTES + 1),
            theme: "bad\ntheme".to_string(),
            shell: Some("/bin/sh\0--arg".to_string()),
            session_history_file: Some(PathBuf::from("x".repeat(MAX_CONFIG_VALUE_BYTES + 1))),
            command_history_path: Some(PathBuf::from("history\nspoof")),
            jsh_update_check: "unexpected".to_string(),
            ..Config::default()
        };

        let normalized = config.normalized();

        assert_eq!(normalized.ai_provider, default_ai_provider());
        assert_eq!(normalized.ai_base_url, default_ai_base_url());
        assert_eq!(normalized.ai_model, default_ai_model());
        assert_eq!(normalized.ai_api_key_file, None);
        assert_eq!(normalized.font_family, default_font_family());
        assert_eq!(normalized.theme, default_theme());
        assert_eq!(normalized.shell, None);
        assert_eq!(normalized.session_history_file, None);
        assert_eq!(normalized.command_history_path, None);
        assert_eq!(normalized.jsh_update_check, "daily");
    }

    #[test]
    fn visual_spoofing_config_strings_never_reach_labels_or_paths() {
        let config = Config {
            ai_model: "safe-model\u{202e}gpj".to_string(),
            font_family: "Monospace\u{2066}hidden".to_string(),
            shell: Some("/bin/sh\u{200b}".to_string()),
            session_history_file: Some(PathBuf::from("/tmp/session\u{202d}.json")),
            command_history_path: Some(PathBuf::from("/tmp/history\u{2069}.jsonl")),
            ..Config::default()
        };

        let normalized = config.normalized();

        assert_eq!(normalized.ai_model, default_ai_model());
        assert_eq!(normalized.font_family, default_font_family());
        assert_eq!(normalized.shell, None);
        assert_eq!(normalized.session_history_file, None);
        assert_eq!(normalized.command_history_path, None);
    }

    #[test]
    fn notification_and_repo_strip_defaults_match_family() {
        let config = Config::from_toml("").expect("empty config parses");
        assert!(config.notify_long_blocks);
        assert_eq!(config.notify_long_block_threshold_ms, 10_000);
        assert!(config.show_repo_strip);

        let config = Config::from_toml(
            "notify_long_blocks = false\n\
             notify_long_block_threshold_ms = 250\n\
             show_repo_strip = false\n",
        )
        .expect("overrides parse");
        assert!(!config.notify_long_blocks);
        assert_eq!(config.notify_long_block_threshold_ms, 250);
        assert!(!config.show_repo_strip);
    }

    #[test]
    fn click_moves_cursor_defaults_on_and_can_be_disabled() {
        let config = Config::from_toml("").expect("empty config parses");
        assert!(config.click_moves_cursor);

        let config = Config::from_toml("click_moves_cursor = false\n").expect("override parses");
        assert!(!config.click_moves_cursor);
    }

    #[test]
    fn bottom_bar_defaults_on_and_can_be_disabled() {
        let config = Config::from_toml("").expect("empty config parses");
        assert!(config.bottom_bar);

        let config = Config::from_toml("bottom_bar = false\n").expect("override parses");
        assert!(!config.bottom_bar);
    }

    #[test]
    fn block_mode_defaults_on_and_can_be_disabled() {
        let config = Config::from_toml("").expect("empty config parses");
        assert!(config.block_mode);

        let config = Config::from_toml("block_mode = false\n").expect("override parses");
        assert!(!config.block_mode);
    }

    #[test]
    fn block_compact_defaults_off_and_can_be_enabled() {
        let config = Config::from_toml("").expect("empty config parses");
        assert!(!config.block_compact);

        let config = Config::from_toml("block_compact = true\n").expect("override parses");
        assert!(config.block_compact);
    }

    #[test]
    fn ai_stream_defaults_on_and_can_be_disabled() {
        let config = Config::from_toml("").expect("empty config parses");
        assert!(config.ai_stream);

        let config = Config::from_toml("ai_stream = false\n").expect("override parses");
        assert!(!config.ai_stream);
    }

    fn remote_host(
        name: &str,
        host: &str,
        docker: bool,
    ) -> jterm_core::jsh_remote::RemoteHostConfig {
        jterm_core::jsh_remote::RemoteHostConfig {
            name: name.to_string(),
            host: host.to_string(),
            user: None,
            docker,
            remote_shell: "jsh".to_string(),
            session: None,
            ssh_args: Vec::new(),
            deploy: "persist".to_string(),
            deploy_artifact: None,
        }
    }

    #[test]
    fn remote_hosts_survive_serialization_and_editing() {
        let mut ssh = remote_host("dev", "dev.example.com", false);
        ssh.user = Some("yj".to_string());
        ssh.ssh_args = vec!["-p".to_string(), "22".to_string()];
        let container = remote_host("myubuntu", "myubuntu", true);

        let round_trip = |config: &Config| -> Config {
            let bytes = config.serialized().expect("serialize config");
            Config::from_toml(std::str::from_utf8(&bytes).expect("config is UTF-8"))
                .expect("reparse serialized config")
        };

        let mut config = Config {
            remote_hosts: vec![ssh.clone(), container.clone()],
            ..Config::default()
        };
        assert_eq!(
            round_trip(&config).remote_hosts,
            [ssh, container.clone()],
            "safe hosts must persist verbatim"
        );

        // The settings editor appends a template and deletes by index; the
        // survivors keep their order and content across a save/reload.
        config.remote_hosts.push(remote_host("", "new-box", false));
        config.remote_hosts.remove(0);
        let reloaded = round_trip(&config).remote_hosts;
        assert_eq!(reloaded.len(), 2);
        assert_eq!(reloaded[0], container);
        assert_eq!(reloaded[1].host, "new-box");
    }

    #[test]
    fn normalization_preserves_spoofing_remote_hosts_but_gate_rejects_them() {
        let safe = remote_host("safe", "safe.example.com", false);
        let mut unsafe_hosts = Vec::new();

        let mut host = safe.clone();
        host.name.push('\u{202e}');
        unsafe_hosts.push(host);
        let mut host = safe.clone();
        host.host.push('\u{200b}');
        unsafe_hosts.push(host);
        let mut host = safe.clone();
        host.user = Some("root\u{2066}hidden".to_string());
        unsafe_hosts.push(host);
        let mut host = safe.clone();
        host.remote_shell.push('\u{2069}');
        unsafe_hosts.push(host);
        let mut host = safe.clone();
        host.session = Some("session\u{202d}hidden".to_string());
        unsafe_hosts.push(host);
        let mut host = safe.clone();
        host.ssh_args
            .push("ProxyJump=safe\u{200b}hidden".to_string());
        unsafe_hosts.push(host);
        let mut host = safe.clone();
        host.deploy_artifact = Some("/tmp/jsh\u{202e}hidden".to_string());
        unsafe_hosts.push(host);
        let mut host = safe.clone();
        host.deploy.push('\u{2067}');
        unsafe_hosts.push(host);

        let original: Vec<_> = std::iter::once(safe.clone()).chain(unsafe_hosts).collect();
        let normalized = Config {
            remote_hosts: original.clone(),
            ..Config::default()
        }
        .normalized();

        assert_eq!(normalized.remote_hosts, original);
        assert!(validate_remote_host(&normalized.remote_hosts[0]).is_ok());
        assert!(normalized.remote_hosts[1..]
            .iter()
            .all(|host| validate_remote_host(host).is_err()));
        let displayed = remote_host_display_name(&normalized.remote_hosts[1], 1);
        assert!(!jterm_core::review_input::contains_visual_spoofing(
            &displayed
        ));
        assert!(displayed.len() <= MAX_CONFIG_NAME_BYTES);
    }

    #[test]
    fn normalization_preserves_semantically_invalid_hosts_but_gate_rejects_them() {
        let safe = remote_host("safe", "safe.example.com", false);
        let mut option_like_host = safe.clone();
        option_like_host.host = "-oProxyCommand=bad".to_string();
        let mut invalid_user = safe.clone();
        invalid_user.user = Some("root@other".to_string());
        let mut unknown_deploy = safe.clone();
        unknown_deploy.deploy = "sometimes".to_string();
        let mut relative_artifact = safe.clone();
        relative_artifact.deploy_artifact = Some("relative/jsh".to_string());
        let mut oversized_name = safe.clone();
        oversized_name.name = "n".repeat(MAX_CONFIG_NAME_BYTES + 1);
        let mut oversized_artifact = safe.clone();
        oversized_artifact.deploy_artifact =
            Some(format!("/tmp/{}", "a".repeat(MAX_CONFIG_VALUE_BYTES)));

        let original = vec![
            safe.clone(),
            option_like_host,
            invalid_user,
            unknown_deploy,
            relative_artifact,
            oversized_name,
            oversized_artifact,
        ];
        let normalized = Config {
            remote_hosts: original.clone(),
            ..Config::default()
        }
        .normalized();

        assert_eq!(normalized.remote_hosts, original);
        assert!(validate_remote_host(&normalized.remote_hosts[0]).is_ok());
        assert!(normalized.remote_hosts[1..]
            .iter()
            .all(|host| validate_remote_host(host).is_err()));
        assert!(
            remote_host_display_name(&normalized.remote_hosts[5], 5).len() <= MAX_CONFIG_NAME_BYTES
        );
    }

    #[test]
    fn remote_gate_redacts_unsafe_deploy_values_before_shared_validation() {
        let safe = remote_host("safe", "safe.example.com", false);

        let mut too_many_arguments = safe.clone();
        too_many_arguments.ssh_args = vec!["secret-marker".to_string(); MAX_REMOTE_SSH_ARGS + 1];
        let argument_error = validate_remote_host(&too_many_arguments).unwrap_err();
        assert_eq!(
            argument_error,
            format!("ssh_args must not contain more than {MAX_REMOTE_SSH_ARGS} entries")
        );
        assert!(!argument_error.contains("secret-marker"));

        let mut oversized_profile = safe.clone();
        oversized_profile.ssh_args = vec!["x".repeat(1_024); MAX_REMOTE_SSH_ARGS];
        let profile_error = validate_remote_host(&oversized_profile).unwrap_err();
        assert_eq!(
            profile_error,
            format!("remote profile exceeds the {MAX_REMOTE_PROFILE_BYTES}-byte limit")
        );

        let mut oversized = safe.clone();
        oversized.deploy = format!("secret-marker-{}", "x".repeat(MAX_CONFIG_NAME_BYTES));
        let oversized_error = validate_remote_host(&oversized).unwrap_err();
        assert_eq!(
            oversized_error,
            format!("deploy exceeds the {MAX_CONFIG_NAME_BYTES}-byte limit")
        );
        assert!(!oversized_error.contains("secret-marker"));
        assert!(oversized_error.len() < 128);

        let mut spoofed = safe;
        spoofed.deploy = "unknown\u{202e}secret-marker".to_string();
        let spoofed_error = validate_remote_host(&spoofed).unwrap_err();
        assert_eq!(
            spoofed_error,
            "deploy must not contain invisible or direction-changing formatting"
        );
        assert!(!spoofed_error.contains("unknown"));
        assert!(!spoofed_error.contains("secret-marker"));
        assert!(spoofed_error.len() < 128);
        assert!(!jterm_core::review_input::contains_visual_spoofing(
            &spoofed_error
        ));

        let mut unknown = remote_host("safe", "safe.example.com", false);
        unknown.deploy = "unknown-secret-marker".to_string();
        let unknown_error = validate_remote_host(&unknown).unwrap_err();
        assert_eq!(unknown_error, "deploy must be off, persist, or incognito");
        assert!(!unknown_error.contains("secret-marker"));
    }

    #[test]
    fn normalization_and_serialization_preserve_the_129th_remote_host() {
        let hosts: Vec<_> = (0..MAX_REMOTE_HOSTS + 3)
            .map(|index| remote_host(&format!("host-{index}"), &format!("host-{index}"), false))
            .collect();
        let expected = hosts.clone();

        let normalized = Config {
            remote_hosts: hosts,
            ..Config::default()
        }
        .normalized();

        assert_eq!(normalized.remote_hosts, expected);
        assert!(validate_remote_host_at(&normalized.remote_hosts, MAX_REMOTE_HOSTS - 1).is_ok());
        assert!(validate_remote_host_at(&normalized.remote_hosts, MAX_REMOTE_HOSTS).is_err());
        assert!(validate_remote_host_at(&normalized.remote_hosts, usize::MAX).is_err());

        let bytes = normalized.serialized().expect("serialize every host draft");
        let reparsed = Config::from_toml(std::str::from_utf8(&bytes).expect("UTF-8 config"))
            .expect("reparse every host draft");
        assert_eq!(reparsed.remote_hosts, expected);
    }

    #[test]
    fn disk_save_preserves_invalid_and_129th_remote_drafts() {
        let mut hosts: Vec<_> = (0..MAX_REMOTE_HOSTS + 1)
            .map(|index| remote_host(&format!("host-{index}"), &format!("host-{index}"), false))
            .collect();
        hosts[0].name.clear();
        hosts[0].host.clear();
        let expected = hosts.clone();
        let root =
            std::env::temp_dir().join(format!("frost-remote-roundtrip-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("config.toml");
        let config = Config {
            remote_hosts: hosts,
            ..Config::default()
        };

        config.save_path_force(&path).expect("save retained drafts");
        let saved = std::fs::read_to_string(&path).unwrap();
        let reloaded = Config::from_toml(&saved).expect("reload retained drafts");
        assert_eq!(reloaded.remote_hosts, expected);
        assert_eq!(remote_host_problem_counts(&reloaded.remote_hosts), (1, 1));
        assert_eq!(
            remote_host_runtime_label(&reloaded.remote_hosts[0]),
            "remote host"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn temporary_empty_remote_host_draft_survives_normalization() {
        let mut draft = remote_host("being-edited", "old.example.com", false);
        draft.host.clear();

        let normalized = Config {
            remote_hosts: vec![draft.clone()],
            ..Config::default()
        }
        .normalized();

        assert_eq!(normalized.remote_hosts, [draft]);
        assert!(validate_remote_host(&normalized.remote_hosts[0]).is_err());
    }

    #[test]
    fn task_dashboard_and_context_sharing_default_off() {
        let config = Config::from_toml("").expect("empty config parses");
        assert!(!config.experimental_task_sidebar);
        assert!(!config.ai_share_command_context);
        assert!(config.ai_redact_secrets);
    }

    /// Both directions of the OSC 52 clipboard boundary are refused by
    /// default. frost was the one app of the four that let PTY output — a
    /// program on the far side of an ssh connection included — replace the
    /// host clipboard with no gate at all, so the next paste produced whatever
    /// that program chose.
    #[test]
    fn both_osc52_clipboard_directions_default_closed() {
        let config = Config::from_toml("").expect("empty config parses");
        assert!(!config.allow_clipboard_read);
        assert!(!config.allow_remote_clipboard_write);

        let config = Config::from_toml(
            "allow_clipboard_read = true\n\
             allow_remote_clipboard_write = true\n",
        )
        .expect("overrides parse");
        assert!(config.allow_clipboard_read);
        assert!(config.allow_remote_clipboard_write);

        // The two are independent: opening reads must not open writes.
        let config = Config::from_toml("allow_clipboard_read = true\n").expect("override parses");
        assert!(config.allow_clipboard_read);
        assert!(!config.allow_remote_clipboard_write);
    }

    #[test]
    fn command_history_defaults_match_the_family() {
        let config = Config::from_toml("").expect("empty config parses");
        assert!(config.command_history_enabled);
        assert_eq!(config.command_history_max_entries, 10_000);
        let path = config
            .resolved_command_history_path()
            .expect("enabled history resolves a path");
        assert!(path.ends_with("frost/history.jsonl"), "{}", path.display());
    }

    #[test]
    fn command_history_overrides_are_resolved_and_bounded() {
        let config = Config::from_toml(
            "command_history_enabled = false\n\
             command_history_path = '/tmp/shared-history.jsonl'\n\
             command_history_max_entries = 7\n",
        )
        .expect("overrides parse");
        assert_eq!(config.resolved_command_history_path(), None);
        assert_eq!(config.command_history_max_entries, 100);

        let config = Config {
            command_history_enabled: true,
            command_history_path: Some(PathBuf::from("/tmp/shared-history.jsonl")),
            ..Config::default()
        };
        assert_eq!(
            config.resolved_command_history_path(),
            Some(PathBuf::from("/tmp/shared-history.jsonl"))
        );

        let config = Config {
            command_history_path: Some(PathBuf::from("~/history.jsonl")),
            ..config
        };
        assert_eq!(
            config.resolved_command_history_path(),
            dirs::home_dir().map(|home| home.join("history.jsonl"))
        );
    }

    #[test]
    fn load_path_reports_the_path_and_parse_location() {
        let root = ScratchDir::new("malformed");
        let path = root.0.join("config.toml");
        write_private(&path, "font_size = [not-valid]\n");

        let error = Config::load_path(&path).expect_err("malformed TOML should fail");

        assert!(error.contains(&path.display().to_string()));
        assert!(error.contains("font_size"));
    }

    #[test]
    fn concurrent_stale_revisions_allow_exactly_one_commit() {
        let root = ScratchDir::new("conflict");
        let path = root.0.join("config.toml");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let mut workers = Vec::new();
        for font_size in [17.0, 29.0] {
            let path = path.clone();
            let barrier = barrier.clone();
            workers.push(std::thread::spawn(move || {
                let config = Config {
                    font_size,
                    ..Config::default()
                };
                barrier.wait();
                (
                    font_size,
                    config.save_path_if_unchanged(&path, Some(&FileRevision::Missing)),
                )
            }));
        }
        barrier.wait();
        let outcomes: Vec<_> = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect();
        assert_eq!(
            outcomes.iter().filter(|(_, result)| result.is_ok()).count(),
            1
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|(_, result)| matches!(result, Err(AtomicWriteError::Conflict { .. })))
                .count(),
            1
        );
        let winner = outcomes
            .iter()
            .find_map(|(font_size, result)| result.as_ref().ok().map(|_| *font_size))
            .unwrap();
        assert_eq!(Config::load_path(&path).unwrap().font_size, winner);
    }

    #[cfg(unix)]
    #[test]
    fn save_is_private_and_never_follows_legacy_temp_or_destination_symlinks() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let root = ScratchDir::new("symlink");
        let path = root.0.join("config.toml");
        let victim = root.0.join("victim.txt");
        write_private(&victim, b"do not touch");

        // The old writer opened this predictable path with truncate(true), so
        // a pre-positioned link caused it to overwrite the link target.
        let legacy_temp = path.with_extension(format!("toml.tmp.{}", std::process::id()));
        symlink(&victim, &legacy_temp).unwrap();
        Config::default().save_path_force(&path).unwrap();
        assert_eq!(std::fs::read(&victim).unwrap(), b"do not touch");
        assert!(!std::fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(&root.0).unwrap().permissions().mode() & 0o777,
            0o700
        );

        let lock = root.0.join(".config.toml.lock");
        std::fs::remove_file(&lock).unwrap();
        symlink(&victim, &lock).unwrap();
        Config::default()
            .save_path_force(&path)
            .expect_err("lock symlink must be rejected");
        assert_eq!(std::fs::read(&victim).unwrap(), b"do not touch");
        std::fs::remove_file(&lock).unwrap();

        std::fs::remove_file(&path).unwrap();
        symlink(&victim, &path).unwrap();
        let error = Config::default()
            .save_path_force(&path)
            .expect_err("destination symlink must be rejected");
        assert!(matches!(error, AtomicWriteError::UnsafeSymlink { .. }));
        assert_eq!(std::fs::read(&victim).unwrap(), b"do not touch");
        assert!(std::fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[test]
    fn remote_hosts_deserialize_through_the_shared_family_type() {
        let config = Config::from_toml(
            r#"
[[remote_hosts]]
name = "build"
host = "myubuntu"
docker = true
deploy = "incognito"
"#,
        )
        .expect("parse");
        assert_eq!(config.remote_hosts.len(), 1);
        assert!(config.remote_hosts[0].validate().is_ok());
        assert_eq!(config.remote_hosts[0].display_name(), "build");

        // No key at all means the worked examples; an explicit empty list is
        // taken as written, so a host deleted in the panel stays deleted.
        let config = Config::from_toml("").expect("parse empty");
        assert_eq!(config.remote_hosts, default_remote_hosts());
        let config = Config::from_toml("remote_hosts = []").expect("parse empty list");
        assert!(config.remote_hosts.is_empty());
    }

    /// The defaults are what a user copies, so they have to be spelled the way
    /// the family type accepts: the port as an `ssh_args` flag and the login in
    /// `user`, never folded into `host` as `root@10.68.18.60:22`.
    #[test]
    fn default_remote_hosts_are_valid_and_correctly_shaped() {
        let hosts = default_remote_hosts();
        let names: Vec<&str> = hosts.iter().map(|h| h.display_name()).collect();
        assert_eq!(names, ["dev-60", "myubuntu"]);
        for host in &hosts {
            assert!(host.validate().is_ok(), "{:?}", host.validate());
        }
        assert_eq!(hosts[0].host, "10.68.18.60");
        assert_eq!(hosts[0].user.as_deref(), Some("root"));
        assert_eq!(hosts[0].ssh_args, ["-p", "22"]);
        assert!(!hosts[0].docker);
        assert!(hosts[1].docker);
    }
}
