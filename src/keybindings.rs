/// 快捷键可配置化系统
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// 所有可用的命令
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Command {
    // === 会话管理 ===
    SessionNew,
    SessionClose,
    SessionNext,
    SessionPrev,
    SessionJump(usize), // 跳转到第 N 个会话 (0-8; defaults expose 0-7)
    SessionLast,

    // === 编辑操作 ===
    EditCopy,
    EditPaste,

    // === 搜索操作 ===
    SearchOpen,
    SearchClose,
    SearchNext,
    SearchPrev,
    SearchHistoryPrev,
    SearchHistoryNext,
    SearchReplaceToggle,

    // === 终端操作 ===
    TerminalSendSigint, // Ctrl+C
    TerminalSendEof,    // Ctrl+D
    TerminalClear,      // Ctrl+L
    TerminalScrollUp,
    TerminalScrollDown,
    TerminalPromptPrev,
    TerminalPromptNext,
    TerminalCopyLastOutput,

    // === 分屏操作 ===
    TerminalSplitVertical,   // Ctrl+Shift+E (left/right)
    TerminalSplitHorizontal, // Ctrl+Shift+D (top/bottom)
    TerminalClosePane,       // Ctrl+Shift+W
    PaneFocusNext,
    PaneFocusPrev,
    PaneFocusLeft,
    PaneFocusRight,
    PaneFocusUp,
    PaneFocusDown,
    PaneResizeLeft,
    PaneResizeRight,
    PaneResizeUp,
    PaneResizeDown,
    PaneZoomToggle,
    PaneSwap,

    // === 窗口操作 ===
    WindowClose,

    // === 配置 ===
    ConfigOpen,
    ConfigClose,
    ConfigToggle,
    SidebarToggle,
    AgentToggle,

    // === 字体缩放 ===
    FontZoomIn,
    FontZoomOut,
    FontZoomReset,
}

impl std::fmt::Display for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Command::SessionNew => write!(f, "session:new"),
            Command::SessionClose => write!(f, "session:close"),
            Command::SessionNext => write!(f, "session:next"),
            Command::SessionPrev => write!(f, "session:prev"),
            Command::SessionJump(n) => write!(f, "session:jump:{}", n),
            Command::SessionLast => write!(f, "session:last"),
            Command::EditCopy => write!(f, "edit:copy"),
            Command::EditPaste => write!(f, "edit:paste"),
            Command::SearchOpen => write!(f, "search:open"),
            Command::SearchClose => write!(f, "search:close"),
            Command::SearchNext => write!(f, "search:next"),
            Command::SearchPrev => write!(f, "search:prev"),
            Command::SearchHistoryPrev => write!(f, "search:history:prev"),
            Command::SearchHistoryNext => write!(f, "search:history:next"),
            Command::SearchReplaceToggle => write!(f, "search:replace:toggle"),
            Command::TerminalSendSigint => write!(f, "terminal:send_sigint"),
            Command::TerminalSendEof => write!(f, "terminal:send_eof"),
            Command::TerminalClear => write!(f, "terminal:clear"),
            Command::TerminalScrollUp => write!(f, "terminal:scroll_up"),
            Command::TerminalScrollDown => write!(f, "terminal:scroll_down"),
            Command::TerminalPromptPrev => write!(f, "terminal:prompt_prev"),
            Command::TerminalPromptNext => write!(f, "terminal:prompt_next"),
            Command::TerminalCopyLastOutput => write!(f, "terminal:copy_last_output"),
            Command::TerminalSplitVertical => write!(f, "terminal:split_vertical"),
            Command::TerminalSplitHorizontal => write!(f, "terminal:split_horizontal"),
            Command::TerminalClosePane => write!(f, "terminal:close_pane"),
            Command::PaneFocusNext => write!(f, "pane:focus_next"),
            Command::PaneFocusPrev => write!(f, "pane:focus_prev"),
            Command::PaneFocusLeft => write!(f, "pane:focus_left"),
            Command::PaneFocusRight => write!(f, "pane:focus_right"),
            Command::PaneFocusUp => write!(f, "pane:focus_up"),
            Command::PaneFocusDown => write!(f, "pane:focus_down"),
            Command::PaneResizeLeft => write!(f, "pane:resize_left"),
            Command::PaneResizeRight => write!(f, "pane:resize_right"),
            Command::PaneResizeUp => write!(f, "pane:resize_up"),
            Command::PaneResizeDown => write!(f, "pane:resize_down"),
            Command::PaneZoomToggle => write!(f, "pane:zoom_toggle"),
            Command::PaneSwap => write!(f, "pane:swap"),
            Command::WindowClose => write!(f, "window:close"),
            Command::ConfigOpen => write!(f, "config:open"),
            Command::ConfigClose => write!(f, "config:close"),
            Command::ConfigToggle => write!(f, "config:toggle"),
            Command::SidebarToggle => write!(f, "sidebar:toggle"),
            Command::AgentToggle => write!(f, "agent:toggle"),
            Command::FontZoomIn => write!(f, "font:zoom_in"),
            Command::FontZoomOut => write!(f, "font:zoom_out"),
            Command::FontZoomReset => write!(f, "font:zoom_reset"),
        }
    }
}

