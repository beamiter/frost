/// 搜索功能模块
use crate::terminal::{Color, SearchLine, TerminalCell};
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
#[cfg(test)]
use std::borrow::Cow;
use std::collections::VecDeque;

const MAX_SEARCH_MATCHES: usize = 20_000;
const MATCH_LIMIT_MESSAGE: &str = "Showing the first 20,000 matches";
/// One-line find query budget, matching block search and the overlay pickers
/// so a paste cannot compile an unbounded regex against scrollback.
pub(crate) const MAX_SEARCH_QUERY_BYTES: usize = jterm_core::workflows::MAX_PICKER_QUERY_BYTES;

fn bound_query_text(query: impl Into<String>) -> String {
    let mut query: String = query
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
    if query.len() > MAX_SEARCH_QUERY_BYTES {
        let mut end = MAX_SEARCH_QUERY_BYTES;
        while end > 0 && !query.is_char_boundary(end) {
            end -= 1;
        }
        query.truncate(end);
    }
    query
}

fn find_query_is_unsafe(query: &str) -> bool {
    query.contains('\u{fffd}') || jterm_core::review_input::contains_visual_spoofing(query)
}

/// Compiled-regex cache slot. Held by `SearchState` so consecutive
/// `recompute_search` calls with the same pattern reuse the same `Regex`
/// instead of paying a fresh `RegexBuilder::build()` per keypress / PTY chunk.
/// Failed compilations are cached too, until the pattern or case mode changes.
#[derive(Clone, Debug)]
pub struct RegexCache {
    pattern: String,
    case_sensitive: bool,
    compiled: Result<Regex, String>,
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

