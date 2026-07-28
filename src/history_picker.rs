//! 历史命令选择器：跨重启持久化命令的模糊搜索浮层（Ctrl+Shift+H 打开）。
//!
//! 记录与检索都建立在家族共享的 `jterm_core::command_history` JSONL 索引上
//! （与 jterm1/jterm4 同名配置键、同文件格式），因此几个兄弟终端可以指向同
//! 一份历史文件。Enter 只把选中的命令回填到活动 pane 的提示符，从不执行。
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use jterm_core::command_history::{self, CommandHistoryRecord};

/// 打开选择器时加载的最大条数。与 jterm4 的历史面板一致：交互检索只需要
/// 一个近期工作集，`read_recent` 本身也把读取限制在有界的文件尾部。
pub const PICKER_MAX_ENTRIES: usize = 2_000;

/// 一次渲染/导航的最大结果数。键盘选择与绘制共用 `filtered()`，因此上限
/// 同时约束两者——更早的命令通过输入查询来召回。
pub const MAX_RESULTS: usize = 15;

/// 把一条 OSC 133 重建的命令行修剪并校验为可持久化文本。返回 `None` 表示
/// 不应写入历史：空白命令，或含换行/控制字符的重建文本（例如 heredoc 的
/// 多行命令）——家族的 review-only 历史格式拒绝控制字符，这类文本也无法
/// 安全地回填到提示符。
pub fn sanitized_command(command: &str) -> Option<&str> {
    let trimmed = command.trim();
    jterm_core::review_input::validate(trimmed).ok()
}

/// 行展示的单行截断（按字符计，避免超长命令把浮层撑成多行）。只影响显示；
/// 回填到提示符的始终是完整命令文本。
pub fn display_command(command: &str) -> String {
    const MAX_DISPLAY_CHARS: usize = 120;
    if command.chars().count() <= MAX_DISPLAY_CHARS {
        return command.to_string();
    }
    let mut shortened: String = command.chars().take(MAX_DISPLAY_CHARS - 1).collect();
    shortened.push('…');
    shortened
}

/// 历史选择器状态。`entries` 最新在前（`read_recent` 的顺序），在打开浮层
/// 时加载一次；期间新完成的命令会在下一次打开时出现。
pub struct HistoryPickerState {
    pub query: String,
    /// 当前过滤结果中的高亮位置。
    pub selected: usize,
    entries: Vec<CommandHistoryRecord>,
    matcher: SkimMatcherV2,
}

impl HistoryPickerState {
    pub fn new(entries: Vec<CommandHistoryRecord>) -> Self {
        Self {
            query: String::new(),
            selected: 0,
            entries,
            matcher: SkimMatcherV2::default(),
        }
    }

    /// 从持久化索引加载最近的一段。读取是有界的（文件尾部窗口 + 条数上限），
    /// 文件缺失或损坏时得到一个空的选择器而不是错误。
    pub fn load(path: &std::path::Path) -> Self {
        Self::new(command_history::read_recent(path, PICKER_MAX_ENTRIES).unwrap_or_default())
    }