impl std::str::FromStr for Command {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "session:new" => Ok(Command::SessionNew),
            "session:close" => Ok(Command::SessionClose),
            "session:next" => Ok(Command::SessionNext),
            "session:prev" => Ok(Command::SessionPrev),
            "session:last" => Ok(Command::SessionLast),
            "edit:copy" => Ok(Command::EditCopy),
            "edit:paste" => Ok(Command::EditPaste),
            "search:open" => Ok(Command::SearchOpen),
            "search:close" => Ok(Command::SearchClose),
            "search:next" => Ok(Command::SearchNext),
            "search:prev" => Ok(Command::SearchPrev),
            "search:history:prev" => Ok(Command::SearchHistoryPrev),
            "search:history:next" => Ok(Command::SearchHistoryNext),
            "search:replace:toggle" => Ok(Command::SearchReplaceToggle),
            "terminal:send_sigint" => Ok(Command::TerminalSendSigint),
            "terminal:send_eof" => Ok(Command::TerminalSendEof),
            "terminal:clear" => Ok(Command::TerminalClear),
            "terminal:scroll_up" => Ok(Command::TerminalScrollUp),
            "terminal:scroll_down" => Ok(Command::TerminalScrollDown),
            "terminal:prompt_prev" => Ok(Command::TerminalPromptPrev),
            "terminal:prompt_next" => Ok(Command::TerminalPromptNext),
            "terminal:copy_last_output" => Ok(Command::TerminalCopyLastOutput),
            "terminal:split_vertical" => Ok(Command::TerminalSplitVertical),
            "terminal:split_horizontal" => Ok(Command::TerminalSplitHorizontal),
            "terminal:close_pane" => Ok(Command::TerminalClosePane),
            "pane:focus_next" => Ok(Command::PaneFocusNext),
            "pane:focus_prev" => Ok(Command::PaneFocusPrev),
            "pane:focus_left" => Ok(Command::PaneFocusLeft),
            "pane:focus_right" => Ok(Command::PaneFocusRight),
            "pane:focus_up" => Ok(Command::PaneFocusUp),
            "pane:focus_down" => Ok(Command::PaneFocusDown),
            "pane:resize_left" => Ok(Command::PaneResizeLeft),
            "pane:resize_right" => Ok(Command::PaneResizeRight),
            "pane:resize_up" => Ok(Command::PaneResizeUp),
            "pane:resize_down" => Ok(Command::PaneResizeDown),
            "pane:zoom_toggle" => Ok(Command::PaneZoomToggle),
            "pane:swap" => Ok(Command::PaneSwap),
            "window:close" => Ok(Command::WindowClose),
            "config:open" => Ok(Command::ConfigOpen),
            "config:close" => Ok(Command::ConfigClose),
            "config:toggle" => Ok(Command::ConfigToggle),
            "sidebar:toggle" => Ok(Command::SidebarToggle),
            "agent:toggle" => Ok(Command::AgentToggle),
            "font:zoom_in" => Ok(Command::FontZoomIn),
            "font:zoom_out" => Ok(Command::FontZoomOut),
            "font:zoom_reset" => Ok(Command::FontZoomReset),
            s if s.starts_with("session:jump:") => {
                let num_str = &s[13..];
                let num = num_str
                    .parse::<usize>()
                    .map_err(|_| format!("Invalid session number: {}", num_str))?;
                if num < 9 {
                    Ok(Command::SessionJump(num))
                } else {
                    Err(format!("Session number out of range: {}", num))
                }
            }
            _ => Err(format!("Unknown command: {}", s)),
        }
    }
}

