/// 搜索功能模块
use crate::terminal::{Color, SearchLine, TerminalCell};
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
#[cfg(test)]
use std::borrow::Cow;
use std::collections::VecDeque;

const MAX_SEARCH_MATCHES: usize = 20_000;
const MATCH_LIMIT_MESSAGE: &str = "Showing the first 20,000 matches";

/// Compiled-regex cache slot. Held by `SearchState` so consecutive
/// `recompute_search` calls with the same pattern reuse the same `Regex`
/// instead of paying a fresh `RegexBuilder::build()` per keypress / PTY chunk.
#[derive(Clone, Debug)]
pub struct RegexCache {
    pattern: String,
    case_sensitive: bool,
    regex: Regex,
}

/// 单个搜索匹配项
#[derive(Clone, Debug, Copy, PartialEq, Eq)]
pub struct SearchMatch {
    /// Absolute row in the terminal buffer (`scrollback + live grid`).
    pub line: usize,
    /// 列起始位置
    pub col_start: usize,
    /// 列结束位置（不含）
    pub col_end: usize,
}

/// 搜索功能的完整状态
#[derive(Clone, Debug)]
pub struct SearchState {
    /// 搜索面板是否打开
    pub is_open: bool,

    /// 搜索输入框中的文本
    pub query: String,

    /// 是否使用正则表达式模式
    pub use_regex: bool,

    /// 是否大小写敏感
    pub case_sensitive: bool,

    /// 所有匹配项的列表
    pub matches: Vec<SearchMatch>,

    /// 当前选中的匹配项索引
    pub current_match_index: usize,

    /// 搜索历史队列（最近在前）
    pub history: VecDeque<SearchHistoryEntry>,

    /// 历史导航位置（None 表示在输入框，Some(i) 表示在历史第 i 项）
    pub history_nav_index: Option<usize>,

    /// 上次搜索词（用于检测搜索词变化）
    last_query: String,

    /// 搜索错误消息（正则表达式编译错误等）
    pub error_message: Option<String>,

    /// Cached compiled regex; reused while pattern + case-sensitive flag are unchanged.
    pub regex_cache: Option<RegexCache>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchHistoryEntry {
    pub query: String,
    pub is_regex: bool,
    pub case_sensitive: bool,
    pub timestamp: String,
}

impl Default for SearchState {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchState {
    /// 创建新的搜索状态
    pub fn new() -> Self {
        Self {
            is_open: false,
            query: String::new(),
            use_regex: false,
            case_sensitive: false,
            matches: Vec::new(),
            current_match_index: 0,
            history: VecDeque::new(),
            history_nav_index: None,
            last_query: String::new(),
            error_message: None,
            regex_cache: None,
        }
    }

    /// 打开或关闭搜索面板
    pub fn toggle(&mut self) {
        self.is_open = !self.is_open;
        if !self.is_open {
            self.close();
        }
    }

    /// 关闭搜索面板
    pub fn close(&mut self) {
        self.is_open = false;
        if !self.query.is_empty() && self.last_query != self.query {
            self.save_to_history();
            self.last_query = self.query.clone();
        }
    }

    /// 获取当前匹配项（如果有）
    pub fn current_match(&self) -> Option<SearchMatch> {
        if self.matches.is_empty() {
            None
        } else {
            Some(self.matches[self.current_match_index % self.matches.len()])
        }
    }

    /// 移动到下一个匹配项
    pub fn next_match(&mut self) {
        if !self.matches.is_empty() {
            self.current_match_index = (self.current_match_index + 1) % self.matches.len();
        }
    }

    /// 移动到上一个匹配项
    pub fn prev_match(&mut self) {
        if !self.matches.is_empty() {
            self.current_match_index = if self.current_match_index == 0 {
                self.matches.len() - 1
            } else {
                self.current_match_index - 1
            };
        }
    }

    /// 切换大小写敏感
    pub fn toggle_case_sensitive(&mut self) {
        self.case_sensitive = !self.case_sensitive;
        self.current_match_index = 0;
    }

    /// 切换正则表达式模式
    pub fn toggle_regex(&mut self) {
        self.use_regex = !self.use_regex;
        self.current_match_index = 0;
        self.error_message = None;
    }

    /// 保存当前搜索词到历史
    fn save_to_history(&mut self) {
        if self.query.is_empty() {
            return;
        }

        // 检查重复
        if !self.history.is_empty() && self.history[0].query == self.query {
            return;
        }

        self.history.push_front(SearchHistoryEntry {
            query: self.query.clone(),
            is_regex: self.use_regex,
            case_sensitive: self.case_sensitive,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| format!("{}", d.as_secs()))
                .unwrap_or_else(|_| "unknown".to_string()),
        });

