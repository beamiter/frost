//! 历史命令选择器：跨重启持久化命令的模糊搜索浮层（Ctrl+Shift+H 打开）。
//!
//! 记录与检索都建立在家族共享的 `jterm_core::command_history` JSONL 索引上
//! （与 anvil/forge 同名配置键、同文件格式），因此几个兄弟终端可以指向同
//! 一份历史文件。Enter 只把选中的命令回填到活动 pane 的提示符，从不执行。
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use jterm_core::command_history::CommandHistoryRecord;
use std::borrow::Cow;
use std::collections::HashSet;
use std::sync::Arc;

/// 打开选择器时加载的最大条数。与 forge 的历史面板一致：交互检索只需要
/// 一个近期工作集，读取同样限制在有界的文件尾部。
pub const PICKER_MAX_ENTRIES: usize = 2_000;
const PICKER_TAIL_BYTES: u64 = 4 * 1024 * 1024;
const MAX_HISTORY_RECORD_BYTES: usize = 1024 * 1024;

/// Dynamic-programming fuzzy scoring costs command length times query length,
/// even though Skim compresses its score matrix to two rows. Bound work, not
/// just the input byte limits: a valid pasted query against long history rows
/// otherwise blocks the UI thread for seconds. Above this budget Skim's linear
/// subsequence scorer still searches the complete command and cwd; only ranking
/// changes. UTF-8 byte lengths conservatively bound the character work.
const MAX_HISTORY_FUZZY_WORK: usize = 64 * 1024;

fn use_linear_history_match(haystack: &str, query: &str) -> bool {
    haystack.len().saturating_mul(query.len()) > MAX_HISTORY_FUZZY_WORK
}

/// 一次渲染/导航的最大结果数。键盘选择与绘制共用 `filtered()`，因此上限
/// 同时约束两者——更早的命令通过输入查询来召回。
pub const MAX_RESULTS: usize = 15;
/// The cwd budget the shared writer enforces (`MAX_CWD_BYTES` in
/// `jterm_core::command_history`, 16 KiB — it is private there, so the number
/// is mirrored rather than imported). Reading with a stricter bound than the
/// writer means dropping the cwd off records that are perfectly well-formed
/// in the family file, so this must not drift below core's value.
const MAX_HISTORY_CWD_BYTES: usize = 16 * 1024;

/// A command's budget in the family-shared history JSONL, taken from that
/// file's own writer: `jterm_core::command_history` accepts up to
/// `review_input::MAX_REVIEW_INPUT_BYTES`. Reading it with the smaller OSC 133
/// replay budget instead would drop every longer record a sibling terminal
/// wrote — the record is valid, the reader just refuses it, and the user sees
/// history entries missing with no explanation.
pub(crate) const MAX_SHARED_HISTORY_COMMAND_BYTES: usize =
    jterm_core::review_input::MAX_REVIEW_INPUT_BYTES;

/// One-line overlay query budget, shared with the workflow picker so a paste
/// cannot grow the iced field and the per-entry fuzzy match without bound.
pub(crate) const MAX_HISTORY_QUERY_BYTES: usize = jterm_core::workflows::MAX_PICKER_QUERY_BYTES;

fn history_query_is_unsafe(query: &str) -> bool {
    query.contains('\u{fffd}') || jterm_core::review_input::contains_visual_spoofing(query)
}

fn bound_history_query(query: impl Into<String>) -> String {
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
    if query.len() > MAX_HISTORY_QUERY_BYTES {
        let mut end = MAX_HISTORY_QUERY_BYTES;
        while end > 0 && !query.is_char_boundary(end) {
            end -= 1;
        }
        query.truncate(end);
    }
    query
}

/// 把一条 OSC 133 重建的命令行修剪并校验为可持久化文本。返回 `None` 表示
/// 不应写入历史：空白命令，或含换行/控制字符的重建文本（例如 heredoc 的
/// 多行命令）——家族的 review-only 历史格式拒绝控制字符，这类文本也无法
/// 安全地回填到提示符。
pub fn sanitized_command(command: &str) -> Option<&str> {
    let trimmed = command.trim_matches(' ');
    crate::review_text::validate_single_line(trimmed, MAX_SHARED_HISTORY_COMMAND_BYTES).ok()
}

pub fn sanitized_cwd(cwd: &str) -> Option<&str> {
    if cwd.len() > MAX_HISTORY_CWD_BYTES
        || cwd.contains('\u{fffd}')
        || cwd.chars().any(char::is_control)
        || jterm_core::review_input::contains_visual_spoofing(cwd)
    {
        None
    } else {
        Some(cwd)
    }
}