/// 快捷键字符串归一化。chord 的解析/序列化本体在家族共享的
/// `jterm_core::keybindings`（jterm1..4 共用一套语法）；这里只保留
/// jterm3 的入口名。
pub struct KeyBinding;

impl KeyBinding {
    /// Canonicalize a binding string to the `ctrl+shift+alt+super+key` order
    /// (all lowercase) used internally. Parsing is the family union grammar
    /// from `jterm_core::keybindings::parse`: any modifier order, modifier
    /// aliases (`control` -> ctrl, `option` -> alt, `cmd`/`command`/`win`/
    /// `meta` -> super), named-key aliases (`enter`/`return`, `esc`/`escape`,
    /// `arrowleft`/`left`, `page_up`/`prior`/`pageup`, ...), the plus-as-key
    /// forms (`ctrl++`, `ctrl+shift++`), `f1`..`f24`, and Unicode (not ASCII)
    /// lowercasing. `\` folds to the word `backslash` so stored strings never
    /// need TOML escaping. Returns `None` if the string isn't a valid binding
    /// — the per-entry diagnostics in
    /// [`KeyBindings::from_toml_with_diagnostics`] rely on that. This is what
    /// lets a user-written `shift+ctrl+f` or `cmd+c` actually match.
    pub fn canonical(binding_str: &str) -> Option<String> {
        jterm_core::keybindings::parse(binding_str)
            .ok()
            .map(|chord| chord.canonical())
    }
}

/// 快捷键绑定集合
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KeyBindings {
    #[serde(flatten)]
    pub bindings: HashMap<String, String>, // "ctrl+shift+a" => "command:name"
}

#[derive(Clone, Debug)]
pub struct KeyBindingsLoad {
    pub bindings: KeyBindings,
    pub diagnostics: Vec<String>,
    /// False when the file itself could not be read or parsed. Callers doing a
    /// live reload should retain their last-known-good table in that case.
    pub usable: bool,
}

impl KeyBindings {
    pub fn new() -> Self {
        Self {
            bindings: HashMap::new(),
        }
    }

    /// 加载默认快捷键
    pub fn default_bindings() -> Self {
        let mut bindings = Self::new();

        // 会话管理
        bindings
            .bindings
            .insert("ctrl+shift+t".to_string(), "session:new".to_string());
        bindings
            .bindings
            .insert("ctrl+d".to_string(), "terminal:send_eof".to_string());
        bindings
            .bindings
            .insert("ctrl+tab".to_string(), "session:next".to_string());
        bindings
            .bindings
            .insert("ctrl+shift+tab".to_string(), "session:prev".to_string());
        bindings
            .bindings
            .insert("ctrl+pagedown".to_string(), "session:next".to_string());
        bindings
            .bindings
            .insert("ctrl+pageup".to_string(), "session:prev".to_string());

        // Browser-style tab switching: Ctrl+1..8 address the first eight tabs,
        // Ctrl+9 always selects the last tab, and Ctrl+0 resets font zoom.
        for i in 0..8 {
            bindings
                .bindings
                .insert(format!("ctrl+{}", i + 1), format!("session:jump:{}", i));
        }
        bindings
            .bindings
            .insert("ctrl+9".to_string(), "session:last".to_string());

        // 编辑操作
        bindings
            .bindings
            .insert("ctrl+shift+c".to_string(), "edit:copy".to_string());
        bindings
            .bindings
            .insert("ctrl+shift+v".to_string(), "edit:paste".to_string());

        // 分屏操作
        bindings.bindings.insert(
            "ctrl+shift+e".to_string(),
            "terminal:split_vertical".to_string(),
        );
        bindings.bindings.insert(
            "ctrl+shift+d".to_string(),
            "terminal:split_horizontal".to_string(),
        );
        bindings.bindings.insert(
            "ctrl+shift+w".to_string(),
            "terminal:close_pane".to_string(),
        );
        for (key, command) in [
            ("left", "pane:focus_left"),
            ("right", "pane:focus_right"),
            ("up", "pane:focus_up"),
            ("down", "pane:focus_down"),
        ] {
            bindings
                .bindings
                .insert(format!("ctrl+alt+{key}"), command.to_string());
        }
        for (key, command) in [
            ("left", "pane:resize_left"),
            ("right", "pane:resize_right"),
            ("up", "pane:resize_up"),
            ("down", "pane:resize_down"),
        ] {
            bindings
                .bindings
                .insert(format!("ctrl+shift+alt+{key}"), command.to_string());
        }
        bindings
            .bindings
            .insert("ctrl+shift+z".to_string(), "pane:zoom_toggle".to_string());
        bindings
            .bindings
            .insert("ctrl+shift+x".to_string(), "pane:swap".to_string());

        // 搜索操作
        bindings
            .bindings
            .insert("ctrl+shift+f".to_string(), "search:open".to_string());
        // Same chord as jterm2's Find & Replace panel; Ctrl+Alt is otherwise
        // only used for pane focus (arrow keys), so the letter chord is free.
        bindings.bindings.insert(
            "ctrl+alt+r".to_string(),
            "search:replace:toggle".to_string(),
        );

        // 配置操作
        bindings
            .bindings
            .insert("ctrl+shift+o".to_string(), "config:toggle".to_string());
        bindings
            .bindings
            .insert("ctrl+backslash".to_string(), "sidebar:toggle".to_string());
        bindings
            .bindings
            .insert("ctrl+shift+a".to_string(), "agent:toggle".to_string());

        // 终端操作
        bindings
            .bindings
            .insert("ctrl+up".to_string(), "terminal:scroll_up".to_string());
        bindings
            .bindings
            .insert("ctrl+down".to_string(), "terminal:scroll_down".to_string());
        bindings.bindings.insert(
            "ctrl+shift+up".to_string(),
            "terminal:prompt_prev".to_string(),
        );
        bindings.bindings.insert(
            "ctrl+shift+down".to_string(),
            "terminal:prompt_next".to_string(),
        );
        bindings.bindings.insert(
            "ctrl+shift+g".to_string(),
            "terminal:copy_last_output".to_string(),
        );

        // 字体缩放
        bindings
            .bindings
            .insert("ctrl+=".to_string(), "font:zoom_in".to_string());
        bindings
            .bindings
            .insert("ctrl+-".to_string(), "font:zoom_out".to_string());
        bindings
            .bindings
            .insert("ctrl+0".to_string(), "font:zoom_reset".to_string());

        bindings
    }