    /// 当前过滤结果（最多 [`MAX_RESULTS`] 条）。空查询保持最新在前；否则按
    /// 模糊匹配分数降序，同分保持新旧顺序（稳定排序），命令与 cwd 一起参与
    /// 匹配，便于按项目目录召回。
    pub fn filtered(&self) -> Vec<&CommandHistoryRecord> {
        if self.query.is_empty() {
            return self.entries.iter().take(MAX_RESULTS).collect();
        }
        let mut scored: Vec<(i64, &CommandHistoryRecord)> = self
            .entries
            .iter()
            .filter_map(|record| {
                let haystack = match record.cwd.as_deref() {
                    Some(cwd) => format!("{} {cwd}", record.command),
                    None => record.command.clone(),
                };
                self.matcher
                    .fuzzy_match(&haystack, &self.query)
                    .map(|score| (score, record))
            })
            .collect();
        scored.sort_by_key(|entry| std::cmp::Reverse(entry.0));
        scored
            .into_iter()
            .take(MAX_RESULTS)
            .map(|(_, record)| record)
            .collect()
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

    /// 当前高亮的命令文本（按过滤结果中的位置）。
    pub fn selected_command(&self) -> Option<String> {
        self.filtered()
            .get(self.selected)
            .map(|record| record.command.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(command: &str, cwd: Option<&str>, exit_code: i32) -> CommandHistoryRecord {
        CommandHistoryRecord {
            command: command.to_string(),
            cwd: cwd.map(str::to_string),
            exit_code,
            end_time_ms: None,
        }
    }

    #[test]
    fn sanitized_command_trims_and_rejects_unsafe_text() {
        assert_eq!(sanitized_command("  cargo test  "), Some("cargo test"));
        assert_eq!(sanitized_command(""), None);
        assert_eq!(sanitized_command("   "), None);
        // Multiline reconstructions (heredocs) cannot be replayed into one
        // prompt line and the family history format rejects control bytes.
        assert_eq!(sanitized_command("cat <<EOF\nhello\nEOF"), None);
        assert_eq!(sanitized_command("printf \u{7}"), None);
    }

    #[test]
    fn display_command_truncates_only_over_long_lines() {
        assert_eq!(display_command("cargo test"), "cargo test");
        let long = "x".repeat(500);
        let shown = display_command(&long);
        assert_eq!(shown.chars().count(), 120);
        assert!(shown.ends_with('…'));
    }

    #[test]
    fn empty_query_keeps_newest_first_order() {
        let state = HistoryPickerState::new(vec![
            record("newest", None, 0),
            record("middle", None, 0),
            record("oldest", None, 0),
        ]);
        let commands: Vec<&str> = state
            .filtered()
            .iter()
            .map(|r| r.command.as_str())
            .collect();
        assert_eq!(commands, vec!["newest", "middle", "oldest"]);
    }

    #[test]
    fn fuzzy_query_drops_non_matches_and_ranks_ties_by_recency() {
        let state = HistoryPickerState {
            query: "cargo".to_string(),
            ..HistoryPickerState::new(vec![
                record("cargo test", None, 0),
                record("git status", None, 0),
                record("cargo test", None, 1),
            ])
        };
        let filtered = state.filtered();
        assert_eq!(filtered.len(), 2);
        // Identical haystacks score identically; the stable sort keeps the
        // newer record first.
        assert_eq!(filtered[0].exit_code, 0);
        assert_eq!(filtered[1].exit_code, 1);
    }

    #[test]
    fn cwd_participates_in_matching() {
        let state = HistoryPickerState {
            query: "myproj".to_string(),
            ..HistoryPickerState::new(vec![
                record("make -j8", Some("/home/u/myproj"), 0),
                record("make -j8", Some("/home/u/other"), 0),
            ])
        };
        let filtered = state.filtered();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].cwd.as_deref(), Some("/home/u/myproj"));
    }

    #[test]
    fn results_are_capped_so_navigation_matches_the_drawn_list() {
        let entries = (0..MAX_RESULTS + 5)
            .map(|i| record(&format!("command-{i}"), None, 0))
            .collect();
        let mut state = HistoryPickerState::new(entries);
        assert_eq!(state.filtered().len(), MAX_RESULTS);

        state.select_prev();
        assert_eq!(state.selected, MAX_RESULTS - 1);
        state.select_next();
        assert_eq!(state.selected, 0);
        assert_eq!(state.selected_command().as_deref(), Some("command-0"));
    }

    #[test]
    fn load_reads_a_bounded_newest_first_slice() {
        let path = std::env::temp_dir().join(format!(
            "jterm3-history-picker-{}.jsonl",
            uuid::Uuid::new_v4()
        ));
        let mut contents = String::new();
        for i in 0..PICKER_MAX_ENTRIES + 10 {
            contents.push_str(&format!("{{\"command\":\"cmd-{i}\",\"exit_code\":0}}\n"));
        }
        std::fs::write(&path, contents).expect("write history fixture");

        let state = HistoryPickerState::load(&path);
        std::fs::remove_file(&path).expect("remove history fixture");

        assert_eq!(state.entries.len(), PICKER_MAX_ENTRIES);
        assert_eq!(
            state.entries.first().map(|r| r.command.as_str()),
            Some(format!("cmd-{}", PICKER_MAX_ENTRIES + 9).as_str())
        );
    }

    #[test]
    fn load_of_a_missing_file_yields_an_empty_picker() {
        let path = std::env::temp_dir().join(format!(
            "jterm3-history-picker-missing-{}.jsonl",
            uuid::Uuid::new_v4()
        ));
        let state = HistoryPickerState::load(&path);
        assert!(state.filtered().is_empty());
        assert_eq!(state.selected_command(), None);
    }
}
