//! Workflow picker overlay state: the iced mirror of anvil's palette workflow
//! tier (`Ctrl+Shift+M`, or the "Workflows" palette action) plus its
//! parameter dialog.
//!
//! The picker fuzzy-searches the loaded workflow list; accepting a workflow
//! without arguments renders and inserts it at the active prompt for review,
//! while one with arguments opens the per-argument form (defaults prefilled,
//! exactly like anvil's dialog). Nothing ever executes: the rendered command
//! goes through the same `PromptRecall` review boundary as history recall.
//!
//! Loading is synchronous at open, bounded by `workflows`' file and count
//! caps — the same seam as `history_picker::HistoryPickerState::load`. This
//! replaces anvil's single-flight background refresh (`workflow_ops.rs`):
//! iced has no palette tier to prewarm, so the GTK worker/latch machinery
//! would buy nothing here.

use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;

use crate::workflows::{self, Workflow};

/// 一次渲染/导航的最大结果数。键盘选择与绘制共用 `filtered()`，因此上限
/// 同时约束两者——更多的 workflow 通过输入查询来召回（与历史选择器一致）。
pub(crate) const MAX_RESULTS: usize = 15;

/// 选择器状态。`entries` 保持 `workflows::load_all` 的目录优先级顺序（与
/// anvil 一致：更早的目录胜出同名项，目录内按文件名排序）；期间磁盘上的
/// 变更在下一次打开时生效。
pub(crate) struct WorkflowPickerState {
    pub query: String,
    /// 当前过滤结果中的高亮位置。
    pub selected: usize,
    entries: Vec<Workflow>,
    matcher: SkimMatcherV2,
}

impl WorkflowPickerState {
    pub(crate) fn new(entries: Vec<Workflow>) -> Self {
        Self {
            query: String::new(),
            selected: 0,
            entries,
            matcher: SkimMatcherV2::default(),
        }
    }

    /// 从全部搜索路径加载。缺失/损坏的文件被 `workflows` 跳过，这里得到的
    /// 只是更短的列表而不是错误。
    pub(crate) fn load() -> Self {
        Self::load_from(&workflows::workflow_dirs())
    }

