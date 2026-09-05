/// 命令面板模块：可模糊搜索的动作列表（Ctrl+Shift+P 打开）。
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;

/// 面板可分发的动作，每一项都 1:1 对应一个已有的 frost 操作。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaletteAction {
    NewTab,
    CloseTab,
    NextTab,
    PrevTab,
    Copy,
    Paste,
    OpenSearch,
    OpenSearchReplace,
    SplitVertical,
    SplitHorizontal,
    FocusPaneLeft,
    FocusPaneRight,
    FocusPaneUp,
    FocusPaneDown,
    ResizePaneLeft,
    ResizePaneRight,
    ResizePaneUp,
    ResizePaneDown,
    ClosePane,
    ZoomPane,
    SwapPanes,
    EqualizePanes,
    ToggleSidebar,
    ToggleAgent,
    ToggleAiChats,
    AskAiGenerate,
    ToggleTasks,
    OpenSettings,
    QuickTabSwitch,
    OpenHelp,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    OpacityIncrease,
    OpacityDecrease,
    ScrollToTop,
    ScrollToBottom,
    PromptJumpPrev,
    PromptJumpNext,
    CopyLastOutput,
    BlockJumpFirstFailed,
    BlockJumpPrevFailed,
    BlockJumpNextFailed,
    BlockCopyCommand,
    BlockCopyOutput,
    BlockRecallCommand,
    BlockSelectAll,
    BlockClear,
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
    BlockFixWithAgent,
    BlockExplainWithAgent,
    BlockRetryFailed,
    BlockToggleCollapse,
    CommandHistory,
    OpenWorkflows,
    ClearScreen,
    InstallJsh,
}

/// 面板中的一条命令项（展示信息 + 关联动作）。
#[derive(Clone, Copy, Debug)]
pub struct PaletteItem {
    pub name: &'static str,
    pub description: &'static str,
    /// The configurable command this item runs, spelled exactly as the
    /// keybindings table spells it (`keybindings::Command`'s `Display` id).
    ///
    /// `Some` means the chord shown beside the row is read from the live
    /// binding table, so a rebound or unbound command reads correctly. The
    /// shipped defaults used to be baked in here as strings, which made this
    /// panel — whose whole job is to teach the keyboard — the surface most
    /// likely to be lying to a user who had configured one.
    pub binding: Option<&'static str>,
    /// Fallback hint for the entries that have no bindable command at all:
    /// the non-configurable app chrome resolved by `chrome_shortcut`, and the
    /// actions that are palette-only. Never used when `binding` is `Some`.
    pub shortcut: &'static str,
    pub action: PaletteAction,
}

impl PaletteItem {
    /// The chord to show beside this row, given the live binding table.
    ///
    /// A bindable command answers only from the table — an unbound one shows
    /// nothing rather than the default it no longer has.
    pub fn shortcut_label(&self, bindings: &crate::keybindings::KeyBindings) -> Option<String> {
        match self.binding {
            Some(command_id) => bindings.shortcut_label(command_id),
            None => (!self.shortcut.is_empty()).then(|| self.shortcut.to_string()),
        }
    }
}

/// 命令面板状态。
pub struct PaletteState {
    pub is_open: bool,
    pub query: String,
    /// 当前过滤结果中的高亮位置。
    pub selected: usize,
    all: Vec<PaletteItem>,
    matcher: SkimMatcherV2,
    /// Most-recent-first list of actions the user has executed. Drives the
    /// empty-query order so frequent commands surface first. Capped to 16.
    mru: Vec<PaletteAction>,
}

impl Default for PaletteState {
    fn default() -> Self {
        Self::new()
    }
}