    /// 获取快捷键对应的命令
    pub fn get_command(&self, key_str: &str) -> Option<Command> {
        let normalized = KeyBinding::canonical(key_str)?;
        self.bindings
            .get(&normalized)
            .and_then(|cmd_str| cmd_str.parse::<Command>().ok())
    }

    /// 检测快捷键冲突
    #[cfg(test)]
    pub fn check_conflicts(&self) -> Vec<String> {
        let mut conflicts = Vec::new();

        // 如果两个不同的快捷键映射到同一个命令，不算冲突
        // 如果一个快捷键映射到多个命令，这在 HashMap 中不会发生

        for (binding, command_str) in &self.bindings {
            if let Err(e) = command_str.parse::<Command>() {
                conflicts.push(format!("Invalid command in binding '{}': {}", binding, e));
            }
        }

        conflicts
    }

    /// Merge user TOML over defaults while retaining every valid entry. Invalid
    /// bindings are diagnosed individually instead of discarding the entire
    /// custom table.
    pub fn from_toml_with_diagnostics(content: &str) -> Result<KeyBindingsLoad, toml::de::Error> {
        let mut bindings = Self::default_bindings();
        let mut diagnostics = Vec::new();
        let user_bindings: KeyBindings = toml::from_str(content)?;
        for (key, value) in user_bindings.bindings {
            let Some(canonical) = KeyBinding::canonical(&key) else {
                diagnostics.push(format!("Invalid shortcut \"{key}\""));
                continue;
            };
            if let Err(error) = value.parse::<Command>() {
                diagnostics.push(format!("Invalid command for \"{key}\": {error}"));
                continue;
            }
            bindings.bindings.insert(canonical, value);
        }
        Ok(KeyBindingsLoad {
            bindings,
            diagnostics,
            usable: true,
        })
    }

