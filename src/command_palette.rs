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
    ToggleSidebar,
    ToggleAgent,
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
    CommandHistory,
    ClearScreen,
    InstallJsh,
}

/// 面板中的一条命令项（展示信息 + 关联动作）。
#[derive(Clone, Copy, Debug)]
pub struct PaletteItem {
    pub name: &'static str,
    pub description: &'static str,
    /// Static shortcut hint. Keep synchronized with the default binding table
    /// and the small set of app-chrome shortcuts in `handle_tab_shortcut`.
    pub shortcut: &'static str,
    pub action: PaletteAction,
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
                shortcut: "Ctrl+Shift+T",
                action: PaletteAction::NewTab,
            },
            PaletteItem {
                name: "Close Tab",
                description: "Close the current tab",
                shortcut: "Ctrl+Shift+W",
                action: PaletteAction::CloseTab,
            },
            PaletteItem {
                name: "Next Tab",
                description: "Switch to the next tab",
                shortcut: "Ctrl+Tab",
                action: PaletteAction::NextTab,
            },
            PaletteItem {
                name: "Previous Tab",
                description: "Switch to the previous tab",
                shortcut: "Ctrl+Shift+Tab",
                action: PaletteAction::PrevTab,
            },
            PaletteItem {
                name: "Copy",
                description: "Copy selected text to the clipboard",
                shortcut: "Ctrl+Shift+C",
                action: PaletteAction::Copy,
            },
            PaletteItem {
                name: "Paste",
                description: "Paste from the clipboard",
                shortcut: "Ctrl+Shift+V",
                action: PaletteAction::Paste,
            },
            PaletteItem {
                name: "Find",
                description: "Open the search overlay",
                shortcut: "Ctrl+Shift+F",
                action: PaletteAction::OpenSearch,
            },
            PaletteItem {
                name: "Find & Replace",
                description: "Search-and-replace the current selection (clipboard or prompt)",
                shortcut: "Ctrl+Alt+R",
                action: PaletteAction::OpenSearchReplace,
            },
            PaletteItem {
                name: "Split Right",
                description: "Add a pane beside the focused one (left | right)",
                shortcut: "Ctrl+Shift+E",
                action: PaletteAction::SplitVertical,
            },
            PaletteItem {
                name: "Split Down",
                description: "Add a pane below the focused one (top / bottom)",
                shortcut: "Ctrl+Shift+D",
                action: PaletteAction::SplitHorizontal,
            },
            PaletteItem {
                name: "Focus Pane Left",
                description: "Move keyboard focus to the pane on the left",
                shortcut: "Ctrl+Alt+Left",
                action: PaletteAction::FocusPaneLeft,
            },
            PaletteItem {
                name: "Focus Pane Right",
                description: "Move keyboard focus to the pane on the right",
                shortcut: "Ctrl+Alt+Right",
                action: PaletteAction::FocusPaneRight,
            },
            PaletteItem {
                name: "Focus Pane Up",
                description: "Move keyboard focus to the pane above",
                shortcut: "Ctrl+Alt+Up",
                action: PaletteAction::FocusPaneUp,
            },
            PaletteItem {
                name: "Focus Pane Down",
                description: "Move keyboard focus to the pane below",
                shortcut: "Ctrl+Alt+Down",
                action: PaletteAction::FocusPaneDown,
            },
            PaletteItem {
                name: "Resize Pane Left",
                description: "Move the split divider to the left",
                shortcut: "Ctrl+Alt+Shift+Left",
                action: PaletteAction::ResizePaneLeft,
            },
            PaletteItem {
                name: "Resize Pane Right",
                description: "Move the split divider to the right",
                shortcut: "Ctrl+Alt+Shift+Right",
                action: PaletteAction::ResizePaneRight,
            },
            PaletteItem {
                name: "Resize Pane Up",
                description: "Move the split divider upward",
                shortcut: "Ctrl+Alt+Shift+Up",
                action: PaletteAction::ResizePaneUp,
            },
            PaletteItem {
                name: "Resize Pane Down",
                description: "Move the split divider downward",
                shortcut: "Ctrl+Alt+Shift+Down",
                action: PaletteAction::ResizePaneDown,
            },
            PaletteItem {
                name: "Zoom Pane",
                description: "Temporarily expand the focused pane to full size",
                shortcut: "Ctrl+Shift+Z",
                action: PaletteAction::ZoomPane,
            },
            PaletteItem {
                name: "Swap Panes",
                description: "Exchange the focused pane with the next one",
                shortcut: "Ctrl+Shift+X",
                action: PaletteAction::SwapPanes,
            },
            PaletteItem {
                name: "Close Focused Pane",
                description: "Close the current pane, or its tab when unsplit",
                shortcut: "Ctrl+Shift+W",
                action: PaletteAction::ClosePane,
            },
            PaletteItem {
                name: "Toggle Sidebar",
                description: "Show or hide the tabs and files sidebar",
                shortcut: "Ctrl+\\",
                action: PaletteAction::ToggleSidebar,
            },
            PaletteItem {
                name: "Toggle AI Agent",
                description: "Open or close the AI agent panel (per-command approval)",
                shortcut: "Ctrl+Alt+G",
                action: PaletteAction::ToggleAgent,
            },
            PaletteItem {
                name: "Toggle Tasks Dashboard",
                description: "Show or hide the experimental agent tasks panel",
                shortcut: "",
                action: PaletteAction::ToggleTasks,
            },
            PaletteItem {
                name: "Settings",
                description: "Open terminal appearance and behavior settings",
                shortcut: "Ctrl+Shift+O",
                action: PaletteAction::OpenSettings,
            },
            PaletteItem {
                name: "Switch Tab",
                description: "Fuzzy-find and switch to an open tab",
                shortcut: "Ctrl+Shift+L",
                action: PaletteAction::QuickTabSwitch,
            },
            PaletteItem {
                name: "Keyboard Shortcuts",
                description: "Show the built-in shortcut reference",
                shortcut: "Ctrl+Shift+/",
                action: PaletteAction::OpenHelp,
            },
            PaletteItem {
                name: "Zoom In",
                description: "Increase terminal font size",
                shortcut: "Ctrl+=",
                action: PaletteAction::ZoomIn,
            },
            PaletteItem {
                name: "Zoom Out",
                description: "Decrease terminal font size",
                shortcut: "Ctrl+-",
                action: PaletteAction::ZoomOut,
            },
            PaletteItem {
                name: "Reset Zoom",
                description: "Restore the default terminal font size",
                shortcut: "Ctrl+0",
                action: PaletteAction::ZoomReset,
            },
            PaletteItem {
                name: "Increase Opacity",
                description: "Make the window background more opaque",
                shortcut: "Ctrl+Alt+=",
                action: PaletteAction::OpacityIncrease,
            },
            PaletteItem {
                name: "Decrease Opacity",
                description: "Make the window background more transparent",
                shortcut: "Ctrl+Alt+-",
                action: PaletteAction::OpacityDecrease,
            },
            PaletteItem {
                name: "Scroll to Top",
                description: "Jump to the top of the scrollback",
                shortcut: "Shift+Home",
                action: PaletteAction::ScrollToTop,
            },
            PaletteItem {
                name: "Scroll to Bottom",
                description: "Jump to the live view",
                shortcut: "Shift+End",
                action: PaletteAction::ScrollToBottom,
            },
            PaletteItem {
                name: "Previous Prompt",
                description: "Scroll to the previous shell prompt (OSC 133)",
                shortcut: "Ctrl+Shift+Up",
                action: PaletteAction::PromptJumpPrev,
            },
            PaletteItem {
                name: "Next Prompt",
                description: "Scroll to the next shell prompt (OSC 133)",
                shortcut: "Ctrl+Shift+Down",
                action: PaletteAction::PromptJumpNext,
            },
            PaletteItem {
                name: "Copy Last Command Output",
                description: "Copy the previous command's output (OSC 133)",
                shortcut: "Ctrl+Shift+G",
                action: PaletteAction::CopyLastOutput,
            },
            PaletteItem {
                name: "Jump to First Failed Block",
                description: "Select and reveal the oldest failed command block (OSC 133)",
                shortcut: "",
                action: PaletteAction::BlockJumpFirstFailed,
            },
            PaletteItem {
                name: "Jump to Previous Failed Block",
                description: "Select and reveal the nearest older failed command block",
                shortcut: "",
                action: PaletteAction::BlockJumpPrevFailed,
            },
            PaletteItem {
                name: "Jump to Next Failed Block",
                description: "Select and reveal the nearest newer failed command block",
                shortcut: "",
                action: PaletteAction::BlockJumpNextFailed,
            },
            PaletteItem {
                name: "Copy Block Command",
                description: "Copy the selected (or latest) command block's command line",
                shortcut: "",
                action: PaletteAction::BlockCopyCommand,
            },
            PaletteItem {
                name: "Copy Block Output",
                description: "Copy the selected (or latest) command block's output",
                shortcut: "",
                action: PaletteAction::BlockCopyOutput,
            },
            PaletteItem {
                name: "Recall Block Command",
                description: "Type the selected (or latest) block's command into the prompt",
                shortcut: "",
                action: PaletteAction::BlockRecallCommand,
            },
            PaletteItem {
                name: "Select All Blocks",
                description: "Select every retained finished block in the current pane",
                shortcut: "Ctrl+Shift+A",
                action: PaletteAction::BlockSelectAll,
            },
            PaletteItem {
                name: "Clear Blocks",
                description: "Remove every retained finished block from the current pane",
                shortcut: "Ctrl+Shift+K",
                action: PaletteAction::BlockClear,
            },
            PaletteItem {
                name: "Select Previous Block",
                description: "Select the previous (older) command block and reveal it",
                shortcut: "",
                action: PaletteAction::BlockSelectPrev,
            },
            PaletteItem {
                name: "Select Next Block",
                description: "Select the next (newer) command block and reveal it",
                shortcut: "",
                action: PaletteAction::BlockSelectNext,
            },
            PaletteItem {
                name: "Reinput Selected Commands",
                description: "Type selected block commands into the prompt without running them",
                shortcut: "Ctrl+Shift+I",
                action: PaletteAction::BlockReinputSelectedCommands,
            },
            PaletteItem {
                name: "Copy Block",
                description: "Copy the selected (or latest) block's command and output",
                shortcut: "",
                action: PaletteAction::BlockCopyBlock,
            },
            PaletteItem {
                name: "Copy Blocks as Markdown",
                description: "Copy selected blocks (or latest block) as Markdown snippets",
                shortcut: "",
                action: PaletteAction::BlockCopyMarkdown,
            },
            PaletteItem {
                name: "Export Session Blocks as Markdown",
                description: "Write retained finalized blocks to a private Markdown file",
                shortcut: "",
                action: PaletteAction::BlockExportSessionMarkdown,
            },
            PaletteItem {
                name: "Export Session Blocks as JSON",
                description: "Write retained finalized blocks to a private JSON file",
                shortcut: "",
                action: PaletteAction::BlockExportSessionJson,
            },
            PaletteItem {
                name: "Search Blocks",
                description: "Search every command block's command and output",
                shortcut: "Ctrl+Alt+F",
                action: PaletteAction::BlockSearch,
            },
            PaletteItem {
                name: "Toggle Block Bookmark",
                description: "Bookmark or unbookmark the selected (or latest) block",
                shortcut: "Ctrl+Shift+B",
                action: PaletteAction::BlockToggleBookmark,
            },
            PaletteItem {
                name: "Jump to Previous Block Bookmark",
                description: "Select and reveal the nearest older bookmarked block",
                shortcut: "",
                action: PaletteAction::BlockJumpPrevBookmark,
            },
            PaletteItem {
                name: "Jump to Next Block Bookmark",
                description: "Select and reveal the nearest newer bookmarked block",
                shortcut: "",
                action: PaletteAction::BlockJumpNextBookmark,
            },
            PaletteItem {
                name: "Fix Failed Block with Agent",
                description: "Start a fresh Agent task to fix the selected (or latest) failed block",
                shortcut: "Ctrl+Alt+X",
                action: PaletteAction::BlockFixWithAgent,
            },
            PaletteItem {
                name: "Explain Failed Block with Agent",
                description: "Start a fresh Agent task to explain the selected (or latest) failed block",
                shortcut: "Ctrl+Alt+E",
                action: PaletteAction::BlockExplainWithAgent,
            },
            PaletteItem {
                name: "Retry Failed Block",
                description: "Replay the selected (or latest) failed block's exact command when its cwd still matches",
                shortcut: "Ctrl+Alt+T",
                action: PaletteAction::BlockRetryFailed,
            },
            PaletteItem {
                name: "Command History",
                description: "Fuzzy-search persisted commands and type one into the prompt",
                shortcut: "Ctrl+Shift+H",
                action: PaletteAction::CommandHistory,
            },
            PaletteItem {
                name: "Clear Screen",
                description: "Clear the terminal screen",
                shortcut: "",
                action: PaletteAction::ClearScreen,
            },
            PaletteItem {
                name: "Install or update jsh",
                description: "Install jterm's companion shell, or update the installed one",
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

    #[test]
    fn shortcut_hints_follow_the_unified_default_contract() {
        let palette = PaletteState::new();
        let shortcut = |action| {
            palette
                .all
                .iter()
                .find(|item| item.action == action)
                .map(|item| item.shortcut)
        };
        let cases = [
            (PaletteAction::SplitVertical, "Ctrl+Shift+E"),
            (PaletteAction::SplitHorizontal, "Ctrl+Shift+D"),
            (PaletteAction::FocusPaneLeft, "Ctrl+Alt+Left"),
            (PaletteAction::FocusPaneDown, "Ctrl+Alt+Down"),
            (PaletteAction::ResizePaneLeft, "Ctrl+Alt+Shift+Left"),
            (PaletteAction::ResizePaneDown, "Ctrl+Alt+Shift+Down"),
            (PaletteAction::ToggleSidebar, "Ctrl+\\"),
            (PaletteAction::ToggleAgent, "Ctrl+Alt+G"),
            (PaletteAction::QuickTabSwitch, "Ctrl+Shift+L"),
            (PaletteAction::ZoomIn, "Ctrl+="),
            (PaletteAction::PromptJumpPrev, "Ctrl+Shift+Up"),
            (PaletteAction::PromptJumpNext, "Ctrl+Shift+Down"),
            (PaletteAction::CopyLastOutput, "Ctrl+Shift+G"),
            (PaletteAction::CommandHistory, "Ctrl+Shift+H"),
            (PaletteAction::OpenSearchReplace, "Ctrl+Alt+R"),
            (PaletteAction::BlockSearch, "Ctrl+Alt+F"),
            (PaletteAction::BlockSelectAll, "Ctrl+Shift+A"),
            (PaletteAction::BlockClear, "Ctrl+Shift+K"),
            (PaletteAction::BlockReinputSelectedCommands, "Ctrl+Shift+I"),
            (PaletteAction::BlockToggleBookmark, "Ctrl+Shift+B"),
        ];
        for (action, expected) in cases {
            assert_eq!(shortcut(action), Some(expected), "{action:?}");
        }
    }
}