impl PaletteState {
    pub fn new() -> Self {
        let all = vec![
            PaletteItem {
                name: "New Tab",
                description: "Open a new terminal tab",
                binding: Some("session:new"),
                shortcut: "",
                action: PaletteAction::NewTab,
            },
            PaletteItem {
                name: "Close Tab",
                description: "Close the current tab",
                binding: Some("session:close"),
                shortcut: "",
                action: PaletteAction::CloseTab,
            },
            PaletteItem {
                name: "Next Tab",
                description: "Switch to the next tab",
                binding: Some("session:next"),
                shortcut: "",
                action: PaletteAction::NextTab,
            },
            PaletteItem {
                name: "Previous Tab",
                description: "Switch to the previous tab",
                binding: Some("session:prev"),
                shortcut: "",
                action: PaletteAction::PrevTab,
            },
            PaletteItem {
                name: "Copy",
                description: "Copy selected text to the clipboard",
                binding: Some("edit:copy"),
                shortcut: "",
                action: PaletteAction::Copy,
            },
            PaletteItem {
                name: "Paste",
                description: "Paste from the clipboard",
                binding: Some("edit:paste"),
                shortcut: "",
                action: PaletteAction::Paste,
            },
            PaletteItem {
                name: "Find",
                description: "Open the search overlay",
                binding: Some("search:open"),
                shortcut: "",
                action: PaletteAction::OpenSearch,
            },
            PaletteItem {
                name: "Find & Replace",
                description: "Search-and-replace the current selection (clipboard or prompt)",
                binding: Some("search:replace:toggle"),
                shortcut: "",
                action: PaletteAction::OpenSearchReplace,
            },
            PaletteItem {
                name: "Split Right",
                description: "Add a pane beside the focused one (left | right)",
                binding: Some("terminal:split_vertical"),
                shortcut: "",
                action: PaletteAction::SplitVertical,
            },
            PaletteItem {
                name: "Split Down",
                description: "Add a pane below the focused one (top / bottom)",
                binding: Some("terminal:split_horizontal"),
                shortcut: "",
                action: PaletteAction::SplitHorizontal,
            },
            PaletteItem {
                name: "Focus Pane Left",
                description: "Move keyboard focus to the pane on the left",
                binding: Some("pane:focus_left"),
                shortcut: "",
                action: PaletteAction::FocusPaneLeft,
            },
            PaletteItem {
                name: "Focus Pane Right",
                description: "Move keyboard focus to the pane on the right",
                binding: Some("pane:focus_right"),
                shortcut: "",
                action: PaletteAction::FocusPaneRight,
            },
            PaletteItem {
                name: "Focus Pane Up",
                description: "Move keyboard focus to the pane above",
                binding: Some("pane:focus_up"),
                shortcut: "",
                action: PaletteAction::FocusPaneUp,
            },
            PaletteItem {
                name: "Focus Pane Down",
                description: "Move keyboard focus to the pane below",
                binding: Some("pane:focus_down"),
                shortcut: "",
                action: PaletteAction::FocusPaneDown,
            },
            PaletteItem {
                name: "Resize Pane Left",
                description: "Move the split divider to the left",
                binding: Some("pane:resize_left"),
                shortcut: "",
                action: PaletteAction::ResizePaneLeft,
            },
            PaletteItem {
                name: "Resize Pane Right",
                description: "Move the split divider to the right",
                binding: Some("pane:resize_right"),
                shortcut: "",
                action: PaletteAction::ResizePaneRight,
            },
            PaletteItem {
                name: "Resize Pane Up",
                description: "Move the split divider upward",
                binding: Some("pane:resize_up"),
                shortcut: "",
                action: PaletteAction::ResizePaneUp,
            },
            PaletteItem {
                name: "Resize Pane Down",
                description: "Move the split divider downward",
                binding: Some("pane:resize_down"),
                shortcut: "",
                action: PaletteAction::ResizePaneDown,
            },
            PaletteItem {
                name: "Zoom Pane",
                description: "Temporarily expand the focused pane to full size",
                binding: Some("pane:zoom_toggle"),
                shortcut: "",
                action: PaletteAction::ZoomPane,
            },
            PaletteItem {
                name: "Swap Panes",
                description: "Exchange the focused pane with the next one",
                binding: Some("pane:swap"),
                shortcut: "",
                action: PaletteAction::SwapPanes,
            },
            PaletteItem {
                name: "Equalize Panes",
                description: "Reset every pane divider to an even split",
                binding: Some("pane:equalize"),
                shortcut: "",
                action: PaletteAction::EqualizePanes,
            },
            PaletteItem {
                name: "Close Focused Pane",
                description: "Close the current pane, or its tab when unsplit",
                binding: Some("terminal:close_pane"),
                shortcut: "",
                action: PaletteAction::ClosePane,
            },
            PaletteItem {
                name: "Toggle Sidebar",
                description: "Show or hide the tabs and files sidebar",
                binding: Some("sidebar:toggle"),
                shortcut: "",
                action: PaletteAction::ToggleSidebar,
            },
            PaletteItem {
                name: "Toggle AI Agent",
                description: "Open or close the AI agent panel (per-command approval)",
                binding: Some("agent:toggle"),
                shortcut: "",
                action: PaletteAction::ToggleAgent,
            },
            PaletteItem {
                name: "AI Chats",
                description: "Open or close the persistent AI chats library",
                // Rendered in jterm_core's frozen display order
                // (Ctrl+Shift+Alt+Super), the same string forge shows for the
                // same panel — one chord must not read as two.
                binding: Some("ai_chat:toggle"),
                shortcut: "",
                action: PaletteAction::ToggleAiChats,
            },
            PaletteItem {
                name: "Ask AI: Generate Command",
                description: "Draft a shell command from a natural-language request; inserted for review, never runs automatically",
                binding: None,
                shortcut: "",
                action: PaletteAction::AskAiGenerate,
            },
            PaletteItem {
                name: "Toggle Tasks Dashboard",
                description: "Show or hide the experimental agent tasks panel",
                binding: None,
                shortcut: "",
                action: PaletteAction::ToggleTasks,
            },
            PaletteItem {
                name: "Settings",
                description: "Open terminal appearance and behavior settings",
                binding: Some("config:toggle"),
                shortcut: "",
                action: PaletteAction::OpenSettings,
            },
            PaletteItem {
                name: "Switch Tab",
                description: "Fuzzy-find and switch to an open tab",
                binding: None,
                shortcut: "Ctrl+Shift+L",
                action: PaletteAction::QuickTabSwitch,
            },
            PaletteItem {
                name: "Keyboard Shortcuts",
                description: "Show the built-in shortcut reference",
                binding: None,
                shortcut: "Ctrl+Shift+/",
                action: PaletteAction::OpenHelp,
            },
            PaletteItem {
                name: "Zoom In",
                description: "Increase terminal font size",
                binding: Some("font:zoom_in"),
                shortcut: "",
                action: PaletteAction::ZoomIn,
            },
            PaletteItem {
                name: "Zoom Out",
                description: "Decrease terminal font size",
                binding: Some("font:zoom_out"),
                shortcut: "",
                action: PaletteAction::ZoomOut,
            },
            PaletteItem {
                name: "Reset Zoom",
                description: "Restore the default terminal font size",
                binding: Some("font:zoom_reset"),
                shortcut: "",
                action: PaletteAction::ZoomReset,
            },
            PaletteItem {
                name: "Increase Opacity",
                description: "Make the window background more opaque",
                binding: Some("opacity:increase"),
                shortcut: "",
                action: PaletteAction::OpacityIncrease,
            },
            PaletteItem {
                name: "Decrease Opacity",
                description: "Make the window background more transparent",
                binding: Some("opacity:decrease"),
                shortcut: "",
                action: PaletteAction::OpacityDecrease,
            },
            PaletteItem {
                name: "Scroll to Top",
                description: "Jump to the top of the scrollback",
                binding: None,
                shortcut: "Shift+Home",
                action: PaletteAction::ScrollToTop,
            },
            PaletteItem {
                name: "Scroll to Bottom",
                description: "Jump to the live view",
                binding: None,
                shortcut: "Shift+End",
                action: PaletteAction::ScrollToBottom,
            },
            PaletteItem {
                name: "Previous Prompt",
                description: "Scroll to the previous shell prompt (OSC 133)",
                binding: Some("terminal:prompt_prev"),
                shortcut: "",
                action: PaletteAction::PromptJumpPrev,
            },
            PaletteItem {
                name: "Next Prompt",
                description: "Scroll to the next shell prompt (OSC 133)",
                binding: Some("terminal:prompt_next"),
                shortcut: "",
                action: PaletteAction::PromptJumpNext,
            },
            PaletteItem {
                name: "Copy Last Command Output",
                description: "Copy the previous command's output (OSC 133)",
                binding: Some("terminal:copy_last_output"),
                shortcut: "",
                action: PaletteAction::CopyLastOutput,
            },
            PaletteItem {
                name: "Jump to First Failed Block",
                description: "Select and reveal the oldest failed command block (OSC 133)",
                binding: Some("block:jump_first_failed"),
                shortcut: "",
                action: PaletteAction::BlockJumpFirstFailed,
            },
            PaletteItem {
                name: "Jump to Previous Failed Block",
                description: "Select and reveal the nearest older failed command block",
                binding: Some("block:jump_prev_failed"),
                shortcut: "",
                action: PaletteAction::BlockJumpPrevFailed,
            },
            PaletteItem {
                name: "Jump to Next Failed Block",
                description: "Select and reveal the nearest newer failed command block",
                binding: Some("block:jump_next_failed"),
                shortcut: "",
                action: PaletteAction::BlockJumpNextFailed,
            },
            PaletteItem {
                name: "Copy Block Command",
                description: "Copy the selected (or latest) command block's command line",
                binding: Some("block:copy_command"),
                shortcut: "",
                action: PaletteAction::BlockCopyCommand,
            },
            PaletteItem {
                name: "Copy Block Output",
                description: "Copy the selected (or latest) command block's output",
                binding: Some("block:copy_output"),
                shortcut: "",
                action: PaletteAction::BlockCopyOutput,
            },
            PaletteItem {
                name: "Recall Block Command",
                description: "Type the selected (or latest) block's command into the prompt",
                binding: Some("block:recall_command"),
                shortcut: "",
                action: PaletteAction::BlockRecallCommand,
            },
            PaletteItem {
                name: "Select All Blocks",
                description: "Select every retained finished block in the current pane",
                binding: Some("block:select_all"),
                shortcut: "",
                action: PaletteAction::BlockSelectAll,
            },
            PaletteItem {
                name: "Clear Blocks",
                description: "Remove every retained finished block from the current pane",
                binding: Some("block:clear"),
                shortcut: "",
                action: PaletteAction::BlockClear,
            },
            PaletteItem {
                name: "Undo Clear Blocks",
                description: "Restore the blocks removed by the most recent Clear Blocks",
                binding: Some("block:undo_clear"),
                shortcut: "",
                action: PaletteAction::BlockUndoClear,
            },
            PaletteItem {
                name: "Select Previous Block",
                description: "Select the previous (older) command block and reveal it",
                binding: Some("block:select_prev"),
                shortcut: "",
                action: PaletteAction::BlockSelectPrev,
            },
            PaletteItem {
                name: "Select Next Block",
                description: "Select the next (newer) command block and reveal it",
                binding: Some("block:select_next"),
                shortcut: "",
                action: PaletteAction::BlockSelectNext,
            },
            PaletteItem {
                name: "Reinput Selected Commands",
                description: "Type selected block commands into the prompt without running them",
                binding: Some("block:reinput_selected_commands"),
                shortcut: "",
                action: PaletteAction::BlockReinputSelectedCommands,
            },
            PaletteItem {
                name: "Copy Block",
                description: "Copy the selected (or latest) block's command and output",
                binding: Some("block:copy_block"),
                shortcut: "",
                action: PaletteAction::BlockCopyBlock,
            },
            PaletteItem {
                name: "Copy Blocks as Markdown",
                description: "Copy selected blocks (or latest block) as Markdown snippets",
                binding: Some("block:copy_markdown"),
                shortcut: "",
                action: PaletteAction::BlockCopyMarkdown,
            },
            PaletteItem {
                name: "Export Session Blocks as Markdown",
                description: "Write retained finalized blocks to a private Markdown file",
                binding: Some("block:export_session_markdown"),
                shortcut: "",
                action: PaletteAction::BlockExportSessionMarkdown,
            },
            PaletteItem {
                name: "Export Session Blocks as JSON",
                description: "Write retained finalized blocks to a private JSON file",
                binding: Some("block:export_session_json"),
                shortcut: "",
                action: PaletteAction::BlockExportSessionJson,
            },
            PaletteItem {
                name: "Collapse or Expand Block Output",
                description: "Fold the selected block's output into a summary row, or unfold it",
                binding: Some("block:toggle_collapse"),
                shortcut: "",
                action: PaletteAction::BlockToggleCollapse,
            },
            PaletteItem {
                name: "Search Blocks",
                description: "Search every command block's command and output",
                binding: Some("block:search"),
                shortcut: "",
                action: PaletteAction::BlockSearch,
            },
            PaletteItem {
                name: "Toggle Block Bookmark",
                description: "Bookmark or unbookmark the selected (or latest) block",
                binding: Some("block:toggle_bookmark"),
                shortcut: "",
                action: PaletteAction::BlockToggleBookmark,
            },
            PaletteItem {
                name: "Jump to Previous Block Bookmark",
                description: "Select and reveal the nearest older bookmarked block",
                binding: Some("block:jump_prev_bookmark"),
                shortcut: "",
                action: PaletteAction::BlockJumpPrevBookmark,
            },
            PaletteItem {
                name: "Jump to Next Block Bookmark",
                description: "Select and reveal the nearest newer bookmarked block",
                binding: Some("block:jump_next_bookmark"),
                shortcut: "",
                action: PaletteAction::BlockJumpNextBookmark,
            },
            PaletteItem {
                name: "Fix Failed Block with Agent",
                description: "Start a fresh Agent task to fix the selected (or latest) failed block",
                binding: Some("block:fix_with_agent"),
                shortcut: "",
                action: PaletteAction::BlockFixWithAgent,
            },
            PaletteItem {
                name: "Explain Failed Block with Agent",
                description: "Start a fresh Agent task to explain the selected (or latest) failed block",
                binding: Some("block:explain_with_agent"),
                shortcut: "",
                action: PaletteAction::BlockExplainWithAgent,
            },
            PaletteItem {
                name: "Retry Failed Block",
                description: "Replay the selected (or latest) failed block's exact command when its cwd still matches",
                binding: Some("block:retry_failed"),
                shortcut: "",
                action: PaletteAction::BlockRetryFailed,
            },
            PaletteItem {
                name: "Command History",
                description: "Fuzzy-search persisted commands and type one into the prompt",
                binding: None,
                shortcut: "Ctrl+Shift+H",
                action: PaletteAction::CommandHistory,
            },
            PaletteItem {
                name: "Workflows",
                description: "Pick a parameterized workflow, fill its arguments, and type the rendered command into the prompt",
                binding: None,
                shortcut: "Ctrl+Shift+M",
                action: PaletteAction::OpenWorkflows,
            },
            PaletteItem {
                name: "Clear Screen",
                description: "Clear the terminal screen",
                binding: Some("terminal:clear"),
                shortcut: "",
                action: PaletteAction::ClearScreen,
            },
            PaletteItem {
                name: "Install or update jsh",
                description: "Install jterm's companion shell, or update the installed one",
                binding: None,
                shortcut: "",
                action: PaletteAction::InstallJsh,
            },
        ];
        Self {
            is_open: false,
            query: String::new(),
            selected: 0,
            all,
            matcher: SkimMatcherV2::default(),
            mru: Vec::new(),
        }
    }