/// 行展示的单行截断（按字符计，避免超长命令把浮层撑成多行）。只影响显示；
/// 回填到提示符的始终是完整命令文本。
pub fn display_command(command: &str) -> String {
    const MAX_DISPLAY_CHARS: usize = 120;
    if command.len() > MAX_SHARED_HISTORY_COMMAND_BYTES {
        return "(command omitted: exceeds review limit)".to_string();
    }
    let visible = crate::review_text::visible_bounded(command, 4 * 1024);
    if visible.chars().count() <= MAX_DISPLAY_CHARS {
        return visible;
    }
    let mut shortened: String = visible.chars().take(MAX_DISPLAY_CHARS - 1).collect();
    shortened.push('…');
    shortened
}

/// Compact cwd chrome for picker rows. The index may store 16 KiB paths; the
/// overlay only needs a short, formatting-safe suffix.
pub fn display_cwd(cwd: &str) -> String {
    const MAX_DISPLAY_CHARS: usize = 80;
    if cwd.len() > MAX_HISTORY_CWD_BYTES {
        return "(cwd omitted: exceeds history limit)".to_string();
    }
    let visible = crate::review_text::visible_bounded(cwd, 256);
    if visible.chars().count() <= MAX_DISPLAY_CHARS {
        return visible;
    }
    let mut shortened: String = visible.chars().take(MAX_DISPLAY_CHARS - 1).collect();
    shortened.push('…');
    shortened
}

/// Exit-status filter for the persisted command snapshot.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HistoryStatus {
    #[default]
    All,
    Success,
    Failed,
}

impl HistoryStatus {
    pub fn next(self) -> Self {
        match self {
            Self::All => Self::Success,
            Self::Success => Self::Failed,
            Self::Failed => Self::All,
        }
    }