    /// Load the user file with path-rich, UI-ready diagnostics.
    pub fn load_with_diagnostics() -> KeyBindingsLoad {
        let mut fallback = KeyBindingsLoad {
            bindings: Self::default_bindings(),
            diagnostics: Vec::new(),
            usable: true,
        };
        let path = match Self::config_path() {
            Ok(path) => path,
            Err(error) => {
                fallback
                    .diagnostics
                    .push(format!("Cannot locate keybindings config: {error}"));
                fallback.usable = false;
                return fallback;
            }
        };
        if !path.exists() {
            return fallback;
        }
        let content = match std::fs::read_to_string(&path) {
            Ok(content) => content,
            Err(error) => {
                fallback
                    .diagnostics
                    .push(format!("Cannot read {}: {error}", path.display()));
                fallback.usable = false;
                return fallback;
            }
        };
        match Self::from_toml_with_diagnostics(&content) {
            Ok(mut loaded) => {
                for diagnostic in &mut loaded.diagnostics {
                    *diagnostic = format!("{}: {diagnostic}", path.display());
                }
                loaded
            }
            Err(error) => {
                fallback
                    .diagnostics
                    .push(format!("Cannot parse {}: {error}", path.display()));
                fallback.usable = false;
                fallback
            }
        }
    }

    /// 获取配置文件路径
    pub fn config_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
        let config_dir = dirs::config_dir().ok_or("Could not determine config directory")?;
        Ok(config_dir.join("jterm3/keybindings.toml"))
    }

    pub fn config_mtime() -> Option<std::time::SystemTime> {
        Self::config_path()
            .ok()
            .and_then(|path| std::fs::metadata(path).ok())
            .and_then(|metadata| metadata.modified().ok())
    }
}

