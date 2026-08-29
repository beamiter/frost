/// 快捷键可配置化系统
use crate::persistence::{self, FileRevision};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

const MAX_KEYBINDINGS_BYTES: u64 = 256 * 1024;

/// 所有可用的命令
// Variant names mirror their `Display` command ids verbatim, even where that
// ends in "...Command" (block:copy_command / block:recall_command).
#[allow(clippy::enum_variant_names)]
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
    EditCopyBlockOutput,
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

    // === 命令块（OSC 133 block mode）===
    // Mostly unbound by default: ctrl+shift+x (the anvil/4 chord for jump-
    // first-failed) already means pane:swap in this repo's defaults,
    // ctrl+alt+up/down (the natural select-prev/next chords) are
    // pane:focus_up/down here, and ctrl+alt+left/right (the natural
    // jump-prev/next-failed chords) are pane:focus_left/right. The P0 batch
    // actions keep the family chords (Ctrl+Shift+A/I/K), and block:search uses
    // Ctrl+Alt+F because Ctrl+Shift+G is terminal:copy_last_output here;
    // Bookmark toggling/navigation use the family Ctrl+Shift+B and Ctrl+,/.
    // chords. The failed-block Agent actions (fix/explain/retry) ride the same
    // free Ctrl+Alt letter family as block:search: Ctrl+Alt+X/E/T. The
    // remaining block commands stay palette-only until bound.
    BlockJumpFirstFailed,
    BlockJumpPrevFailed,
    BlockJumpNextFailed,
    BlockCopyCommand,
    BlockCopyOutput,
    BlockRecallCommand,
    BlockSelectAll,
    BlockClear,
    /// Restore the blocks removed by the most recent Clear Blocks. Like
    /// anvil/forge it ships palette-only (no default chord) but stays bindable.
    BlockUndoClear,
    BlockSelectPrev,
    BlockSelectNext,
    BlockReinputSelectedCommands,
    BlockCopyBlock,
    BlockCopyMarkdown,
    BlockExportSessionMarkdown,
    BlockExportSessionJson,
    BlockSearch,
    BlockToggleBookmark,
    BlockJumpPrevBookmark,
    BlockJumpNextBookmark,
    // Failed-block Agent/retry actions (f740a6f) ride the free Ctrl+Alt
    // letter family like block:search and search:replace:toggle.
    BlockFixWithAgent,
    BlockExplainWithAgent,
    BlockRetryFailed,
    /// Fold/unfold the selected block's output. Ctrl+Alt+Z keeps it in the
    /// same free Ctrl+Alt letter family as the other block chords.
    BlockToggleCollapse,

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
    /// Reset every split divider in the focused tab to an even share. ember
    /// ships this palette-only too, so no default chord here.
    PaneEqualize,

    // === 窗口操作 ===
    WindowClose,

    // === 配置 ===
    ConfigOpen,
    ConfigClose,
    ConfigToggle,
    SidebarToggle,
    AgentToggle,
    /// Show or hide the persistent AI chats library panel (anvil's
    /// `toggle_ai_panel` action and its Ctrl+Shift+Alt+A chord).
    ///
    /// The id is the family's `ai_chat:toggle` — singular, like the
    /// `agent:toggle` / `sidebar:toggle` / `debug:toggle` it sits beside. One
    /// id and one chord for one panel across anvil, forge, ember and frost:
    /// a per-app spelling makes a shared keybindings file mean different
    /// things in different windows.
    AiChatToggle,

    // === 字体缩放 ===
    FontZoomIn,
    FontZoomOut,
    FontZoomReset,

    // === 窗口透明度 ===
    OpacityIncrease,
    OpacityDecrease,
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
            Command::EditCopyBlockOutput => write!(f, "edit:copy_block_output"),
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
            Command::BlockJumpFirstFailed => write!(f, "block:jump_first_failed"),
            Command::BlockJumpPrevFailed => write!(f, "block:jump_prev_failed"),
            Command::BlockJumpNextFailed => write!(f, "block:jump_next_failed"),
            Command::BlockCopyCommand => write!(f, "block:copy_command"),
            Command::BlockCopyOutput => write!(f, "block:copy_output"),
            Command::BlockRecallCommand => write!(f, "block:recall_command"),
            Command::BlockSelectAll => write!(f, "block:select_all"),
            Command::BlockClear => write!(f, "block:clear"),
            Command::BlockUndoClear => write!(f, "block:undo_clear"),
            Command::BlockSelectPrev => write!(f, "block:select_prev"),
            Command::BlockSelectNext => write!(f, "block:select_next"),
            Command::BlockReinputSelectedCommands => {
                write!(f, "block:reinput_selected_commands")
            }
            Command::BlockCopyBlock => write!(f, "block:copy_block"),
            Command::BlockCopyMarkdown => write!(f, "block:copy_markdown"),
            Command::BlockExportSessionMarkdown => {
                write!(f, "block:export_session_markdown")
            }
            Command::BlockExportSessionJson => write!(f, "block:export_session_json"),
            Command::BlockSearch => write!(f, "block:search"),
            Command::BlockToggleBookmark => write!(f, "block:toggle_bookmark"),
            Command::BlockJumpPrevBookmark => write!(f, "block:jump_prev_bookmark"),
            Command::BlockJumpNextBookmark => write!(f, "block:jump_next_bookmark"),
            Command::BlockFixWithAgent => write!(f, "block:fix_with_agent"),
            Command::BlockExplainWithAgent => write!(f, "block:explain_with_agent"),
            Command::BlockRetryFailed => write!(f, "block:retry_failed"),
            Command::BlockToggleCollapse => write!(f, "block:toggle_collapse"),
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
            Command::PaneEqualize => write!(f, "pane:equalize"),
            Command::WindowClose => write!(f, "window:close"),
            Command::ConfigOpen => write!(f, "config:open"),
            Command::ConfigClose => write!(f, "config:close"),
            Command::ConfigToggle => write!(f, "config:toggle"),
            Command::SidebarToggle => write!(f, "sidebar:toggle"),
            Command::AgentToggle => write!(f, "agent:toggle"),
            Command::AiChatToggle => write!(f, "ai_chat:toggle"),
            Command::FontZoomIn => write!(f, "font:zoom_in"),
            Command::FontZoomOut => write!(f, "font:zoom_out"),
            Command::FontZoomReset => write!(f, "font:zoom_reset"),
            Command::OpacityIncrease => write!(f, "opacity:increase"),
            Command::OpacityDecrease => write!(f, "opacity:decrease"),
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
            "edit:copy_block_output" => Ok(Command::EditCopyBlockOutput),
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
            "block:jump_first_failed" => Ok(Command::BlockJumpFirstFailed),
            "block:jump_prev_failed" => Ok(Command::BlockJumpPrevFailed),
            "block:jump_next_failed" => Ok(Command::BlockJumpNextFailed),
            "block:copy_command" => Ok(Command::BlockCopyCommand),
            "block:copy_output" => Ok(Command::BlockCopyOutput),
            "block:recall_command" => Ok(Command::BlockRecallCommand),
            "block:select_all" => Ok(Command::BlockSelectAll),
            "block:clear" => Ok(Command::BlockClear),
            "block:undo_clear" => Ok(Command::BlockUndoClear),
            "block:select_prev" => Ok(Command::BlockSelectPrev),
            "block:select_next" => Ok(Command::BlockSelectNext),
            "block:reinput_selected_commands" => Ok(Command::BlockReinputSelectedCommands),
            "block:copy_block" => Ok(Command::BlockCopyBlock),
            "block:copy_markdown" => Ok(Command::BlockCopyMarkdown),
            "block:export_session_markdown" => Ok(Command::BlockExportSessionMarkdown),
            "block:export_session_json" => Ok(Command::BlockExportSessionJson),
            "block:search" => Ok(Command::BlockSearch),
            "block:toggle_bookmark" => Ok(Command::BlockToggleBookmark),
            "block:jump_prev_bookmark" => Ok(Command::BlockJumpPrevBookmark),
            "block:jump_next_bookmark" => Ok(Command::BlockJumpNextBookmark),
            "block:fix_with_agent" => Ok(Command::BlockFixWithAgent),
            "block:explain_with_agent" => Ok(Command::BlockExplainWithAgent),
            "block:retry_failed" => Ok(Command::BlockRetryFailed),
            "block:toggle_collapse" => Ok(Command::BlockToggleCollapse),
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
            "pane:equalize" => Ok(Command::PaneEqualize),
            "window:close" => Ok(Command::WindowClose),
            "config:open" => Ok(Command::ConfigOpen),
            "config:close" => Ok(Command::ConfigClose),
            "config:toggle" => Ok(Command::ConfigToggle),
            "sidebar:toggle" => Ok(Command::SidebarToggle),
            "agent:toggle" => Ok(Command::AgentToggle),
            "ai_chat:toggle" => Ok(Command::AiChatToggle),
            "font:zoom_in" => Ok(Command::FontZoomIn),
            "font:zoom_out" => Ok(Command::FontZoomOut),
            "font:zoom_reset" => Ok(Command::FontZoomReset),
            "opacity:increase" => Ok(Command::OpacityIncrease),
            "opacity:decrease" => Ok(Command::OpacityDecrease),
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
/// `jterm_core::keybindings`（anvil..4 共用一套语法）；这里只保留
/// frost 的入口名。
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
    /// Exact observed contents. Parse failures carry a revision so the same
    /// bad bytes are diagnosed once; transient read failures leave this None
    /// and are retried on the next tick.
    pub revision: Option<FileRevision>,
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
        // Ctrl+9 always selects the last tab, and Ctrl+0 resets the font size.
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
        bindings.bindings.insert(
            "ctrl+shift+alt+c".to_string(),
            "edit:copy_block_output".to_string(),
        );
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
        // Same chord as ember's Find & Replace panel; Ctrl+Alt is otherwise
        // only used for pane focus (arrow keys), so the letter chord is free.
        bindings.bindings.insert(
            "ctrl+alt+r".to_string(),
            "search:replace:toggle".to_string(),
        );
        // Cross-block search picker. ctrl+shift+g (ember's chord) is
        // terminal:copy_last_output in this repo, so the free ctrl+alt letter
        // family carries it instead (like ctrl+alt+r above).
        bindings
            .bindings
            .insert("ctrl+alt+f".to_string(), "block:search".to_string());
        bindings
            .bindings
            .insert("ctrl+shift+a".to_string(), "block:select_all".to_string());
        bindings
            .bindings
            .insert("ctrl+shift+k".to_string(), "block:clear".to_string());
        bindings.bindings.insert(
            "ctrl+shift+i".to_string(),
            "block:reinput_selected_commands".to_string(),
        );
        bindings.bindings.insert(
            "ctrl+shift+b".to_string(),
            "block:toggle_bookmark".to_string(),
        );
        bindings
            .bindings
            .insert("ctrl+,".to_string(), "block:jump_prev_bookmark".to_string());
        bindings
            .bindings
            .insert("ctrl+.".to_string(), "block:jump_next_bookmark".to_string());
        // Failed-block Agent actions and retry: same free ctrl+alt letter
        // family as block:search (F) and search:replace:toggle (R). X/E/T do
        // not collide with any default (ctrl+alt letters in use: F/G/R) and
        // their alt-less bases (ctrl+x/e/t) are unbound, so the Alt-copy
        // fallback never shadows them.
        bindings
            .bindings
            .insert("ctrl+alt+x".to_string(), "block:fix_with_agent".to_string());
        bindings.bindings.insert(
            "ctrl+alt+e".to_string(),
            "block:explain_with_agent".to_string(),
        );
        bindings
            .bindings
            .insert("ctrl+alt+t".to_string(), "block:retry_failed".to_string());
        // Collapse/expand was right-click-only; Z is the remaining free letter
        // in this family (ctrl+shift+z is pane zoom, ctrl+z is unbound).
        bindings.bindings.insert(
            "ctrl+alt+z".to_string(),
            "block:toggle_collapse".to_string(),
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
            .insert("ctrl+alt+g".to_string(), "agent:toggle".to_string());
        // The family's AI-panel chord (Ctrl+Shift+Alt+A in display order),
        // stored in the shared canonical spelling. The ctrl+shift+alt trio is
        // otherwise used only by pane-resize arrows and copy-block-output, so
        // the letter is free.
        bindings
            .bindings
            .insert("ctrl+shift+alt+a".to_string(), "ai_chat:toggle".to_string());

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

        // 窗口透明度实时调节，与 anvil/forge 相同的键位。
        bindings
            .bindings
            .insert("ctrl+alt+=".to_string(), "opacity:increase".to_string());
        bindings
            .bindings
            .insert("ctrl+alt+-".to_string(), "opacity:decrease".to_string());

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
            revision: None,
        })
    }

    /// Load the user file with path-rich, UI-ready diagnostics.
    pub fn load_with_diagnostics() -> KeyBindingsLoad {
        let path = match Self::config_path() {
            Ok(path) => path,
            Err(error) => {
                let mut fallback = Self::fallback_load();
                fallback
                    .diagnostics
                    .push(format!("Cannot locate keybindings config: {error}"));
                fallback.usable = false;
                return fallback;
            }
        };
        Self::load_path_with_diagnostics(&path)
    }

    fn fallback_load() -> KeyBindingsLoad {
        KeyBindingsLoad {
            bindings: Self::default_bindings(),
            diagnostics: Vec::new(),
            usable: true,
            revision: None,
        }
    }

    fn load_path_with_diagnostics(path: &std::path::Path) -> KeyBindingsLoad {
        let mut fallback = Self::fallback_load();
        let revision = match persistence::read_revision(path, MAX_KEYBINDINGS_BYTES) {
            Ok(revision) => revision,
            Err(error) => {
                fallback
                    .diagnostics
                    .push(format!("Cannot read {}: {error}", path.display()));
                fallback.usable = false;
                return fallback;
            }
        };
        if revision == FileRevision::Missing {
            fallback.revision = Some(revision);
            return fallback;
        }
        let content = match revision
            .bytes()
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
        {
            Some(content) => content,
            None => {
                fallback.diagnostics.push(format!(
                    "Cannot parse {}: file is not valid UTF-8",
                    path.display()
                ));
                fallback.usable = false;
                fallback.revision = Some(revision);
                return fallback;
            }
        };
        match Self::from_toml_with_diagnostics(content) {
            Ok(mut loaded) => {
                for diagnostic in &mut loaded.diagnostics {
                    *diagnostic = format!("{}: {diagnostic}", path.display());
                }
                loaded.revision = Some(revision);
                loaded
            }
            Err(error) => {
                fallback
                    .diagnostics
                    .push(format!("Cannot parse {}: {error}", path.display()));
                fallback.usable = false;
                fallback.revision = Some(revision);
                fallback
            }
        }
    }

    /// 获取配置文件路径
    pub fn config_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
        let config_dir = dirs::config_dir().ok_or("Could not determine config directory")?;
        Ok(config_dir.join("frost/keybindings.toml"))
    }

    pub fn config_revision() -> Result<FileRevision, String> {
        let path = Self::config_path().map_err(|error| error.to_string())?;
        persistence::read_revision(&path, MAX_KEYBINDINGS_BYTES)
            .map_err(|error| format!("Cannot read {}: {error}", path.display()))
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
            let path = std::env::temp_dir().join(format!(
                "frost-keybindings-{label}-{}",
                uuid::Uuid::new_v4()
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn test_command_parse() {
        let cmd: Command = "session:new".parse().unwrap();
        assert_eq!(cmd, Command::SessionNew);

        let cmd: Command = "session:jump:5".parse().unwrap();
        assert_eq!(cmd, Command::SessionJump(5));

        let cmd: Command = "sidebar:toggle".parse().unwrap();
        assert_eq!(cmd, Command::SidebarToggle);

        let cmd: Command = "pane:equalize".parse().unwrap();
        assert_eq!(cmd, Command::PaneEqualize);
        assert_eq!(cmd.to_string(), "pane:equalize");

        let cmd: Command = "edit:copy_block_output".parse().unwrap();
        assert_eq!(cmd, Command::EditCopyBlockOutput);
        assert_eq!(cmd.to_string(), "edit:copy_block_output");

        // The family id for this panel is singular, matching agent:toggle.
        let cmd: Command = "ai_chat:toggle".parse().unwrap();
        assert_eq!(cmd, Command::AiChatToggle);
        assert_eq!(cmd.to_string(), "ai_chat:toggle");
        assert!("ai_chats:toggle".parse::<Command>().is_err());
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
            // Plus-as-key: frost alone used to reject these; the family
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

    /// frost's command for each family-contract action. `None` rows are
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

    /// The family ergonomic contract, driven by core's table so frost can't
    /// silently drift from anvil/2/4: every `DEFAULT_CHORDS` row is either
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
    fn frost_chords_beyond_the_family_contract() {
        let bindings = KeyBindings::default_bindings();
        let cases = [
            ("ctrl+alt+shift+c", Command::EditCopyBlockOutput),
            ("ctrl+shift+a", Command::BlockSelectAll),
            ("ctrl+shift+k", Command::BlockClear),
            ("ctrl+shift+i", Command::BlockReinputSelectedCommands),
            ("ctrl+alt+g", Command::AgentToggle),
            ("ctrl+shift+alt+a", Command::AiChatToggle),
            ("ctrl+alt+r", Command::SearchReplaceToggle),
            ("ctrl+shift+x", Command::PaneSwap),
            ("ctrl+d", Command::TerminalSendEof),
            ("ctrl+shift+up", Command::TerminalPromptPrev),
            ("ctrl+shift+down", Command::TerminalPromptNext),
            ("ctrl+shift+g", Command::TerminalCopyLastOutput),
            ("ctrl+,", Command::BlockJumpPrevBookmark),
            ("ctrl+.", Command::BlockJumpNextBookmark),
            ("ctrl+alt+x", Command::BlockFixWithAgent),
            ("ctrl+alt+e", Command::BlockExplainWithAgent),
            ("ctrl+alt+t", Command::BlockRetryFailed),
            ("ctrl+alt+z", Command::BlockToggleCollapse),
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
    fn block_commands_parse_and_p0_batch_actions_ship_bound() {
        for (name, expected) in [
            ("block:jump_first_failed", Command::BlockJumpFirstFailed),
            ("block:jump_prev_failed", Command::BlockJumpPrevFailed),
            ("block:jump_next_failed", Command::BlockJumpNextFailed),
            ("block:copy_command", Command::BlockCopyCommand),
            ("block:copy_output", Command::BlockCopyOutput),
            ("block:recall_command", Command::BlockRecallCommand),
            ("block:select_all", Command::BlockSelectAll),
            ("block:clear", Command::BlockClear),
            ("block:undo_clear", Command::BlockUndoClear),
            ("block:select_prev", Command::BlockSelectPrev),
            ("block:select_next", Command::BlockSelectNext),
            (
                "block:reinput_selected_commands",
                Command::BlockReinputSelectedCommands,
            ),
            ("block:copy_block", Command::BlockCopyBlock),
            ("block:copy_markdown", Command::BlockCopyMarkdown),
            (
                "block:export_session_markdown",
                Command::BlockExportSessionMarkdown,
            ),
            ("block:export_session_json", Command::BlockExportSessionJson),
            ("block:search", Command::BlockSearch),
            ("block:toggle_bookmark", Command::BlockToggleBookmark),
            ("block:jump_prev_bookmark", Command::BlockJumpPrevBookmark),
            ("block:jump_next_bookmark", Command::BlockJumpNextBookmark),
            ("block:fix_with_agent", Command::BlockFixWithAgent),
            ("block:explain_with_agent", Command::BlockExplainWithAgent),
            ("block:retry_failed", Command::BlockRetryFailed),
            ("block:toggle_collapse", Command::BlockToggleCollapse),
        ] {
            assert_eq!(name.parse::<Command>().as_ref(), Ok(&expected), "{name}");
            assert_eq!(expected.to_string(), name);
        }
        // anvil/4 bind jump-first-failed to ctrl+shift+x, but that chord is
        // pane:swap here; ctrl+alt+up/down (select-prev/next elsewhere) and
        // ctrl+alt+left/right (the natural jump-prev/next-failed chords) are
        // pane focus here — so those block commands have no default chord.
        let bindings = KeyBindings::default_bindings();
        assert_eq!(
            bindings.get_command("ctrl+shift+x"),
            Some(Command::PaneSwap)
        );
        assert_eq!(
            bindings.get_command("ctrl+alt+up"),
            Some(Command::PaneFocusUp)
        );
        assert_eq!(
            bindings.get_command("ctrl+alt+down"),
            Some(Command::PaneFocusDown)
        );
        assert_eq!(
            bindings.get_command("ctrl+alt+left"),
            Some(Command::PaneFocusLeft)
        );
        assert_eq!(
            bindings.get_command("ctrl+alt+right"),
            Some(Command::PaneFocusRight)
        );
        // Search keeps frost's free ctrl+alt chord; the P0 batch actions use
        // the same family chords as anvil/forge.
        assert_eq!(
            bindings.get_command("ctrl+alt+f"),
            Some(Command::BlockSearch)
        );
        assert_eq!(
            bindings.get_command("ctrl+shift+a"),
            Some(Command::BlockSelectAll)
        );
        assert_eq!(
            bindings.get_command("ctrl+shift+i"),
            Some(Command::BlockReinputSelectedCommands)
        );
        assert_eq!(
            bindings.get_command("ctrl+shift+k"),
            Some(Command::BlockClear)
        );
        assert_eq!(
            bindings.get_command("ctrl+shift+b"),
            Some(Command::BlockToggleBookmark)
        );
        assert_eq!(
            bindings.get_command("ctrl+,"),
            Some(Command::BlockJumpPrevBookmark)
        );
        assert_eq!(
            bindings.get_command("ctrl+."),
            Some(Command::BlockJumpNextBookmark)
        );
        let mut bound_block_commands: Vec<&str> = bindings
            .bindings
            .values()
            .filter(|command| command.starts_with("block:"))
            .map(String::as_str)
            .collect();
        bound_block_commands.sort_unstable();
        assert_eq!(
            bound_block_commands,
            vec![
                "block:clear",
                "block:explain_with_agent",
                "block:fix_with_agent",
                "block:jump_next_bookmark",
                "block:jump_prev_bookmark",
                "block:reinput_selected_commands",
                "block:retry_failed",
                "block:search",
                "block:select_all",
                "block:toggle_bookmark",
                "block:toggle_collapse",
            ]
        );
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
    fn failed_reload_keeps_last_known_good_and_tracks_only_observed_bytes() {
        let root = ScratchDir::new("last-known-good");
        let path = root.0.join("keybindings.toml");
        write_private(&path, "\"ctrl+shift+k\" = \"session:new\"\n");
        let loaded = KeyBindings::load_path_with_diagnostics(&path);
        assert!(loaded.usable);
        let good_revision = loaded.revision.clone().expect("exact valid revision");
        let mut active = loaded.bindings;
        assert_eq!(
            active.get_command("ctrl+shift+k"),
            Some(Command::SessionNew)
        );

        write_private(&path, b"not = [valid toml");
        let failed = KeyBindings::load_path_with_diagnostics(&path);
        assert!(!failed.usable);
        assert_ne!(failed.revision.as_ref(), Some(&good_revision));
        if failed.usable {
            active = failed.bindings;
        }
        assert_eq!(
            active.get_command("ctrl+shift+k"),
            Some(Command::SessionNew),
            "a parse failure must not replace the live table with defaults"
        );

        // A transient/non-regular read has no revision, so the polling layer
        // will retry instead of mistaking the failure for a successful load.
        let unreadable = KeyBindings::load_path_with_diagnostics(&root.0);
        assert!(!unreadable.usable);
        assert!(unreadable.revision.is_none());
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