    /// Test seam (and any future caller with an explicit search path).
    pub(crate) fn load_from(dirs: &[std::path::PathBuf]) -> Self {
        Self::new(workflows::load_all(dirs))
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 当前过滤结果（最多 [`MAX_RESULTS`] 条）。空查询保持加载顺序；否则按
    /// 模糊匹配分数降序，同分保持加载顺序（稳定排序）。名称、描述与标签一起
    /// 参与匹配，对应 anvil 面板对 tags 的检索。
    pub(crate) fn filtered(&self) -> Vec<&Workflow> {
        if self.query.is_empty() {
            return self.entries.iter().take(MAX_RESULTS).collect();
        }
        let mut scored: Vec<(i64, &Workflow)> = self
            .entries
            .iter()
            .filter_map(|workflow| {
                let haystack = format!(
                    "{} {} {}",
                    workflow.name,
                    workflow.description,
                    workflow.tags.join(" ")
                );
                self.matcher
                    .fuzzy_match(&haystack, &self.query)
                    .map(|score| (score, workflow))
            })
            .collect();
        scored.sort_by_key(|entry| std::cmp::Reverse(entry.0));
        scored
            .into_iter()
            .take(MAX_RESULTS)
            .map(|(_, workflow)| workflow)
            .collect()
    }

    /// 高亮项下移（在过滤结果中循环）。
    pub(crate) fn select_next(&mut self) {
        let len = self.filtered().len();
        if len == 0 {
            self.selected = 0;
        } else {
            self.selected = (self.selected + 1) % len;
        }
    }

    /// 高亮项上移（在过滤结果中循环）。
    pub(crate) fn select_prev(&mut self) {
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

    /// 当前高亮的 workflow（按过滤结果中的位置）。
    pub(crate) fn selected_workflow(&self) -> Option<&Workflow> {
        self.filtered().get(self.selected).copied()
    }

    /// 过滤结果中第 `index` 条的 workflow（用于鼠标点击分发）。
    pub(crate) fn workflow_at_filtered(&self, index: usize) -> Option<&Workflow> {
        self.filtered().get(index).copied()
    }
}

/// 参数填写表单状态，对应 anvil 的 workflow 对话框：每个声明的参数一行输入，
/// 以声明的默认值预填；用户清空的输入以显式空值覆盖默认值（anvil 的 Open
/// 消息同样用默认值或空串播种整个 values 表）。
pub(crate) struct WorkflowArgsState {
    pub workflow: Workflow,
    /// 与 `workflow.args` 对齐的当前值。
    pub values: Vec<String>,
    /// 最近一次渲染错误，内联显示在表单上（correction 卡片的 feedback 惯例）。
    pub feedback: Option<String>,
}

impl WorkflowArgsState {
    pub(crate) fn new(workflow: Workflow) -> Self {
        let values = workflow
            .args
            .iter()
            .map(|arg| arg.default.clone().unwrap_or_default())
            .collect();
        Self {
            workflow,
            values,
            feedback: None,
        }
    }

    pub(crate) fn set_value(&mut self, index: usize, value: String) {
        if let Some(slot) = self.values.get_mut(index) {
            *slot = value;
        }
    }

    /// Render the template with the current values. All declared arguments are
    /// supplied (prefilled or edited), so a missing-value error here means the
    /// template references a placeholder the file never declared — reported
    /// verbatim, like anvil's dialog error label.
    pub(crate) fn render(&self) -> Result<String, String> {
        let values: std::collections::HashMap<String, String> = self
            .workflow
            .args
            .iter()
            .zip(self.values.iter())
            .map(|(arg, value)| (arg.name.clone(), value.clone()))
            .collect();
        workflows::render(&self.workflow, &values)
    }
}

/// The workflows overlay is either the searchable list or one workflow's
/// argument form; Escape closes both. Both variants are boxed: the picker's
/// matcher and entry list are far larger than the form, and this enum sits in
/// the app state full-time.
pub(crate) enum WorkflowOverlay {
    Picker(Box<WorkflowPickerState>),
    Args(Box<WorkflowArgsState>),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflows::WorkflowArg;
    use std::path::PathBuf;

    fn workflow(name: &str, description: &str, tags: &[&str]) -> Workflow {
        Workflow {
            name: name.to_string(),
            description: description.to_string(),
            command: "echo ok".to_string(),
            tags: tags.iter().map(|tag| tag.to_string()).collect(),
            shell: None,
            args: Vec::new(),
            source_path: None,
        }
    }

    #[test]
    fn empty_query_keeps_load_order_and_caps_results() {
        let entries = (0..MAX_RESULTS + 5)
            .map(|i| workflow(&format!("wf-{i:02}"), "", &[]))
            .collect();
        let state = WorkflowPickerState::new(entries);
        let filtered = state.filtered();
        assert_eq!(filtered.len(), MAX_RESULTS);
        assert_eq!(filtered[0].name, "wf-00");
        assert_eq!(
            state.selected_workflow().map(|wf| wf.name.as_str()),
            Some("wf-00")
        );
    }

    #[test]
    fn fuzzy_query_matches_name_description_and_tags() {
        let state = WorkflowPickerState {
            query: "deploy".to_string(),
            ..WorkflowPickerState::new(vec![
                workflow("Deploy to staging", "", &[]),
                workflow("Ship it", "run the deploy playbook", &[]),
                workflow("Tunnel", "ssh forward", &["deploy", "net"]),
                workflow("Unrelated", "nothing", &[]),
            ])
        };
        let names: Vec<&str> = state.filtered().iter().map(|wf| wf.name.as_str()).collect();
        assert_eq!(
            names.len(),
            3,
            "tags and descriptions must match: {names:?}"
        );
        assert!(!names.contains(&"Unrelated"));
    }

    #[test]
    fn selection_wraps_and_click_index_resolves() {
        let mut state =
            WorkflowPickerState::new(vec![workflow("one", "", &[]), workflow("two", "", &[])]);
        state.select_prev();
        assert_eq!(state.selected, 1);
        assert_eq!(
            state.workflow_at_filtered(1).map(|wf| wf.name.as_str()),
            Some("two")
        );
        state.select_next();
        assert_eq!(state.selected, 0);
        assert!(state.workflow_at_filtered(2).is_none());
    }

    #[test]
    fn load_from_reads_the_given_search_path_only() {
        let dir = std::env::temp_dir().join(format!(
            "frost-workflow-picker-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.yaml"), "name: A\ncommand: echo a\n").unwrap();
        std::fs::write(dir.join("broken.yaml"), "name: [not\n").unwrap();

        let state = WorkflowPickerState::load_from(std::slice::from_ref(&dir));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(state.filtered().len(), 1);
        assert_eq!(state.selected_workflow().unwrap().name, "A");

        let missing = PathBuf::from("/nonexistent/frost/workflows/never");
        assert!(WorkflowPickerState::load_from(std::slice::from_ref(&missing)).is_empty());
    }

    #[test]
    fn args_form_prefills_defaults_and_renders_edits() {
        let workflow = Workflow {
            name: "Deploy".to_string(),
            description: String::new(),
            command: "deploy {service} --env={{env}}".to_string(),
            tags: Vec::new(),
            shell: None,
            args: vec![
                WorkflowArg {
                    name: "service".to_string(),
                    description: String::new(),
                    default: Some("api".to_string()),
                },
                WorkflowArg {
                    name: "env".to_string(),
                    description: String::new(),
                    default: None,
                },
            ],
            source_path: None,
        };
        let mut form = WorkflowArgsState::new(workflow);
        assert_eq!(form.values, ["api", ""]);
        // A declared argument without a default starts as an explicit empty
        // value, not a missing one — anvil's dialog seeds the values table the
        // same way, so "missing values" is unreachable from the form (the
        // missing-placeholder path is covered by workflows::render's tests).
        assert_eq!(form.render().unwrap(), "deploy api --env=");

        form.set_value(1, "staging".to_string());
        assert_eq!(form.render().unwrap(), "deploy api --env=staging");

        // An edited-away value stays an explicit empty string, matching
        // anvil's dialog (it does not fall back to the declared default).
        form.set_value(0, String::new());
        assert_eq!(form.render().unwrap(), "deploy  --env=staging");

        // Values cross the review-only boundary: a control character fails.
        form.set_value(0, "ok\nrm -rf /".to_string());
        assert!(form.render().unwrap_err().contains("unsafe"));
    }
}