impl Default for KeyBindings {
    fn default() -> Self {
        Self::default_bindings()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jterm_core::keybindings::{CommonAction, DEFAULT_CHORDS};

    #[test]
    fn test_command_parse() {
        let cmd: Command = "session:new".parse().unwrap();
        assert_eq!(cmd, Command::SessionNew);

        let cmd: Command = "session:jump:5".parse().unwrap();
        assert_eq!(cmd, Command::SessionJump(5));

        let cmd: Command = "sidebar:toggle".parse().unwrap();
        assert_eq!(cmd, Command::SidebarToggle);
    }

    #[test]
    fn canonical_accepts_the_family_union_grammar() {
        // Reordering and aliases collapse to the one stored spelling.
        for (input, want) in [
            ("shift+ctrl+f", "ctrl+shift+f"),
            ("Control+Shift+T", "ctrl+shift+t"),
            ("cmd+c", "super+c"),
            ("option+left", "alt+left"),
            ("ctrl+enter", "ctrl+return"),
            ("ctrl+esc", "ctrl+escape"),
            ("ctrl+alt+arrowleft", "ctrl+alt+left"),
            ("ctrl+page_up", "ctrl+pageup"),
            ("ctrl+\\", "ctrl+backslash"),
            // Plus-as-key: jterm3 alone used to reject these; the family
            // grammar accepts them (canonical form keeps the literal '+').
            ("ctrl++", "ctrl++"),
            ("ctrl+shift++", "ctrl+shift++"),
            ("ctrl+plus", "ctrl++"),
            // Unicode (not ASCII) lowercasing — same fold as the runtime
            // key mapper in main.rs.
            ("ctrl+Ю", "ctrl+ю"),
        ] {
            assert_eq!(
                KeyBinding::canonical(input).as_deref(),
                Some(want),
                "{input}"
            );
        }
        // Still invalid: empties, duplicate modifiers, and a doubled
        // separator mid-chord ("ctrl++x" is an empty modifier, not a key).
        for input in ["", "  ", "ctrl", "ctrl+shift", "ctrl+ctrl+t", "ctrl++x"] {
            assert_eq!(KeyBinding::canonical(input), None, "{input:?}");
        }
    }

    /// jterm3's command for each family-contract action. `None` rows are
    /// handled by the fixed app-chrome layer (`chrome_shortcut` in main.rs)
    /// rather than the configurable map.
    fn common_action_command(action: CommonAction) -> Option<Command> {
        use CommonAction as A;
        Some(match action {
            A::NewTab => Command::SessionNew,
            A::ClosePaneOrTab => Command::TerminalClosePane,
            A::Copy => Command::EditCopy,
            A::Paste => Command::EditPaste,
            A::NextTab | A::NextTabPage => Command::SessionNext,
            A::PrevTab | A::PrevTabPage => Command::SessionPrev,
            A::QuickSwitch(n) => Command::SessionJump(usize::from(n) - 1),
            A::LastTab => Command::SessionLast,
            A::FontIncrease => Command::FontZoomIn,
            A::FontDecrease => Command::FontZoomOut,
            A::FontReset => Command::FontZoomReset,
            A::Search => Command::SearchOpen,
            A::Settings => Command::ConfigToggle,
            A::Sidebar => Command::SidebarToggle,
            A::ScrollUp => Command::TerminalScrollUp,
            A::ScrollDown => Command::TerminalScrollDown,
            A::PaneFocusLeft => Command::PaneFocusLeft,
            A::PaneFocusRight => Command::PaneFocusRight,
            A::PaneFocusUp => Command::PaneFocusUp,
            A::PaneFocusDown => Command::PaneFocusDown,
            A::PaneResizeLeft => Command::PaneResizeLeft,
            A::PaneResizeRight => Command::PaneResizeRight,
            A::PaneResizeUp => Command::PaneResizeUp,
            A::PaneResizeDown => Command::PaneResizeDown,
            A::SplitSideBySide => Command::TerminalSplitVertical,
            A::SplitStacked => Command::TerminalSplitHorizontal,
            A::PaneZoom => Command::PaneZoomToggle,
            A::CommandPalette | A::DebugPanel => return None,
        })
    }

    /// The family ergonomic contract, driven by core's table so jterm3 can't
    /// silently drift from jterm1/2/4: every `DEFAULT_CHORDS` row is either
    /// bound to the mapped command or deliberately owned by the chrome layer
    /// (in which case the configurable map must leave the chord free).
    #[test]
    fn common_default_chord_matrix() {
        let bindings = KeyBindings::default_bindings();
        for (action, chord) in DEFAULT_CHORDS {
            match common_action_command(*action) {
                Some(expected) => assert_eq!(
                    bindings.get_command(chord),
                    Some(expected),
                    "{action:?} ({chord})"
                ),
                None => assert_eq!(
                    bindings.get_command(chord),
                    None,
                    "{action:?} ({chord}) belongs to the chrome layer; a map \
                     binding would shadow it"
                ),
            }
        }
    }

    #[test]
    fn jterm3_chords_beyond_the_family_contract() {
        let bindings = KeyBindings::default_bindings();
        let cases = [
            // Excluded from DEFAULT_CHORDS by design (jterm1/4 bind it to
            // SelectAllBlocks); jterm3 keeps its lineage meaning.
            ("ctrl+shift+a", Command::AgentToggle),
            ("ctrl+alt+r", Command::SearchReplaceToggle),
            ("ctrl+shift+x", Command::PaneSwap),
            ("ctrl+d", Command::TerminalSendEof),
            ("ctrl+shift+up", Command::TerminalPromptPrev),
            ("ctrl+shift+down", Command::TerminalPromptNext),
            ("ctrl+shift+g", Command::TerminalCopyLastOutput),
        ];
        for (chord, expected) in cases {
            assert_eq!(bindings.get_command(chord), Some(expected), "{chord}");
        }

        assert_eq!(
            bindings.get_command("ctrl+\\"),
            Some(Command::SidebarToggle),
            "literal and named backslash forms must canonicalize identically"
        );
        assert_eq!(bindings.get_command("ctrl+shift+j"), None);
    }

    #[test]
    fn test_conflict_detection() {
        let bindings = KeyBindings::default_bindings();
        let conflicts = bindings.check_conflicts();
        assert!(
            conflicts.is_empty(),
            "Default bindings should have no conflicts"
        );
    }

    #[test]
    fn invalid_user_entries_do_not_discard_valid_overrides() {
        // "ctrl+shift+b" is a valid chord bound to a nonexistent command;
        // "ctrl++x" is genuinely malformed under the family grammar too (a
        // doubled separator mid-chord — not the valid plus-as-key "ctrl++").
        let loaded = KeyBindings::from_toml_with_diagnostics(
            r#"
"shift+ctrl+k" = "session:new"
"ctrl+shift+b" = "command:does-not-exist"
"ctrl++x" = "edit:copy"
"#,
        )
        .expect("valid TOML");

        assert_eq!(
            loaded.bindings.get_command("ctrl+shift+k"),
            Some(Command::SessionNew)
        );
        assert_eq!(loaded.diagnostics.len(), 2);
        assert!(loaded
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("Invalid command")));
        assert!(loaded
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("Invalid shortcut")));
    }

    #[test]
    fn test_command_display() {
        assert_eq!(Command::SessionNew.to_string(), "session:new");
        assert_eq!(Command::SessionJump(3).to_string(), "session:jump:3");
        assert_eq!(Command::SessionLast.to_string(), "session:last");
        assert_eq!(Command::PaneFocusLeft.to_string(), "pane:focus_left");
        assert_eq!(Command::PaneResizeDown.to_string(), "pane:resize_down");
    }
}