        // 限制历史大小
        while self.history.len() > 50 {
            self.history.pop_back();
        }
    }

    /// 从历史中加载前一条
    pub fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }

        if let Some(idx) = self.history_nav_index {
            if idx + 1 < self.history.len() {
                self.history_nav_index = Some(idx + 1);
                let entry = &self.history[idx + 1];
                self.query = entry.query.clone();
                self.use_regex = entry.is_regex;
                self.case_sensitive = entry.case_sensitive;
            }
        } else {
            self.history_nav_index = Some(0);
            let entry = &self.history[0];
            self.query = entry.query.clone();
            self.use_regex = entry.is_regex;
            self.case_sensitive = entry.case_sensitive;
        }
    }

    /// 从历史中加载后一条
    pub fn history_next(&mut self) {
        if let Some(idx) = self.history_nav_index {
            if idx > 0 {
                self.history_nav_index = Some(idx - 1);
                let entry = &self.history[idx - 1];
                self.query = entry.query.clone();
                self.use_regex = entry.is_regex;
                self.case_sensitive = entry.case_sensitive;
            } else {
                // 返回输入框
                self.history_nav_index = None;
                self.query.clear();
            }
        }
    }
}

/// 搜索引擎（用于在可见网格中进行搜索）
pub struct SearchEngine;

impl SearchEngine {
    /// 在网格中搜索文本
    #[cfg(test)]
    pub fn search(
        grid: &[Vec<TerminalCell>],
        query: &str,
        use_regex: bool,
        case_sensitive: bool,
        regex_cache: &mut Option<RegexCache>,
    ) -> (Vec<SearchMatch>, Option<String>) {
        Self::search_lines(
            grid.iter()
                .map(|line| SearchLine::Cells(Cow::Borrowed(line.as_slice()))),
            query,
            use_regex,
            case_sensitive,
            regex_cache,
        )
    }