    /// Record an action as just-used so it sorts to the top of the empty-query
    /// list next time the palette is opened. Caps at 16 entries; duplicate
    /// inserts are deduplicated to the front.
    pub fn record_use(&mut self, action: PaletteAction) {
        self.mru.retain(|a| *a != action);
        self.mru.insert(0, action);
        const MRU_CAP: usize = 16;
        if self.mru.len() > MRU_CAP {
            self.mru.truncate(MRU_CAP);
        }
    }

    pub fn open(&mut self) {
        self.is_open = true;
        self.query.clear();
        self.selected = 0;
    }

    pub fn close(&mut self) {
        self.is_open = false;
    }

    pub fn toggle(&mut self) {
        if self.is_open {
            self.close();
        } else {
            self.open();
        }
    }

    /// 当前过滤结果，元素为 `(all 中的索引, 命令项)`。空查询时按 MRU 排序(最近使用
    /// 优先,其余按声明顺序);否则按模糊匹配分数降序排列,丢弃不匹配项。
    pub fn filtered(&self) -> Vec<(usize, &PaletteItem)> {
        if self.query.is_empty() {
            // MRU first (preserving recency order), then everything else in
            // declaration order so the list is stable and complete.
            let mut out: Vec<(usize, &PaletteItem)> = Vec::with_capacity(self.all.len());
            let mut seen = vec![false; self.all.len()];
            for a in &self.mru {
                if let Some((i, item)) = self.all.iter().enumerate().find(|(_, it)| it.action == *a)
                {
                    if !seen[i] {
                        seen[i] = true;
                        out.push((i, item));
                    }
                }
            }
            for (i, item) in self.all.iter().enumerate() {
                if !seen[i] {
                    out.push((i, item));
                }
            }
            return out;
        }
        let mut scored: Vec<(i64, usize, &PaletteItem)> = self
            .all
            .iter()
            .enumerate()
            .filter_map(|(i, item)| {
                let haystack = format!("{} {}", item.name, item.description);
                self.matcher
                    .fuzzy_match(&haystack, &self.query)
                    .map(|score| (score, i, item))
            })
            .collect();
        scored.sort_by_key(|entry| std::cmp::Reverse(entry.0));
        scored.into_iter().map(|(_, i, item)| (i, item)).collect()
    }