    /// Draft and mode flags to restore after navigating back out of history.
    history_draft: Option<(String, bool, bool)>,

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
            history_draft: None,
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
        self.save_to_history();
        self.history_nav_index = None;
        self.history_draft = None;
    }

    /// Replace the find query; the current-match highlight resets. Control
    /// characters are dropped and the byte budget is enforced on a char
    /// boundary so iced `text_input` and the raw-key path share one contract.
    pub fn set_query(&mut self, query: impl Into<String>) {
        let query = bound_query_text(query);
        if find_query_is_unsafe(&query) {
            self.error_message = Some(crate::review_text::bound_query_error(
                "Query contains control or visual-spoofing characters and was not saved",
            ));
            return;
        }
        if self.query == query {
            self.error_message = None;
            return;
        }
        self.query = query;
        self.history_draft = None;
        self.history_nav_index = None;
        self.current_match_index = 0;
        self.error_message = None;
    }

    /// Append typed text. Returns whether the stored query changed.
    pub fn push_query_text(&mut self, text: &str) -> bool {
        let previous = self.query.clone();
        let mut query = previous.clone();
        query.push_str(text);
        self.set_query(query);
        self.query != previous
    }

    /// Delete the last character of the query. Returns whether anything was
    /// deleted.
    pub fn backspace(&mut self) -> bool {
        if self.query.pop().is_none() {
            return false;
        }
        self.history_nav_index = None;
        self.history_draft = None;
        self.current_match_index = 0;
        true
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
        self.history_nav_index = None;
        self.history_draft = None;
        self.current_match_index = 0;
    }

    /// 切换正则表达式模式
    pub fn toggle_regex(&mut self) {
        self.use_regex = !self.use_regex;
        self.history_nav_index = None;
        self.history_draft = None;
        self.current_match_index = 0;
        self.error_message = None;
    }

    /// Find-bar diagnostic: regex compile failures quote the draft.
    pub fn set_error_message(&mut self, error: Option<String>) {
        self.error_message = error.map(crate::review_text::bound_query_error);
    }

    /// 保存当前搜索词到历史
    fn save_to_history(&mut self) {
        if self.query.is_empty() {
            return;
        }

        // 检查重复
        if self.history.front().is_some_and(|entry| {
            entry.query == self.query
                && entry.is_regex == self.use_regex
                && entry.case_sensitive == self.case_sensitive
        }) {
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

        let next = match self.history_nav_index {
            Some(idx) if idx + 1 < self.history.len() => idx + 1,
            None => 0,
            _ => return,
        };
        self.restore_history_entry(next);
    }

    /// 从历史中加载后一条
    pub fn history_next(&mut self) {
        if let Some(idx) = self.history_nav_index {
            if idx > 0 {
                self.restore_history_entry(idx - 1);
            } else {
                self.history_nav_index = None;
                if let Some((query, use_regex, case_sensitive)) = self.history_draft.take() {
                    self.query = query;
                    self.use_regex = use_regex;
                    self.case_sensitive = case_sensitive;
                } else {
                    self.query.clear();
                }
                self.current_match_index = 0;
                self.error_message = None;
            }
        }
    }

    fn restore_history_entry(&mut self, idx: usize) {
        let Some(entry) = self.history.get(idx) else {
            return;
        };
        let query = bound_query_text(entry.query.clone());
        let use_regex = entry.is_regex;
        let case_sensitive = entry.case_sensitive;
        if find_query_is_unsafe(&query) {
            self.error_message = Some(crate::review_text::bound_query_error(
                "Query contains control or visual-spoofing characters and was not restored",
            ));
            return;
        }
        if self.history_nav_index.is_none() {
            self.history_draft = Some((self.query.clone(), self.use_regex, self.case_sensitive));
        }
        self.query = query;
        self.use_regex = use_regex;
        self.case_sensitive = case_sensitive;
        self.history_nav_index = Some(idx);
        self.current_match_index = 0;
        self.error_message = None;
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
        if find_query_is_unsafe(query) {
            return (
                Vec::new(),
                Some(crate::review_text::bound_query_error(
                    "Query contains control or visual-spoofing characters",
                )),
            );
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
                // ASCII case folding never expands characters or changes cell
                // positions, so no per-character column map is needed.
                SearchLine::Text(text) if text.is_ascii() => {
                    let folded = text.to_ascii_lowercase();
                    Self::collect_plaintext_line(
                        line_idx,
                        &folded,
                        &search_query,
                        query_chars,
                        Self::identity_span,
                        &mut matches,
                    )
                }
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
        let mut start_char = 0;
        while let Some(rel) = search_line[start_byte..].find(search_query) {
            let match_byte = start_byte + rel;
            // Count only the new gap. Recounting the whole prefix for every
            // match makes column mapping quadratic on densely matching rows.
            let char_start = start_char + search_line[start_byte..match_byte].chars().count();
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
            start_char = char_start + 1;
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
            *cache = Some(RegexCache {
                pattern: pattern.to_string(),
                case_sensitive,
                compiled: builder
                    .build()
                    .map_err(crate::review_text::safe_regex_error),
            });
        }
        // Invalid drafts are common while typing. Cache failures too so each
        // PTY refresh does not compile the same invalid expression again.
        let regex = match &cache.as_ref().unwrap().compiled {
            Ok(regex) => regex,
            Err(error) => return (Vec::new(), Some(error.clone())),
        };
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
        let mut previous_byte = 0;
        let mut previous_char = 0;
        for mat in regex.find_iter(line_str) {
            if mat.is_empty() {
                continue;
            }
            // Regex matches do not overlap; each character in the row needs
            // to be counted at most once, including gaps between matches.
            let char_start = previous_char + line_str[previous_byte..mat.start()].chars().count();
            let char_end = char_start + mat.as_str().chars().count();
            previous_byte = mat.end();
            previous_char = char_end;
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
    fn history_navigation_restores_draft_and_modes() {
        let mut state = SearchState::new();
        state.set_query("older");
        state.close();
        state.set_query("newer");
        state.toggle_regex();
        state.close();
        state.set_query("unfinished");
        state.toggle_regex();
        state.toggle_case_sensitive();

        state.history_prev();
        assert_eq!(state.query, "newer");
        assert!(state.use_regex);
        assert!(!state.case_sensitive);
        state.history_prev();
        assert_eq!(state.query, "older");
        state.history_next();
        assert_eq!(state.query, "newer");
        state.history_next();
        assert_eq!(state.query, "unfinished");
        assert!(!state.use_regex);
        assert!(state.case_sensitive);
        assert!(state.history_nav_index.is_none());
        assert!(state.history_draft.is_none());
    }

    #[test]
    fn edited_history_becomes_the_new_draft() {
        let mut state = SearchState::new();
        state.set_query("saved");
        state.close();
        state.set_query("original draft");
        state.history_prev();
        state.push_query_text(" edit");
        state.history_prev();
        state.history_next();
        assert_eq!(state.query, "saved edit");
        state.history_prev();
        state.backspace();
        state.history_prev();
        state.history_next();
        assert_eq!(state.query, "save");
    }

    #[test]
    fn history_mode_edits_become_drafts_but_noop_input_preserves_navigation() {
        let mut state = SearchState::new();
        state.set_query("saved");
        state.close();
        state.set_query("draft");
        state.history_prev();
        assert!(!state.push_query_text("\n"));
        assert_eq!(state.history_nav_index, Some(0));
        state.history_next();
        assert_eq!(state.query, "draft");

        state.history_prev();
        state.toggle_regex();
        state.toggle_case_sensitive();
        assert!(state.history_nav_index.is_none());
        state.history_prev();
        assert!(!state.use_regex);
        assert!(!state.case_sensitive);
        state.history_next();
        assert_eq!(state.query, "saved");
        assert!(state.use_regex);
        assert!(state.case_sensitive);
    }

    #[test]
    fn history_remembers_mode_changes_and_resets_navigation_on_close() {
        let mut state = SearchState::new();
        state.set_query("same query");
        state.close();
        state.close();
        assert_eq!(state.history.len(), 1);
        state.toggle_regex();
        state.close();
        state.toggle_case_sensitive();
        state.close();
        assert_eq!(state.history.len(), 3);
        state.history_prev();
        state.history_prev();
        assert!(!state.case_sensitive);
        state.close();
        assert!(state.history_nav_index.is_none());
        state.history_prev();
        assert_eq!(state.history_nav_index, Some(0));
        assert!(state.use_regex);
        assert!(!state.case_sensitive);
    }

    // A deliberately simple prefix-counting oracle independent of the
    // optimized cursors. Exercise overlapping literals, UTF-8 gaps and fold
    // expansions in both borrowed scrollback and live-cell rows.
    #[test]
    fn plaintext_column_cursor_matches_reference() {
        for text in [
            "AAAAA",
            "aBaBa",
            "ééé-éaé",
            "İB İİB",
            "αβ ααβ",
            "",
            "--x--x",
        ] {
            let cells: Vec<_> = text
                .chars()
                .map(|character| TerminalCell {
                    character,
                    ..TerminalCell::default()
                })
                .collect();
            for query in [
                "A",
                "aa",
                "aba",
                "éé",
                "é",
                "i",
                "i\u{307}b",
                "αβ",
                "x",
                "missing",
            ] {
                for case_sensitive in [true, false] {
                    let (search_text, columns) = if case_sensitive {
                        (
                            text.to_string(),
                            text.chars().enumerate().map(|(i, _)| (i, i + 1)).collect(),
                        )
                    } else {
                        SearchEngine::fold_plain_text(text)
                    };
                    let query = if case_sensitive {
                        query.to_string()
                    } else {
                        query.to_lowercase()
                    };
                    let query_chars = query.chars().count();
                    let expected: Vec<_> = search_text
                        .char_indices()
                        .enumerate()
                        .filter(|(_, (byte, _))| search_text[*byte..].starts_with(&query))
                        .map(|(start, _)| SearchMatch {
                            line: 0,
                            col_start: columns[start].0,
                            col_end: columns[start + query_chars - 1].1,
                        })
                        .collect();
                    for line in [
                        SearchLine::Text(text),
                        SearchLine::Cells(Cow::Borrowed(&cells)),
                    ] {
                        let (actual, error) = SearchEngine::search_lines(
                            [line],
                            &query,
                            false,
                            case_sensitive,
                            &mut None,
                        );
                        assert!(error.is_none());
                        assert_eq!(
                            actual, expected,
                            "text={text:?}, query={query:?}, case={case_sensitive}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn regex_column_cursor_matches_reference_with_empty_matches_and_utf8_gaps() {
        for text in ["éAé B éAA", "αβ ααβ", "aaaa", "", "İB İİB"] {
            for pattern in ["A+", "a*", "^|A|$", "é|B", ".", "αβ", r"\b", "İB"] {
                for case_sensitive in [true, false] {
                    let regex = RegexBuilder::new(pattern)
                        .case_insensitive(!case_sensitive)
                        .build()
                        .unwrap();
                    let expected: Vec<_> = regex
                        .find_iter(text)
                        .filter(|mat| !mat.is_empty())
                        .map(|mat| SearchMatch {
                            line: 0,
                            col_start: text[..mat.start()].chars().count(),
                            col_end: text[..mat.end()].chars().count(),
                        })
                        .collect();
                    let (actual, error) = SearchEngine::search_lines(
                        [SearchLine::Text(text)],
                        pattern,
                        true,
                        case_sensitive,
                        &mut None,
                    );
                    assert!(error.is_none());
                    assert_eq!(actual, expected, "text={text:?}, pattern={pattern:?}");
                }
            }
        }
    }

    #[test]
    fn regex_cursor_maps_multiple_wide_cell_matches() {
        let mut cells = vec![TerminalCell::default(); 8];
        for start in [0, 4] {
            cells[start].character = '界';
            cells[start].flags.set_wide(true);
            cells[start + 1].flags.set_wide_continuation(true);
            cells[start + 2].character = 'é';
        }
        let (matches, error) = SearchEngine::search_lines(
            [SearchLine::Cells(Cow::Borrowed(&cells))],
            "界é",
            true,
            true,
            &mut None,
        );
        assert!(error.is_none());
        assert_eq!(
            matches,
            vec![
                SearchMatch {
                    line: 0,
                    col_start: 0,
                    col_end: 3
                },
                SearchMatch {
                    line: 0,
                    col_start: 4,
                    col_end: 7
                },
            ]
        );
    }

    #[test]
    fn regex_cache_keeps_failures_and_invalidates_on_pattern_or_mode_changes() {
        let mut cache = None;
        let (_, first_error) =
            SearchEngine::search_lines([SearchLine::Text("A")], "(", true, true, &mut cache);
        assert!(cache.as_ref().unwrap().compiled.is_err());
        let (_, repeated_error) =
            SearchEngine::search_lines([SearchLine::Text("A")], "(", true, true, &mut cache);
        assert_eq!(first_error, repeated_error);
        let (matches, error) =
            SearchEngine::search_lines([SearchLine::Text("A")], "a", true, true, &mut cache);
        assert!(matches.is_empty());
        assert!(error.is_none());
        assert!(cache.as_ref().unwrap().compiled.is_ok());
        let (matches, error) =
            SearchEngine::search_lines([SearchLine::Text("A")], "a", true, false, &mut cache);
        assert_eq!(matches.len(), 1);
        assert!(error.is_none());
        assert!(!cache.as_ref().unwrap().case_sensitive);
        let (matches, error) =
            SearchEngine::search_lines([SearchLine::Text("A")], "[", true, false, &mut cache);
        assert!(matches.is_empty());
        assert!(error.is_some());
        assert!(cache.as_ref().unwrap().compiled.is_err());
    }

    #[test]
    fn dense_unicode_matches_keep_exact_columns_and_cap() {
        let text = "é".repeat(MAX_SEARCH_MATCHES + 1);
        for use_regex in [false, true] {
            let (matches, warning) = SearchEngine::search_lines(
                [SearchLine::Text(&text)],
                "é",
                use_regex,
                true,
                &mut None,
            );
            assert_eq!(matches.len(), MAX_SEARCH_MATCHES);
            assert_eq!(warning.as_deref(), Some(MATCH_LIMIT_MESSAGE));
            for (column, found) in matches.iter().enumerate() {
                assert_eq!(found.col_start, column);
                assert_eq!(found.col_end, column + 1);
            }
        }
    }

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

    #[test]
    fn invalid_regex_error_does_not_echo_controls() {
        let mut cache = None;
        let (matches, error) = SearchEngine::search_lines(
            vec![SearchLine::Text("text")],
            "(\u{1b}[31m\u{202e}",
            true,
            true,
            &mut cache,
        );
        assert!(matches.is_empty());
        let error = error.expect("unsafe query");
        assert!(error.contains("visual-spoofing"));
        assert!(!error.contains('\u{1b}'));
        assert!(!error.contains('\u{202e}'));
        let (matches, error) =
            SearchEngine::search_lines(vec![SearchLine::Text("text")], "(", true, true, &mut cache);
        assert!(matches.is_empty());
        let error = error.expect("compile failure");
        assert!(error.contains("Invalid regex"));
        assert!(!error.contains('\u{1b}'));
        assert!(!error.contains('\u{202e}'));
        assert!(error.len() <= crate::review_text::MAX_REGEX_ERROR_BYTES);
        let mut state = SearchState::new();
        state.set_error_message(Some(format!(
            "Invalid regex: \u{1b}[31m{}",
            "x".repeat(400)
        )));
        let shown = state.error_message.expect("bounded error");
        assert!(!shown.contains('\u{1b}'));
        assert!(shown.len() <= crate::review_text::MAX_REGEX_ERROR_BYTES);
        assert!(shown.starts_with("Invalid regex"));
    }

    #[test]
    fn query_drops_controls_and_truncates_on_a_char_boundary() {
        let mut state = SearchState::new();
        state.current_match_index = 3;
        assert!(state.push_query_text("er\nr\u{1b}or"));
        assert_eq!(state.query, "error");
        assert_eq!(state.current_match_index, 0);
        assert!(!state.push_query_text("\n"));
        assert_eq!(state.query, "error");

        state.set_query(format!("{}x", "界".repeat(MAX_SEARCH_QUERY_BYTES)));
        assert!(state.query.len() <= MAX_SEARCH_QUERY_BYTES);
        assert!(state.query.is_char_boundary(state.query.len()));
        assert!(!state.query.contains('x'));

        state.set_query(format!("{}y", "x".repeat(MAX_SEARCH_QUERY_BYTES)));
        assert_eq!(state.query.len(), MAX_SEARCH_QUERY_BYTES);
        assert!(!state.query.contains('y'));
        let filled = state.query.clone();
        assert!(!state.push_query_text("z"));
        assert_eq!(state.query, filled);
        assert!(state.backspace());
        assert_eq!(state.query.len(), filled.len() - 1);

        state.history.push_front(SearchHistoryEntry {
            query: format!("{}!", "x".repeat(MAX_SEARCH_QUERY_BYTES + 8)),
            is_regex: false,
            case_sensitive: false,
            timestamp: "0".into(),
        });
        state.history_prev();
        assert_eq!(state.query.len(), MAX_SEARCH_QUERY_BYTES);
        assert!(!state.query.contains('!'));
        assert_eq!(state.history_nav_index, Some(0));
        state.set_query("keep");
        state.set_query("err\u{202e}or");
        assert_eq!(state.query, "keep");
        assert!(!state.query.contains('\u{202e}'));
        assert!(!state.query.contains('\u{fffd}'));
        assert!(state
            .error_message
            .as_deref()
            .is_some_and(|message| message.contains("not saved")));
        let mut row = vec![TerminalCell::default(); 8];
        for (index, character) in "error".chars().enumerate() {
            row[index].character = character;
        }
        let mut cache = None;
        let (matches, error) =
            SearchEngine::search(&[row], "err\u{fffd}or", false, true, &mut cache);
        assert!(matches.is_empty());
        assert!(error.is_some_and(|message| message.contains("visual-spoofing")));
    }

    #[test]
    fn history_restore_refuses_spoofed_queries() {
        let mut state = SearchState::new();
        state.set_query("keep");
        state.history.push_front(SearchHistoryEntry {
            query: "err\u{202e}or".into(),
            is_regex: false,
            case_sensitive: false,
            timestamp: "0".into(),
        });
        state.history_prev();
        assert_eq!(state.query, "keep");
        assert!(state.history_nav_index.is_none());
        assert!(state
            .error_message
            .as_deref()
            .is_some_and(|message| message.contains("not restored")));
    }
}
