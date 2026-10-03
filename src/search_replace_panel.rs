//! 查找替换面板状态（Ctrl+Alt+R 打开；引擎逻辑见 search_replace.rs）。
//!
//! 语义与 ember 的同名面板一致：终端 scrollback 是只读程序输出，原地替换
//! 无意义。面板对「当前选中文本」做查找替换，默认把结果复制到剪贴板；也可
//! 显式回填到活动 pane 的提示符（不带回车，避免误执行）。iced 视图与按键
//! 路由在 main.rs（`search_replace_panel` / `handle_search_replace_key`），
//! 本模块只保存状态并提供纯的面板到引擎胶水 [`SearchReplacePanelState::apply`]。
use crate::search_replace::{ReplaceOptions, SearchAndReplaceEngine, SearchConfig};

fn bound_field(text: impl Into<String>, max_bytes: usize) -> String {
    let mut text: String = text
        .into()
        .chars()
        .filter_map(|character| {
            if character.is_control() {
                None
            } else if jterm_core::review_input::is_visual_spoofing_character(character) {
                Some('\u{fffd}')
            } else {
                Some(character)
            }
        })
        .collect();
    if text.len() > max_bytes {
        let mut end = max_bytes;
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    text
}

fn replacement_output_is_unsafe(text: &str) -> bool {
    text.chars().any(|character| {
        if matches!(character, '\n' | '\t' | '\r') {
            false
        } else {
            character.is_control()
                || character == '\u{fffd}'
                || jterm_core::review_input::is_visual_spoofing_character(character)
        }
    })
}

/// 调用方需要执行的动作（面板本身不持有终端/剪贴板）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchReplaceAction {
    /// 对选中文本替换后复制到剪贴板。
    ReplaceToClipboard,
    /// 对选中文本替换后回填到活动 pane 的提示符（不带换行）。
    TypeIntoTerminal,
}

/// 面板状态。输入与开关跨开关面板保留（与搜索栏一致），便于反复应用。
pub struct SearchReplacePanelState {
    pub is_open: bool,
    pub search_input: String,
    pub replace_input: String,
    pub config: SearchConfig,
    pub options: ReplaceOptions,
    /// 最近一次操作的结果行（替换计数 / 错误 / "No selection"）。
    pub status: String,
}

impl Default for SearchReplacePanelState {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchReplacePanelState {
    pub fn new() -> Self {
        Self {
            is_open: false,
            search_input: String::new(),
            replace_input: String::new(),
            config: SearchConfig::default(),
            options: ReplaceOptions::default(),
            status: String::new(),
        }
    }

    pub fn toggle(&mut self) {
        self.is_open = !self.is_open;
    }

    pub fn set_search_input(&mut self, text: impl Into<String>) {
        self.search_input = bound_field(text, crate::search::MAX_SEARCH_QUERY_BYTES);
    }

    pub fn set_replace_input(&mut self, text: impl Into<String>) {
        self.replace_input = bound_field(text, crate::review_text::MAX_PROMPT_INSERT_BYTES);
    }