    /// Search an arbitrary terminal buffer without first cloning every row.
    /// Plain scrollback rows arrive as borrowed text with an identity
    /// character→column map; everything else arrives as cells. Match row
    /// indices follow iterator order.
    pub fn search_lines<'a>(
        lines: impl IntoIterator<Item = SearchLine<'a>>,
        query: &str,
        use_regex: bool,
        case_sensitive: bool,
        regex_cache: &mut Option<RegexCache>,
    ) -> (Vec<SearchMatch>, Option<String>) {
        if query.is_empty() {
            return (Vec::new(), None);
        }

        if use_regex {
            Self::search_regex(lines, query, case_sensitive, regex_cache)
        } else {
            let (matches, truncated) = Self::search_plaintext(lines, query, case_sensitive);
            (matches, truncated.then(|| MATCH_LIMIT_MESSAGE.to_string()))
        }
    }

    /// 普通文本搜索
    fn search_plaintext<'a>(
        lines: impl IntoIterator<Item = SearchLine<'a>>,
        query: &str,
        case_sensitive: bool,
    ) -> (Vec<SearchMatch>, bool) {
        let mut matches = Vec::new();
        let mut truncated = false;

        let search_query = if case_sensitive {
            query.to_string()
        } else {
            query.to_lowercase()
        };

        let query_chars = search_query.chars().count();
        'lines: for (line_idx, line) in lines.into_iter().enumerate() {
            let hit_cap = match line {
                SearchLine::Text(text) if case_sensitive => Self::collect_plaintext_line(
                    line_idx,
                    text,
                    &search_query,
                    query_chars,
                    Self::identity_span,
                    &mut matches,
                ),
                SearchLine::Text(text) => {
                    let (folded, columns) = Self::fold_plain_text(text);
                    Self::collect_plaintext_line(
                        line_idx,
                        &folded,
                        &search_query,
                        query_chars,
                        |char_index| columns.get(char_index).copied(),
                        &mut matches,
                    )
                }
                SearchLine::Cells(cells) => {
                    let (search_line, columns) =
                        Self::line_text_and_columns(&cells, !case_sensitive);
                    Self::collect_plaintext_line(
                        line_idx,
                        &search_line,
                        &search_query,
                        query_chars,
                        |char_index| columns.get(char_index).copied(),
                        &mut matches,
                    )
                }
            };
            if hit_cap {
                truncated = true;
                break 'lines;
            }
        }

        (matches, truncated)
    }

    /// Find every occurrence of `search_query` in one row's text, mapping the
    /// matched character span back to terminal cells through `span_at`.
    /// Returns true when the match cap stopped the scan inside this row.
    fn collect_plaintext_line(
        line_idx: usize,
        search_line: &str,
        search_query: &str,
        query_chars: usize,
        mut span_at: impl FnMut(usize) -> Option<(usize, usize)>,
        matches: &mut Vec<SearchMatch>,
    ) -> bool {
        let mut start_byte = 0;
        while let Some(rel) = search_line[start_byte..].find(search_query) {
            let match_byte = start_byte + rel;
            let char_start = search_line[..match_byte].chars().count();
            let char_end = char_start + query_chars;
            if let (Some((col_start, _)), Some((_, col_end))) =
                (span_at(char_start), span_at(char_end.saturating_sub(1)))
            {
                matches.push(SearchMatch {
                    line: line_idx,
                    col_start,
                    col_end,
                });
                if matches.len() >= MAX_SEARCH_MATCHES {
                    return true;
                }
            }
            // Advance one char past the match start, staying on a UTF-8
            // boundary (a raw +1 would panic inside a multi-byte char).
            let step = search_line[match_byte..]
                .chars()
                .next()
                .map(|c| c.len_utf8())
                .unwrap_or(1);
            start_byte = match_byte + step;
        }
        false
    }

    /// 正则表达式搜索
    fn search_regex<'a>(
        lines: impl IntoIterator<Item = SearchLine<'a>>,
        pattern: &str,
        case_sensitive: bool,
        cache: &mut Option<RegexCache>,
    ) -> (Vec<SearchMatch>, Option<String>) {
        let mut matches = Vec::new();

        // Reuse the cached regex when the pattern + case flag are unchanged;
        // otherwise (re)build and store. `RegexBuilder::build` is what we want
        // to avoid on every keystroke.
        let stale = match cache.as_ref() {
            Some(c) => c.pattern != pattern || c.case_sensitive != case_sensitive,
            None => true,
        };
        if stale {
            let mut builder = RegexBuilder::new(pattern);
            if !case_sensitive {
                builder.case_insensitive(true);
            }
            match builder.build() {
                Ok(r) => {
                    *cache = Some(RegexCache {
                        pattern: pattern.to_string(),
                        case_sensitive,
                        regex: r,
                    });
                }
                Err(e) => {
                    *cache = None;
                    return (Vec::new(), Some(format!("Invalid regex: {}", e)));
                }
            }
        }
        let regex = &cache.as_ref().unwrap().regex;
        let mut truncated = false;

        'lines: for (line_idx, line) in lines.into_iter().enumerate() {
            let hit_cap = match line {
                SearchLine::Text(text) => Self::collect_regex_line(
                    line_idx,
                    text,
                    regex,
                    Self::identity_span,
                    &mut matches,
                ),
                SearchLine::Cells(cells) => {
                    let (line_str, columns) = Self::line_text_and_columns(&cells, false);
                    Self::collect_regex_line(
                        line_idx,
                        &line_str,
                        regex,
                        |char_index| columns.get(char_index).copied(),
                        &mut matches,
                    )
                }
            };
            if hit_cap {
                truncated = true;
                break 'lines;
            }
        }

        (matches, truncated.then(|| MATCH_LIMIT_MESSAGE.to_string()))
    }

    /// Find every regex match in one row's text, mapping the matched
    /// character span back to terminal cells through `span_at`. Returns true
    /// when the match cap stopped the scan inside this row.
    fn collect_regex_line(
        line_idx: usize,
        line_str: &str,
        regex: &Regex,
        mut span_at: impl FnMut(usize) -> Option<(usize, usize)>,
        matches: &mut Vec<SearchMatch>,
    ) -> bool {
        for mat in regex.find_iter(line_str) {
            if mat.is_empty() {
                continue;
            }
            let char_start = line_str[..mat.start()].chars().count();
            let char_end = line_str[..mat.end()].chars().count();
            if let (Some((col_start, _)), Some((_, col_end))) =
                (span_at(char_start), span_at(char_end.saturating_sub(1)))
            {
                matches.push(SearchMatch {
                    line: line_idx,
                    col_start,
                    col_end,
                });
                if matches.len() >= MAX_SEARCH_MATCHES {
                    return true;
                }
            }
        }
        false
    }

    /// Character→cell span for borrowed plain rows. Their cells are narrow
    /// by construction (see `ScrollbackLine::search_text`), so the character
    /// index is the terminal column; match-derived indices are always in
    /// range, keeping this equivalent to the explicit column vectors below.
    fn identity_span(char_index: usize) -> Option<(usize, usize)> {
        Some((char_index, char_index + 1))
    }

    /// Case-fold borrowed plain text, keeping the identity character→column
    /// map: fold expansions (e.g. `İ` → `i` + combining dot) still point at
    /// their source cell, exactly like the per-cell fold below.
    fn fold_plain_text(text: &str) -> (String, Vec<(usize, usize)>) {
        let mut folded = String::with_capacity(text.len());
        let mut columns = Vec::with_capacity(text.len());
        for (column, ch) in text.chars().enumerate() {
            for lower in ch.to_lowercase() {
                folded.push(lower);
                columns.push((column, column + 1));
            }
        }
        (folded, columns)
    }

    /// Build searchable text and a character-to-cell mapping. Wide-character
    /// continuation placeholders are skipped so adjacent CJK text stays
    /// searchable. Case-fold expansions retain the source cell span.
    fn line_text_and_columns(
        line: &[TerminalCell],
        fold_case: bool,
    ) -> (String, Vec<(usize, usize)>) {
        // Ignore structural row padding so a query such as a single space cannot
        // turn a 100k×1024 buffer into tens of millions of meaningless hits.
        let meaningful_len = line
            .iter()
            .rposition(|cell| {
                cell.character != ' '
                    || cell.background != Color::Default
                    || cell.foreground != Color::Default
                    || cell.flags.wide()
                    || cell.flags.wide_continuation()
            })
            .map_or(0, |index| index + 1);
        let line = &line[..meaningful_len];
        let mut text = String::with_capacity(line.len());
        let mut columns = Vec::with_capacity(line.len());
        for (column, cell) in line.iter().enumerate() {
            if cell.flags.wide_continuation() {
                continue;
            }
            let end = column + usize::from(cell.flags.wide()) + 1;
            if fold_case {
                for ch in cell.character.to_lowercase() {
                    text.push(ch);
                    columns.push((column, end));
                }
            } else {
                text.push(cell.character);
                columns.push((column, end));
            }
        }
        (text, columns)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_search_state_toggle() {
        let mut state = SearchState::new();
        assert!(!state.is_open);
        state.toggle();
        assert!(state.is_open);
        state.toggle();
        assert!(!state.is_open);
    }

    #[test]
    fn test_match_navigation() {
        let mut state = SearchState::new();
        state.matches = vec![
            SearchMatch {
                line: 0,
                col_start: 0,
                col_end: 5,
            },
            SearchMatch {
                line: 1,
                col_start: 10,
                col_end: 15,
            },
        ];

        assert_eq!(state.current_match_index, 0);
        state.next_match();
        assert_eq!(state.current_match_index, 1);
        state.next_match();
        assert_eq!(state.current_match_index, 0); // 循环

        state.prev_match();
        assert_eq!(state.current_match_index, 1);
    }

    #[test]
    fn test_case_sensitive_toggle() {
        let mut state = SearchState::new();
        assert!(!state.case_sensitive);
        state.toggle_case_sensitive();
        assert!(state.case_sensitive);
    }

    #[test]
    fn test_regex_toggle() {
        let mut state = SearchState::new();
        assert!(!state.use_regex);
        state.toggle_regex();
        assert!(state.use_regex);
    }

    #[test]
    fn search_skips_wide_character_continuation_cells() {
        let mut row = vec![TerminalCell::default(); 6];
        row[0].character = '中';
        row[0].flags.set_wide(true);
        row[1].flags.set_wide_continuation(true);
        row[2].character = '文';
        row[2].flags.set_wide(true);
        row[3].flags.set_wide_continuation(true);
        let mut cache = None;

        let (matches, error) = SearchEngine::search(&[row], "中文", false, true, &mut cache);

        assert!(error.is_none());
        assert_eq!(
            matches,
            vec![SearchMatch {
                line: 0,
                col_start: 0,
                col_end: 4,
            }]
        );
    }

    #[test]
    fn case_fold_expansion_keeps_terminal_columns() {
        let mut row = vec![TerminalCell::default(); 3];
        row[0].character = 'İ';
        row[1].character = 'B';
        let mut cache = None;

        let (matches, _) = SearchEngine::search(&[row], "i\u{307}b", false, false, &mut cache);

        assert_eq!(matches[0].col_start, 0);
        assert_eq!(matches[0].col_end, 2);
    }

    #[test]
    fn structural_padding_is_not_searchable() {
        let row = vec![TerminalCell::default(); 1024];
        let mut cache = None;

        let (matches, _) = SearchEngine::search(&[row], " ", false, true, &mut cache);

        assert!(matches.is_empty());
    }

    #[test]
    fn match_count_is_bounded() {
        let cell = TerminalCell {
            character: 'A',
            ..TerminalCell::default()
        };
        let grid = vec![vec![cell; 1000]; 21];
        let mut cache = None;

        let (matches, warning) = SearchEngine::search(&grid, "A", false, true, &mut cache);

        assert_eq!(matches.len(), MAX_SEARCH_MATCHES);
        assert_eq!(warning.as_deref(), Some(MATCH_LIMIT_MESSAGE));
    }

    #[test]
    fn borrowed_plain_text_uses_identity_columns() {
        let lines = vec![SearchLine::Text("alpha beta alpha")];
        let mut cache = None;

        let (matches, error) = SearchEngine::search_lines(lines, "alpha", false, true, &mut cache);

        assert!(error.is_none());
        assert_eq!(
            matches,
            vec![
                SearchMatch {
                    line: 0,
                    col_start: 0,
                    col_end: 5,
                },
                SearchMatch {
                    line: 0,
                    col_start: 11,
                    col_end: 16,
                },
            ]
        );
    }

    #[test]
    fn borrowed_plain_text_case_fold_keeps_source_columns() {
        // `İ` folds to `i` + combining dot: the expansion still maps back to
        // its source cell, exactly like the per-cell fold.
        let lines = vec![SearchLine::Text("İB")];
        let mut cache = None;

        let (matches, _) = SearchEngine::search_lines(lines, "i\u{307}b", false, false, &mut cache);

        assert_eq!(matches[0].col_start, 0);
        assert_eq!(matches[0].col_end, 2);
    }

    #[test]
    fn mixed_borrowed_and_cell_rows_keep_line_indices_and_wide_spans() {
        let mut wide = vec![TerminalCell::default(); 4];
        wide[0].character = '中';
        wide[0].flags.set_wide(true);
        wide[1].flags.set_wide_continuation(true);
        wide[2].character = 'x';
        let lines = vec![
            SearchLine::Text("find me"),
            SearchLine::Cells(Cow::Borrowed(wide.as_slice())),
            SearchLine::Text("find again"),
        ];
        let mut cache = None;

        let (matches, _) = SearchEngine::search_lines(lines, "find", false, true, &mut cache);
        assert_eq!(
            matches,
            vec![
                SearchMatch {
                    line: 0,
                    col_start: 0,
                    col_end: 4,
                },
                SearchMatch {
                    line: 2,
                    col_start: 0,
                    col_end: 4,
                },
            ]
        );

        // A match starting on a wide character spans its continuation cell,
        // while the borrowed rows around it keep identity columns.
        let mut wide = vec![TerminalCell::default(); 4];
        wide[0].character = '中';
        wide[0].flags.set_wide(true);
        wide[1].flags.set_wide_continuation(true);
        wide[2].character = 'x';
        let lines = vec![
            SearchLine::Text("top"),
            SearchLine::Cells(Cow::Borrowed(wide.as_slice())),
        ];
        let (matches, _) = SearchEngine::search_lines(lines, "中x", false, true, &mut cache);
        assert_eq!(
            matches,
            vec![SearchMatch {
                line: 1,
                col_start: 0,
                col_end: 3,
            }]
        );
    }

    #[test]
    fn borrowed_text_match_count_is_bounded() {
        let line = "A".repeat(MAX_SEARCH_MATCHES + 1000);
        let lines = vec![SearchLine::Text(line.as_str())];
        let mut cache = None;

        let (matches, warning) = SearchEngine::search_lines(lines, "A", false, true, &mut cache);

        assert_eq!(matches.len(), MAX_SEARCH_MATCHES);
        assert_eq!(warning.as_deref(), Some(MATCH_LIMIT_MESSAGE));
    }

    #[test]
    fn regex_search_borrows_plain_text_with_identity_columns() {
        let lines = vec![
            SearchLine::Text("error: first"),
            SearchLine::Text("no problem"),
            SearchLine::Text("error: second"),
        ];
        let mut cache = None;

        let (matches, error) =
            SearchEngine::search_lines(lines, r"error: \w+", true, true, &mut cache);

        assert!(error.is_none());
        assert_eq!(
            matches,
            vec![
                SearchMatch {
                    line: 0,
                    col_start: 0,
                    col_end: 12,
                },
                SearchMatch {
                    line: 2,
                    col_start: 0,
                    col_end: 13,
                },
            ]
        );
    }
}