    fn matches(self, exit_code: i32) -> bool {
        match self {
            Self::All => true,
            Self::Success => exit_code == 0,
            Self::Failed => exit_code != 0,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum HistoryFilterAction {
    ToggleDirectory,
    SetStatus(HistoryStatus),
    ToggleUnique,
    Reset,
}

/// 历史选择器状态。`entries` 最新在前，在打开浮层
/// 时加载一次；期间新完成的命令会在下一次打开时出现。
pub struct HistoryPickerState {
    query: String,
    /// 当前过滤结果中的高亮位置。
    pub selected: usize,
    entries: Vec<Arc<CommandHistoryRecord>>,
    matcher: SkimMatcherV2,
    linear_matcher: SkimMatcherV2,
    current_directory: Option<String>,
    directory_only: bool,
    status: HistoryStatus,
    unique: bool,
    /// Indices into the immutable entry snapshot. Query/filter changes do the
    /// expensive matching once; drawing, navigation and acceptance reuse it.
    results: Vec<usize>,
    match_count: usize,
    older_not_loaded: bool,
    #[cfg(test)]
    rebuild_count: usize,
}

/// Accept only a retained row from the currently open snapshot, then close
/// before dispatch. New/open/load always allocates fresh record Arcs. A click
/// retains its original row across query edits, but not close/reopen. Targeting
/// remains the active prompt at dispatch, as for keyboard history recall.
pub(crate) fn accept_clicked_record(
    picker: &mut Option<HistoryPickerState>,
    record: &Arc<CommandHistoryRecord>,
) -> Option<String> {
    let state = picker.as_ref()?;
    if !state.entries.iter().any(|current| Arc::ptr_eq(current, record)) {
        return None;
    }
    let command = record.command.clone();
    *picker = None;
    Some(command)
}

impl HistoryPickerState {
    pub fn new(mut entries: Vec<CommandHistoryRecord>) -> Self {
        entries.retain_mut(|record| {
            let Some(command) = sanitized_command(&record.command) else {
                return false;
            };
            if command.len() != record.command.len() {
                record.command = command.to_string();
            }
            if record
                .cwd
                .as_deref()
                .is_some_and(|cwd| sanitized_cwd(cwd).is_none())
            {
                record.cwd = None;
            }
            true
        });
        let mut state = Self {
            query: String::new(),
            selected: 0,
            entries: entries.into_iter().map(Arc::new).collect(),
            matcher: SkimMatcherV2::default(),
            // A nonempty match always exceeds one matrix element, selecting
            // Skim's linear fallback without truncating either input.
            linear_matcher: SkimMatcherV2::default().element_limit(1),
            current_directory: None,
            directory_only: false,
            status: HistoryStatus::All,
            unique: false,
            results: Vec::new(),
            match_count: 0,
            older_not_loaded: false,
            #[cfg(test)]
            rebuild_count: 0,
        };
        state.rebuild_results();
        state
    }

    /// 从持久化索引加载最近的一段。读取是有界的（文件尾部窗口 + 条数上限），
    /// 文件缺失或损坏时得到一个空的选择器而不是错误。
    pub fn load(path: &std::path::Path) -> Self {
        // The core's read_recent deduplicates by command before returning.
        // This view needs individual executions to filter older failures and
        // distinguish identical commands run in different directories.
        let snapshot = crate::persistence::prepare_command_history_path(path, false)
            .and_then(|()| crate::persistence::read_jsonl_tail(path, PICKER_TAIL_BYTES));
        let Ok((tail, tail_truncated)) = snapshot else {
            return Self::new(Vec::new());
        };
        let mut records = Vec::new();
        for line in tail
            .rsplit(|&byte| byte == b'\n')
            .filter(|line| !line.is_empty())
        {
            if line.len().saturating_add(1) > MAX_HISTORY_RECORD_BYTES {
                continue;
            }
            let Ok(record) = serde_json::from_slice::<CommandHistoryRecord>(line) else {
                continue;
            };
            if sanitized_command(&record.command).is_none() {
                continue;
            }
            records.push(record);
            if records.len() > PICKER_MAX_ENTRIES {
                break;
            }
        }
        let older_not_loaded = tail_truncated || records.len() > PICKER_MAX_ENTRIES;
        records.truncate(PICKER_MAX_ENTRIES);
        let mut state = Self::new(records);
        state.older_not_loaded = older_not_loaded;
        state
    }

    /// 当前过滤结果（最多 [`MAX_RESULTS`] 条）。空查询保持最新在前；否则按
    /// 模糊匹配分数降序，同分保持新旧顺序（稳定排序），命令与 cwd 一起参与
    /// 匹配，便于按项目目录召回。
    pub fn filtered(&self) -> Vec<&CommandHistoryRecord> {
        self.results
            .iter()
            .map(|&index| self.entries[index].as_ref())
            .collect()
    }

    /// Immutable click payloads share the loaded snapshot. Rebuilding the iced
    /// view must not clone up to fifteen 256 KiB commands on every keypress.
    /// A queued click still owns exactly the original row even if the query or
    /// picker changes before dispatch; only acceptance copies the raw command.
    pub fn shared_filtered(&self) -> Vec<Arc<CommandHistoryRecord>> {
        self.results
            .iter()
            .map(|&index| Arc::clone(&self.entries[index]))
            .collect()
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn current_directory(&self) -> Option<&str> {
        self.current_directory.as_deref()
    }

    /// Snapshot the invoking pane's directory. Exact string equality avoids
    /// treating sibling directories, prefixes or absent cwd metadata as local.
    pub fn set_current_directory(&mut self, cwd: Option<String>) {
        let cwd = cwd
            .filter(|cwd| std::path::Path::new(cwd).is_absolute() && sanitized_cwd(cwd).is_some());
        if self.current_directory == cwd {
            return;
        }
        let was_directory_only = self.directory_only;
        self.current_directory = cwd;
        if self.current_directory.is_none() {
            self.directory_only = false;
        }
        if was_directory_only {
            self.rebuild_results();
        }
    }

    pub fn directory_only(&self) -> bool {
        self.directory_only
    }

    pub fn status(&self) -> HistoryStatus {
        self.status
    }

    pub fn unique(&self) -> bool {
        self.unique
    }

    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    pub fn match_count(&self) -> usize {
        self.match_count
    }

    pub fn older_not_loaded(&self) -> bool {
        self.older_not_loaded
    }

    pub fn apply_filter(&mut self, action: HistoryFilterAction) {
        match action {
            HistoryFilterAction::ToggleDirectory => {
                if self.current_directory.is_none() {
                    return;
                }
                self.directory_only = !self.directory_only;
            }
            HistoryFilterAction::SetStatus(status) => {
                if self.status == status {
                    return;
                }
                self.status = status;
            }
            HistoryFilterAction::ToggleUnique => self.unique = !self.unique,
            HistoryFilterAction::Reset => {
                self.directory_only = false;
                self.status = HistoryStatus::All;
                self.unique = false;
                self.query.clear();
            }
        }
        self.rebuild_results();
    }

    fn rebuild_results(&mut self) {
        #[cfg(test)]
        {
            self.rebuild_count += 1;
        }
        self.selected = 0;
        let mut seen = HashSet::new();
        let mut scored = Vec::new();
        for (index, record) in self.entries.iter().enumerate() {
            if !self.status.matches(record.exit_code)
                || (self.directory_only
                    && record.cwd.as_deref() != self.current_directory.as_deref())
            {
                continue;
            }
            // Keep the newest matching execution per command+cwd. Apply the
            // status filter first so a later success cannot hide a failure.
            if self.unique && !seen.insert((record.command.as_str(), record.cwd.as_deref())) {
                continue;
            }
            let score = if self.query.is_empty() {
                0
            } else {
                let haystack = match record.cwd.as_deref() {
                    Some(cwd) => Cow::Owned(format!("{} {cwd}", record.command)),
                    None => Cow::Borrowed(record.command.as_str()),
                };
                let matcher = if use_linear_history_match(&haystack, &self.query) {
                    &self.linear_matcher
                } else {
                    &self.matcher
                };
                let Some(score) = matcher.fuzzy_match(&haystack, &self.query) else {
                    continue;
                };
                score
            };
            scored.push((score, index));
        }
        // Stable sort keeps newest-first ordering for equal fuzzy scores.
        scored.sort_by_key(|entry| std::cmp::Reverse(entry.0));
        self.match_count = scored.len();
        self.results = scored
            .into_iter()
            .take(MAX_RESULTS)
            .map(|(_, index)| index)
            .collect();
    }

    /// 高亮项下移（在过滤结果中循环）。
    pub fn select_next(&mut self) {
        let len = self.results.len();
        if len == 0 {
            self.selected = 0;
        } else {
            self.selected = (self.selected + 1) % len;
        }
    }

    /// 高亮项上移（在过滤结果中循环）。
    pub fn select_prev(&mut self) {
        let len = self.results.len();
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
        self.results
            .get(self.selected)
            .and_then(|&index| sanitized_command(&self.entries[index].command))
            .map(str::to_string)
    }

    /// Replace the query; the highlight returns to the first row. Control
    /// characters are dropped and the byte budget is enforced on a char
    /// boundary so iced `text_input` and the raw-key path share one contract.
    pub fn set_query(&mut self, query: impl Into<String>) {
        let query = bound_history_query(query);
        if history_query_is_unsafe(&query) {
            return;
        }
        if self.query != query {
            self.query = query;
            self.rebuild_results();
        }
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
        self.rebuild_results();
        true
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
        assert_eq!(sanitized_command("printf safe\u{202e}txt"), None);
        assert_eq!(sanitized_command("printf ok\u{fffd}"), None);
        assert_eq!(sanitized_command("echo\u{00a0}not-a-separator"), None);
        assert_eq!(
            sanitized_command(&"x".repeat(MAX_SHARED_HISTORY_COMMAND_BYTES + 1)),
            None
        );
    }

    /// The JSONL index is a family-shared file: anything the core writer
    /// accepts has to survive frost's read side, or frost silently hides
    /// records that are present and well-formed on disk (and a record frost
    /// writes has to be readable by the siblings for the same reason).
    #[test]
    fn every_record_the_core_writer_accepts_survives_the_picker() {
        // The writer's own bounds: command <= MAX_REVIEW_INPUT_BYTES, cwd <=
        // 16 KiB.
        assert_eq!(
            MAX_SHARED_HISTORY_COMMAND_BYTES,
            jterm_core::review_input::MAX_REVIEW_INPUT_BYTES
        );
        let command = "e".repeat(jterm_core::review_input::MAX_REVIEW_INPUT_BYTES);
        let cwd = format!("/{}", "d".repeat(16 * 1024 - 1));
        assert_eq!(sanitized_command(&command), Some(command.as_str()));
        assert_eq!(sanitized_cwd(&cwd), Some(cwd.as_str()));
        let state = HistoryPickerState::new(vec![record(&command, Some(&cwd), 0)]);
        assert_eq!(state.entries.len(), 1);
        assert_eq!(state.entries[0].cwd.as_deref(), Some(cwd.as_str()));
    }

    #[test]
    fn display_command_truncates_only_over_long_lines() {
        assert_eq!(display_command("cargo test"), "cargo test");
        let long = "x".repeat(500);
        let shown = display_command(&long);
        assert_eq!(shown.chars().count(), 120);
        assert!(shown.ends_with('…'));
        assert_eq!(display_command("safe\u{202e}hidden"), "safe\\u{202E}hidden");
    }

    #[test]
    fn display_cwd_escapes_spoofing_and_truncates() {
        assert_eq!(display_cwd("/tmp/frost"), "/tmp/frost");
        assert_eq!(display_cwd("/tmp/\u{202e}spoof"), "/tmp/\\u{202E}spoof");
        let long = format!("/{}", "x".repeat(400));
        let shown = display_cwd(&long);
        assert_eq!(shown.chars().count(), 80);
        assert!(shown.ends_with('…'));
        assert!(!shown.contains('\u{202e}'));
    }

    #[test]
    fn constructor_drops_unsafe_commands_and_untrusted_cwds() {
        let state = HistoryPickerState::new(vec![
            record("echo safe\u{2066}hidden", Some("/tmp"), 0),
            record("cargo test", Some("/tmp/\u{202e}spoof"), 0),
        ]);
        assert_eq!(state.entries.len(), 1);
        assert_eq!(state.entries[0].command, "cargo test");
        assert_eq!(state.entries[0].cwd, None);
        assert_eq!(state.selected_command().as_deref(), Some("cargo test"));
        assert_eq!(sanitized_cwd("/tmp/\u{fffd}spoof"), None);
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
        let mut state = HistoryPickerState::new(vec![
            record("cargo test", None, 0),
            record("git status", None, 0),
            record("cargo test", None, 1),
        ]);
        state.set_query("cargo");
        let filtered = state.filtered();
        assert_eq!(filtered.len(), 2);
        // Identical haystacks score identically; the stable sort keeps the
        // newer record first.
        assert_eq!(filtered[0].exit_code, 0);
        assert_eq!(filtered[1].exit_code, 1);
    }

    #[test]
    fn clicked_history_record_requires_current_open_snapshot_and_is_one_shot() {
        let state = HistoryPickerState::new(vec![record("cargo test", None, 0)]);
        let clicked = state.shared_filtered().remove(0);
        let keyboard = state.selected_command();
        let mut picker = Some(state);
        assert_eq!(accept_clicked_record(&mut picker, &clicked), keyboard);
        assert!(picker.is_none());
        assert!(accept_clicked_record(&mut picker, &clicked).is_none());
        picker = Some(HistoryPickerState::new(vec![record("cargo test", None, 0)]));
        assert!(accept_clicked_record(&mut picker, &clicked).is_none());
        assert!(picker.is_some());
        let reopened = picker.as_ref().unwrap().shared_filtered().remove(0);
        assert_eq!(accept_clicked_record(&mut picker, &reopened).as_deref(), Some("cargo test"));
    }

    #[test]
    fn clicked_history_record_survives_query_edit_but_not_explicit_close() {
        let mut picker = Some(HistoryPickerState::new(vec![record("cargo test", None, 0)]));
        let clicked = picker.as_ref().unwrap().shared_filtered().remove(0);
        picker.as_mut().unwrap().set_query("not matching");
        assert!(picker.as_ref().unwrap().filtered().is_empty());
        assert_eq!(accept_clicked_record(&mut picker, &clicked).as_deref(), Some("cargo test"));
        let state = HistoryPickerState::new(vec![record("git status", None, 0)]);
        let closed = state.shared_filtered().remove(0);
        drop(state);
        assert!(accept_clicked_record(&mut picker, &closed).is_none());
    }

    #[test]
    fn history_callback_closes_snapshot_before_existing_active_prompt_recall() {
        let source = include_str!("main.rs");
        let handler = source.split_once("            Message::HistoryPickerAccept(record) => {")
            .unwrap().1.split_once("            Message::WorkflowPickerInput(").unwrap().0;
        assert!(handler.find("accept_clicked_record(").unwrap()
            < handler.find("self.recall_into_active_pane(command)").unwrap());
        assert!(!handler.contains("record.command"));
        // Cross-session history targeting is deliberately unchanged: no
        // opening-session pin or new PTY/prompt implementation is introduced.
        assert!(handler.contains("None => Task::none()"));
    }

    #[test]
    fn shared_click_payload_preserves_full_snapshot_without_copying_command_bytes() {
        let command = format!(
            "printf {}",
            "x".repeat(MAX_SHARED_HISTORY_COMMAND_BYTES - 7)
        );
        let mut state = HistoryPickerState::new(vec![record(&command, Some("/work/frost"), 0)]);
        let first = state.shared_filtered().remove(0);
        let second = state.shared_filtered().remove(0);
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(first.command.as_ptr(), state.entries[0].command.as_ptr());
        assert_eq!(first.command, command);
        assert!(display_command(&first.command).len() < first.command.len());
        state.set_query("unmatched");
        assert!(state.shared_filtered().is_empty());
        drop(state);
        assert_eq!(
            first.command, command,
            "a queued click cannot retarget another row"
        );
        assert_eq!(first.cwd.as_deref(), Some("/work/frost"));
    }

    #[test]
    fn fuzzy_work_budget_preserves_ordinary_scoring() {
        assert!(!use_linear_history_match(
            &"a".repeat(256),
            &"a".repeat(256)
        ));
        assert!(use_linear_history_match(&"a".repeat(257), &"a".repeat(256)));
        let mut state = HistoryPickerState::new(vec![
            record("cargo test", Some("/work/frost"), 0),
            record("cat config", None, 0),
            record("git status", None, 0),
        ]);
        state.set_query("ct");
        let matcher = SkimMatcherV2::default();
        let mut reference: Vec<_> = state
            .entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| {
                let text = match entry.cwd.as_deref() {
                    Some(cwd) => format!("{} {cwd}", entry.command),
                    None => entry.command.clone(),
                };
                matcher.fuzzy_match(&text, "ct").map(|score| (score, index))
            })
            .collect();
        reference.sort_by_key(|entry| std::cmp::Reverse(entry.0));
        assert_eq!(
            state.results,
            reference
                .into_iter()
                .map(|(_, index)| index)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn linear_fallback_changes_ranking_not_match_membership() {
        let exact = SkimMatcherV2::default();
        let linear = SkimMatcherV2::default().element_limit(1);
        let choices = [
            "",
            "cargo test",
            "CARGO test",
            "a___b__c",
            "aaabaaaab",
            "目录/项目",
            "İstanbul Straße",
            "编译🙂终端",
            "foo/bar.rs",
            "foo BAR baz",
            " /é/É/ ",
        ];
        let patterns = [
            "",
            "ct",
            "CT",
            "ab",
            "aaaa",
            "目录",
            "目项",
            "🙂端",
            "İS",
            "st",
            "ß",
            "fb",
            "fB",
            " ",
            "éÉ",
            "unmatched",
        ];
        for choice in choices {
            for pattern in patterns {
                assert_eq!(
                    exact.fuzzy_match(choice, pattern).is_some(),
                    linear.fuzzy_match(choice, pattern).is_some(),
                    "choice {choice:?}, pattern {pattern:?}",
                );
            }
        }
    }

    #[test]
    fn long_history_queries_search_full_commands_and_cwd_without_truncation() {
        let command = format!(
            "{} target",
            "a".repeat(MAX_SHARED_HISTORY_COMMAND_BYTES - 7)
        );
        let cwd = format!("/{}目录", "d".repeat(MAX_HISTORY_CWD_BYTES - 7));
        let mut state = HistoryPickerState::new(vec![
            record(&command, Some(&cwd), 0),
            record(&command, Some(&cwd), 1),
            record("unrelated", None, 0),
        ]);
        state.set_query(format!("{} target", "a".repeat(512)));
        assert_eq!(state.match_count(), 2);
        assert_eq!(
            state.filtered()[0].exit_code,
            0,
            "ties retain newest-first order"
        );
        assert_eq!(state.selected_command().as_deref(), Some(command.as_str()));
        state.set_query("target 目录");
        assert_eq!(
            state.match_count(),
            2,
            "a match can cross from command to cwd"
        );
        state.apply_filter(HistoryFilterAction::SetStatus(HistoryStatus::Failed));
        assert_eq!(state.match_count(), 1);
        assert_eq!(state.filtered()[0].exit_code, 1);
        state.set_query("TARGET");
        assert_eq!(
            state.match_count(),
            0,
            "smart-case semantics survive fallback"
        );
    }

    #[test]
    fn cwd_participates_in_matching() {
        let mut state = HistoryPickerState::new(vec![
            record("make -j8", Some("/home/u/myproj"), 0),
            record("make -j8", Some("/home/u/other"), 0),
        ]);
        state.set_query("myproj");
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
    fn directory_and_status_filters_compose_before_the_result_cap() {
        let mut entries: Vec<_> = (0..MAX_RESULTS + 5)
            .map(|i| record(&format!("cargo unrelated-{i}"), Some("/work/other"), 1))
            .collect();
        entries.extend([
            record("cargo test", Some("/work/项目"), 1),
            record("cargo check", Some("/work/项目"), 0),
            record("cargo prefix", Some("/work/项目-old"), 1),
            record("cargo nested", Some("/work/项目/src"), 1),
            record("cargo unknown", None, 1),
        ]);
        let mut state = HistoryPickerState::new(entries);
        state.set_current_directory(Some("/work/项目".into()));
        state.set_query("cargo");
        state.select_next();
        state.apply_filter(HistoryFilterAction::ToggleDirectory);
        assert_eq!(state.selected, 0);
        assert_eq!(state.match_count(), 2);
        state.apply_filter(HistoryFilterAction::SetStatus(HistoryStatus::Failed));
        assert_eq!(state.match_count(), 1);
        assert_eq!(state.selected_command().as_deref(), Some("cargo test"));
        state.apply_filter(HistoryFilterAction::SetStatus(HistoryStatus::Success));
        assert_eq!(state.selected_command().as_deref(), Some("cargo check"));
        state.apply_filter(HistoryFilterAction::Reset);
        assert_eq!(state.query(), "");
        assert!(!state.directory_only());
        assert_eq!(state.match_count(), MAX_RESULTS + 10);
        assert_eq!(state.filtered().len(), MAX_RESULTS);
    }

    #[test]
    fn unique_keeps_the_newest_matching_execution_per_command_and_directory() {
        let mut newest_failure = record("cargo test", Some("/work/a"), -9);
        newest_failure.end_time_ms = Some(42);
        let mut state = HistoryPickerState::new(vec![
            record("cargo test", Some("/work/a"), 0),
            newest_failure,
            record("cargo test", Some("/work/a"), 1),
            record("cargo test", Some("/work/b"), 1),
            record("cargo test", None, 1),
            record("cargo test", None, 1),
        ]);
        state.apply_filter(HistoryFilterAction::ToggleUnique);
        assert_eq!(state.match_count(), 3);
        assert_eq!(state.filtered()[0].exit_code, 0);
        state.set_query("cargo");
        state.apply_filter(HistoryFilterAction::SetStatus(HistoryStatus::Failed));
        assert_eq!(state.match_count(), 3);
        let filtered = state.filtered();
        let local = filtered
            .iter()
            .find(|record| record.cwd.as_deref() == Some("/work/a"))
            .unwrap();
        assert_eq!(local.exit_code, -9);
        assert_eq!(local.end_time_ms, Some(42));
        assert_eq!(
            state.entries.len(),
            6,
            "folding never changes persisted records"
        );
        state.apply_filter(HistoryFilterAction::ToggleUnique);
        assert_eq!(state.match_count(), 5);
    }

    #[test]
    fn missing_or_unsafe_directory_cannot_enable_a_directory_filter() {
        let mut state = HistoryPickerState::new(vec![
            record("local", Some("/work/a"), 0),
            record("other", Some("/work/b"), 1),
        ]);
        for cwd in [
            None,
            Some("relative"),
            Some("/work/\u{202e}spoof"),
            Some("/work/\u{fffd}"),
            Some("/work/\n"),
        ] {
            state.set_current_directory(cwd.map(str::to_string));
            state.apply_filter(HistoryFilterAction::ToggleDirectory);
            assert!(!state.directory_only());
            assert_eq!(state.match_count(), 2);
        }
        state.set_current_directory(Some("/work/a".into()));
        state.apply_filter(HistoryFilterAction::ToggleDirectory);
        assert_eq!(state.selected_command().as_deref(), Some("local"));
        state.set_current_directory(Some("/work/b".into()));
        assert_eq!(state.selected_command().as_deref(), Some("other"));
        state.set_current_directory(None);
        assert!(!state.directory_only());
        assert_eq!(state.match_count(), 2);
    }

    #[test]
    fn cached_matches_are_reused_for_render_navigation_and_rejected_queries() {
        let mut state = HistoryPickerState::new(vec![
            record("cargo test", Some("/work/a"), 0),
            record("cargo check", Some("/work/a"), 1),
        ]);
        state.set_query("cargo");
        let rebuilds = state.rebuild_count;
        for _ in 0..20 {
            assert_eq!(state.filtered().len(), 2);
            state.select_next();
            state.select_prev();
            assert_eq!(state.selected_command().as_deref(), Some("cargo test"));
        }
        state.set_query("cargo");
        state.set_query("unsafe\u{202e}query");
        assert_eq!(state.rebuild_count, rebuilds);
        assert_eq!(state.query(), "cargo");
        assert!(state.backspace());
        assert_eq!(state.rebuild_count, rebuilds + 1);
        state.apply_filter(HistoryFilterAction::SetStatus(HistoryStatus::Failed));
        assert_eq!(state.rebuild_count, rebuilds + 2);
        assert_eq!(state.selected_command().as_deref(), Some("cargo check"));
        state.set_query("does not match");
        assert_eq!(state.match_count(), 0);
        state.select_prev();
        state.select_next();
        assert_eq!(state.selected_command(), None);
        assert_eq!(state.selected, 0);
    }

    #[test]
    fn load_reads_a_bounded_newest_first_slice() {
        let root =
            std::env::temp_dir().join(format!("frost-history-picker-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).expect("create private history fixture directory");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
                .expect("make history fixture directory private");
        }
        let path = root.join("history.jsonl");
        let mut contents = String::new();
        for i in 0..PICKER_MAX_ENTRIES + 10 {
            contents.push_str(&format!("{{\"command\":\"cmd-{i}\",\"exit_code\":0}}\n"));
        }
        write_private(&path, contents);

        let state = HistoryPickerState::load(&path);
        std::fs::remove_dir_all(&root).expect("remove history fixture");

        assert_eq!(state.entries.len(), PICKER_MAX_ENTRIES);
        assert!(state.older_not_loaded());
        assert_eq!(
            state.entries.first().map(|r| r.command.as_str()),
            Some(format!("cmd-{}", PICKER_MAX_ENTRIES + 9).as_str())
        );
    }

    #[test]
    fn disk_snapshot_preserves_duplicate_executions_for_status_and_directory_filters() {
        let root =
            std::env::temp_dir().join(format!("frost-history-duplicates-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let path = root.join("history.jsonl");
        let records = [
            record("cargo test", Some("/work/a"), 1),
            record("cargo test", Some("/work/b"), 1),
            record("cargo test", Some("/work/a"), 0),
        ];
        let mut contents = String::from("corrupt record\n");
        for record in records {
            contents.push_str(&serde_json::to_string(&record).unwrap());
            contents.push('\n');
        }
        contents.push_str("{\"command\":\"incomplete append\",\"exit_code\":0}");
        write_private(&path, contents);
        let mut state = HistoryPickerState::load(&path);
        assert_eq!(state.entry_count(), 3);
        assert!(!state.older_not_loaded());
        state.apply_filter(HistoryFilterAction::ToggleUnique);
        assert_eq!(state.match_count(), 2);
        state.set_current_directory(Some("/work/a".into()));
        state.apply_filter(HistoryFilterAction::ToggleDirectory);
        state.apply_filter(HistoryFilterAction::SetStatus(HistoryStatus::Failed));
        assert_eq!(state.match_count(), 1);
        assert_eq!(
            state.filtered()[0].exit_code,
            1,
            "a later success must not erase this failure"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn tail_reader_omits_fragments_and_reports_a_truncated_history() {
        let root =
            std::env::temp_dir().join(format!("frost-history-tail-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("history.jsonl");
        write_private(&path, b"first\nsecond\nthird\nincomplete");
        let (tail, truncated) = crate::persistence::read_jsonl_tail(&path, 16).unwrap();
        assert_eq!(tail, b"third\n");
        assert!(truncated);
        let (tail, truncated) = crate::persistence::read_jsonl_tail(&path, 23).unwrap();
        assert_eq!(tail, b"second\nthird\n");
        assert!(
            truncated,
            "a newline just before the window keeps the first whole record"
        );
        let (tail, truncated) = crate::persistence::read_jsonl_tail(&path, 100).unwrap();
        assert_eq!(tail, b"first\nsecond\nthird\n");
        assert!(!truncated);
        assert!(crate::persistence::read_jsonl_tail(&path, 0)
            .unwrap()
            .0
            .is_empty());

        write_private(
            &path,
            format!(
                "{}\n{{\"command\":\"latest\",\"exit_code\":1}}\n",
                "x".repeat(PICKER_TAIL_BYTES as usize)
            ),
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let state = HistoryPickerState::load(&path);
        assert_eq!(state.entry_count(), 1);
        assert_eq!(state.selected_command().as_deref(), Some("latest"));
        assert!(state.older_not_loaded());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn load_of_a_missing_file_yields_an_empty_picker() {
        let root = std::env::temp_dir().join(format!(
            "frost-history-picker-missing-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir(&root).expect("create private history fixture directory");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
                .expect("make history fixture directory private");
        }
        let path = root.join("history.jsonl");
        let state = HistoryPickerState::load(&path);
        std::fs::remove_dir_all(&root).expect("remove history fixture");
        assert!(state.filtered().is_empty());
        assert_eq!(state.selected_command(), None);
    }

    #[test]
    fn query_drops_controls_and_truncates_on_a_char_boundary() {
        let mut state = HistoryPickerState::new(vec![record("cargo test", None, 0)]);
        state.selected = 3;
        assert!(state.push_query_text("ca\nrg\u{1b}o"));
        assert_eq!(state.query, "cargo");
        assert_eq!(state.selected, 0);
        assert!(!state.push_query_text("\n"));
        assert_eq!(state.query, "cargo");

        state.set_query(format!("{}x", "界".repeat(MAX_HISTORY_QUERY_BYTES)));
        assert!(state.query.len() <= MAX_HISTORY_QUERY_BYTES);
        assert!(state.query.is_char_boundary(state.query.len()));
        assert!(!state.query.contains('x'));
        assert_eq!(state.query.chars().next(), Some('界'));

        state.set_query(format!("{}y", "x".repeat(MAX_HISTORY_QUERY_BYTES)));
        assert_eq!(state.query.len(), MAX_HISTORY_QUERY_BYTES);
        assert!(!state.query.contains('y'));
        let filled = state.query.clone();
        assert!(!state.push_query_text("z"));
        assert_eq!(state.query, filled);

        assert!(state.backspace());
        assert_eq!(state.query.len(), filled.len() - 1);
        state.set_query("cargo");
        state.set_query("cargo\u{202e}test");
        assert_eq!(state.query, "cargo");
        assert!(!state.query.contains('\u{202e}'));
        assert!(!state.query.contains('\u{fffd}'));
        assert!(!state.filtered().is_empty());
        state.set_query("cargo\u{fffd}test");
        assert_eq!(state.query, "cargo");
        assert_eq!(state.selected_command(), Some("cargo test".into()));
    }
}