    /// 对给定文本执行替换，更新状态行，返回替换后的文本（失败返回 `None`）。
    pub fn apply(&mut self, text: &str) -> Option<String> {
        match SearchAndReplaceEngine::search_and_replace(
            text,
            &self.search_input,
            &self.replace_input,
            &self.config,
            &self.options,
        ) {
            Ok((result, count)) => {
                if replacement_output_is_unsafe(&result) {
                    self.status = crate::review_text::bound_query_error(
                        "replacement contains control or visual-spoofing characters",
                    );
                    return None;
                }
                self.status = format!("{} replacement(s)", count);
                Some(result)
            }
            Err(e) => {
                self.status = crate::review_text::bound_query_error(e);
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_replaces_first_match_by_default_and_reports_count() {
        let mut panel = SearchReplacePanelState::new();
        panel.search_input = "hello".to_string();
        panel.replace_input = "hi".to_string();

        assert_eq!(
            panel.apply("hello world hello").as_deref(),
            Some("hi world hello")
        );
        assert_eq!(panel.status, "1 replacement(s)");
    }

    #[test]
    fn apply_replace_all_covers_every_match() {
        let mut panel = SearchReplacePanelState::new();
        panel.search_input = "hello".to_string();
        panel.replace_input = "hi".to_string();
        panel.options.replace_all = true;

        assert_eq!(
            panel.apply("hello world hello").as_deref(),
            Some("hi world hi")
        );
        assert_eq!(panel.status, "2 replacement(s)");
    }

    #[test]
    fn apply_whole_word_skips_substring_matches() {
        let mut panel = SearchReplacePanelState::new();
        panel.search_input = "cat".to_string();
        panel.replace_input = "dog".to_string();
        panel.config.whole_word = true;
        panel.options.replace_all = true;

        assert_eq!(
            panel.apply("cat category cat").as_deref(),
            Some("dog category dog")
        );
        assert_eq!(panel.status, "2 replacement(s)");
    }

    #[test]
    fn apply_whole_word_bounds_regex_matches() {
        let mut panel = SearchReplacePanelState::new();
        panel.search_input = "err".to_string();
        panel.replace_input = "X".to_string();
        panel.config.use_regex = true;
        panel.config.whole_word = true;
        panel.options.replace_all = true;

        assert_eq!(
            panel.apply("Err error ERRORS").as_deref(),
            Some("X error ERRORS")
        );
        assert_eq!(panel.status, "1 replacement(s)");
    }

    #[test]
    fn apply_empty_pattern_is_a_noop_with_zero_count() {
        let mut panel = SearchReplacePanelState::new();
        panel.replace_input = "hi".to_string();

        assert_eq!(panel.apply("unchanged").as_deref(), Some("unchanged"));
        assert_eq!(panel.status, "0 replacement(s)");
    }

    #[test]
    fn apply_invalid_regex_reports_the_error_and_returns_none() {
        let mut panel = SearchReplacePanelState::new();
        panel.search_input = "(".to_string();
        panel.config.use_regex = true;

        assert_eq!(panel.apply("text"), None);
        assert!(panel.status.contains("Invalid regex"), "{}", panel.status);
        assert!(!panel.status.contains('\u{1b}'));
        assert!(!panel.status.contains('\u{202e}'));
    }

    #[test]
    fn apply_invalid_regex_does_not_echo_controls_from_the_pattern() {
        let mut panel = SearchReplacePanelState::new();
        panel.search_input = "(\u{1b}[31m\u{202e}".to_string();
        panel.config.use_regex = true;

        assert_eq!(panel.apply("text"), None);
        assert!(panel.status.contains("Invalid regex"), "{}", panel.status);
        assert!(!panel.status.contains('\u{1b}'));
        assert!(!panel.status.contains('\u{202e}'));
        assert!(panel.status.len() <= crate::review_text::MAX_REGEX_ERROR_BYTES);
        panel.status = format!("Invalid regex: \u{1b}[31m{}", "n".repeat(400));
        let shown = crate::review_text::bound_query_error(panel.status.clone());
        assert!(!shown.contains('\u{1b}'));
        assert!(shown.len() <= crate::review_text::MAX_REGEX_ERROR_BYTES);
    }

    #[test]
    fn toggle_preserves_inputs_across_open_close() {
        let mut panel = SearchReplacePanelState::new();
        panel.search_input = "kept".to_string();
        panel.toggle();
        assert!(panel.is_open);
        panel.toggle();
        assert!(!panel.is_open);
        assert_eq!(panel.search_input, "kept");
    }

    #[test]
    fn find_and_replace_fields_drop_controls_and_truncate() {
        let mut panel = SearchReplacePanelState::new();
        panel.set_search_input("er\nr\u{1b}or");
        assert_eq!(panel.search_input, "error");
        panel.set_search_input(format!(
            "{}y",
            "x".repeat(crate::search::MAX_SEARCH_QUERY_BYTES)
        ));
        assert_eq!(
            panel.search_input.len(),
            crate::search::MAX_SEARCH_QUERY_BYTES
        );
        assert!(!panel.search_input.contains('y'));

        panel.set_replace_input("hi\n\u{1b}");
        assert_eq!(panel.replace_input, "hi");
        panel.set_replace_input(format!(
            "{}z",
            "x".repeat(crate::review_text::MAX_PROMPT_INSERT_BYTES)
        ));
        assert_eq!(
            panel.replace_input.len(),
            crate::review_text::MAX_PROMPT_INSERT_BYTES
        );
        assert!(!panel.replace_input.contains('z'));
        panel.set_search_input("err\u{202e}or");
        assert!(!panel.search_input.contains('\u{202e}'));
        assert!(panel.search_input.contains('\u{fffd}'));
        panel.set_replace_input("ok\u{202e}");
        assert!(!panel.replace_input.contains('\u{202e}'));
        assert!(panel.replace_input.contains('\u{fffd}'));
    }

    #[test]
    fn apply_refuses_spoofed_replacement_output() {
        let mut panel = SearchReplacePanelState::new();
        panel.search_input = "hello".to_string();
        panel.replace_input = "ok\u{fffd}".to_string();
        assert_eq!(panel.apply("hello world"), None);

        panel.replace_input = "hi".to_string();
        assert_eq!(panel.apply("hello\u{202e} world"), None);
        assert_eq!(panel.apply("hello world").as_deref(), Some("hi world"));
    }
}