    /// 高亮项下移（在过滤结果中循环）。
    pub fn select_next(&mut self) {
        let len = self.filtered().len();
        if len == 0 {
            self.selected = 0;
        } else {
            self.selected = (self.selected + 1) % len;
        }
    }

    /// 高亮项上移（在过滤结果中循环）。
    pub fn select_prev(&mut self) {
        let len = self.filtered().len();
        if len == 0 {
            self.selected = 0;
        } else {
            self.selected = if self.selected == 0 {
                len - 1
            } else {
                self.selected - 1
            };
        }
    }

    /// 当前高亮项的动作（按过滤结果中的位置）。
    pub fn selected_action(&self) -> Option<PaletteAction> {
        self.filtered()
            .get(self.selected)
            .map(|(_, item)| item.action)
    }

    /// 按 `all` 中的索引取动作（用于鼠标点击分发）。
    pub fn action_at(&self, index: usize) -> Option<PaletteAction> {
        self.all.get(index).map(|item| item.action)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The hints the palette shows are read from the binding table rather
    /// than baked into the item list, so this now pins the DEFAULT table's
    /// rendering. The expected strings are unchanged — what changed is where
    /// they come from, and the two tests below prove the difference matters.
    #[test]
    fn shortcut_hints_follow_the_unified_default_contract() {
        let palette = PaletteState::new();
        let defaults = crate::keybindings::KeyBindings::default_bindings();
        let shortcut = |action| {
            palette
                .all
                .iter()
                .find(|item| item.action == action)
                .and_then(|item| item.shortcut_label(&defaults))
        };
        let cases = [
            (PaletteAction::SplitVertical, "Ctrl+Shift+E"),
            (PaletteAction::SplitHorizontal, "Ctrl+Shift+D"),
            (PaletteAction::FocusPaneLeft, "Ctrl+Alt+Left"),
            (PaletteAction::FocusPaneDown, "Ctrl+Alt+Down"),
            // Core's frozen modifier order is Ctrl+Shift+Alt+Super. The
            // hardcoded hints spelled these two "Ctrl+Alt+Shift+…", so the
            // palette printed a chord in an order the family's own renderer
            // never produces — the second thing reading the live table fixes.
            (PaletteAction::ResizePaneLeft, "Ctrl+Shift+Alt+Left"),
            (PaletteAction::ResizePaneDown, "Ctrl+Shift+Alt+Down"),
            (PaletteAction::ToggleSidebar, "Ctrl+\\"),
            (PaletteAction::ToggleAgent, "Ctrl+Alt+G"),
            (PaletteAction::ToggleAiChats, "Ctrl+Shift+Alt+A"),
            (PaletteAction::QuickTabSwitch, "Ctrl+Shift+L"),
            (PaletteAction::ZoomIn, "Ctrl+="),
            (PaletteAction::PromptJumpPrev, "Ctrl+Shift+Up"),
            (PaletteAction::PromptJumpNext, "Ctrl+Shift+Down"),
            (PaletteAction::CopyLastOutput, "Ctrl+Shift+G"),
            (PaletteAction::CommandHistory, "Ctrl+Shift+H"),
            (PaletteAction::OpenWorkflows, "Ctrl+Shift+M"),
            (PaletteAction::OpenSearchReplace, "Ctrl+Alt+R"),
            (PaletteAction::BlockSearch, "Ctrl+Alt+F"),
            (PaletteAction::BlockSelectAll, "Ctrl+Shift+A"),
            (PaletteAction::BlockClear, "Ctrl+Shift+K"),
            (PaletteAction::BlockReinputSelectedCommands, "Ctrl+Shift+I"),
            (PaletteAction::BlockToggleBookmark, "Ctrl+Shift+B"),
        ];
        for (action, expected) in cases {
            assert_eq!(shortcut(action).as_deref(), Some(expected), "{action:?}");
        }
    }

    /// Every configurable item names a command the binding table can actually
    /// resolve. A typo here would silently print nothing beside the row.
    #[test]
    fn every_palette_binding_names_a_real_command() {
        for item in &PaletteState::new().all {
            let Some(command_id) = item.binding else {
                continue;
            };
            let command = command_id
                .parse::<crate::keybindings::Command>()
                .unwrap_or_else(|error| panic!("{}: {command_id} — {error}", item.name));
            assert_eq!(
                command.to_string(),
                command_id,
                "{} must spell its command id exactly as the table does",
                item.name
            );
        }
    }

    /// The point of reading the live table: a user who rebinds a command is
    /// shown their chord, and a user who unbinds one is shown nothing rather
    /// than the default they deliberately removed.
    #[test]
    fn palette_hints_follow_a_rebind_and_disappear_on_an_unbind() {
        let palette = PaletteState::new();
        let copy = palette
            .all
            .iter()
            .find(|item| item.action == PaletteAction::Copy)
            .expect("the palette offers Copy");

        let mut rebound = crate::keybindings::KeyBindings::default_bindings();
        rebound
            .bindings
            .retain(|_, command| command.as_str() != "edit:copy");
        assert_eq!(copy.shortcut_label(&rebound), None, "an unbound command");

        rebound
            .bindings
            .insert("ctrl+alt+y".to_string(), "edit:copy".to_string());
        assert_eq!(copy.shortcut_label(&rebound).as_deref(), Some("Ctrl+Alt+Y"));

        // An item with no configurable command at all keeps its literal hint:
        // `chrome_shortcut` owns those chords and no table can move them.
        let switcher = palette
            .all
            .iter()
            .find(|item| item.action == PaletteAction::QuickTabSwitch)
            .expect("the palette offers the tab switcher");
        assert_eq!(switcher.binding, None);
        assert_eq!(
            switcher.shortcut_label(&rebound).as_deref(),
            Some("Ctrl+Shift+L")
        );
    }

    /// The AI panel's hint is the one place frost prints this chord, and the
    /// family prints it in `jterm_core`'s frozen display order
    /// (Ctrl+Shift+Alt+Super). Deriving the expected string from the default
    /// binding keeps the hint, the table and forge's rendering of the same
    /// chord from drifting into three spellings of one key.
    #[test]
    fn the_ai_chat_hint_is_the_core_rendering_of_its_default_binding() {
        let defaults = crate::keybindings::KeyBindings::default_bindings();
        let binding = defaults
            .bindings
            .iter()
            .find(|(_, command)| command.as_str() == "ai_chat:toggle")
            .map(|(binding, _)| binding.clone())
            .expect("the AI chat panel has a default binding");
        let rendered = jterm_core::keybindings::parse(&binding)
            .expect("the default binding parses")
            .display();
        let hint = PaletteState::new()
            .all
            .iter()
            .find(|item| item.action == PaletteAction::ToggleAiChats)
            .and_then(|item| item.shortcut_label(&defaults))
            .expect("the palette offers the AI chats panel");
        assert_eq!(hint, rendered);
        assert_eq!(rendered, "Ctrl+Shift+Alt+A");
    }
}
