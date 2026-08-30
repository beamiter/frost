//! Workflow picker overlay state: the iced mirror of anvil's palette workflow
//! tier (`Ctrl+Shift+M`, or the "Workflows" palette action) plus its
//! parameter dialog.
//!
//! The picker fuzzy-searches the loaded workflow list; accepting a workflow
//! without arguments renders and inserts it at the active prompt for review,
//! while one with arguments opens the per-argument form. Nothing ever
//! executes: the rendered command goes through the same `PromptRecall` review
//! boundary as history recall.
//!
//! Loading is synchronous at open, bounded by the file and count caps in
//! [`jterm_core::workflows`] — the same seam as
//! `history_picker::HistoryPickerState::load`. This replaces anvil's
//! single-flight background refresh (`workflow_ops.rs`): iced has no palette
//! tier to prewarm, so the GTK worker/latch machinery would buy nothing here.
//!
//! Iced wiring stays here; both pure state machines do not. The searchable
//! list is [`jterm_core::workflows::WorkflowPicker`], and the form's value
//! bookkeeping is [`ArgsForm`], because
//! keeping "untouched and undefaulted" apart from "deliberately emptied" is
//! what makes the family-wide missing-value guard reachable at all (see
//! [`crate::workflows`]).

use crate::workflows::{self, ArgsForm, Workflow};
use jterm_core::workflows::{PickerPolicy, WorkflowPicker};

/// 一次渲染/导航的最大结果数。键盘选择与绘制共用 `filtered()`，因此上限
/// 同时约束两者——更多的 workflow 通过输入查询来召回（与历史选择器一致）。
pub(crate) const MAX_RESULTS: usize = 15;
const PICKER_POLICY: PickerPolicy = PickerPolicy::new(MAX_RESULTS, false);

/// 选择器状态。`entries` 保持 `workflows::load_all` 的目录优先级顺序（与
/// anvil 一致：更早的目录胜出同名项，目录内按文件名排序）；期间磁盘上的
/// 变更在下一次打开时生效。
pub(crate) struct WorkflowPickerState {
    picker: WorkflowPicker,
}

impl WorkflowPickerState {
    pub(crate) fn new(entries: Vec<Workflow>) -> Self {
        Self {
            picker: WorkflowPicker::new(entries, PICKER_POLICY),
        }
    }

    /// 从全部搜索路径加载。缺失/损坏的文件被 `jterm_core::workflows` 记录并
    /// 跳过，这里得到的只是更短的列表而不是错误。
    pub(crate) fn load() -> Self {
        Self::load_from(&workflows::workflow_dirs())
    }

    /// Test seam (and any future caller with an explicit search path). 顺序由
    /// `workflows::load_library_from` 钉死为目录优先级顺序——`filtered()` 的
    /// 空查询分支直接暴露它。
    pub(crate) fn load_from(dirs: &[std::path::PathBuf]) -> Self {
        Self::new(workflows::load_library_from(dirs))
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.picker.is_empty()
    }

    pub(crate) fn query(&self) -> &str {
        self.picker.query()
    }

    pub(crate) fn selected(&self) -> usize {
        self.picker.selected()
    }

    /// Iced `text_input` and accessibility/programmatic input both cross the
    /// core's one-line and byte-budget boundary here.
    pub(crate) fn set_query(&mut self, query: impl Into<String>) {
        self.picker.set_query(query);
    }

    pub(crate) fn push_query_text(&mut self, text: &str) -> bool {
        self.picker.push_query_text(text)
    }

    pub(crate) fn backspace(&mut self) -> bool {
        self.picker.backspace()
    }

    /// 当前过滤结果（最多 [`MAX_RESULTS`] 条）。空查询保持加载顺序；否则按
    /// 模糊匹配分数降序，同分保持加载顺序（稳定排序）。名称、描述与标签一起
    /// 参与匹配，对应 anvil 面板对 tags 的检索。
    pub(crate) fn filtered(&self) -> Vec<&Workflow> {
        self.picker.filtered()
    }

    /// 高亮项下移（在过滤结果中循环）。
    pub(crate) fn select_next(&mut self) {
        self.picker.select_next();
    }

    /// 高亮项上移（在过滤结果中循环）。
    pub(crate) fn select_prev(&mut self) {
        self.picker.select_prev();
    }

    /// 当前高亮的 workflow（按过滤结果中的位置）。
    pub(crate) fn selected_workflow(&self) -> Option<&Workflow> {
        self.picker.selected_workflow()
    }

    /// 过滤结果中第 `index` 条的 workflow（用于鼠标点击分发）。
    pub(crate) fn workflow_at_filtered(&self, index: usize) -> Option<&Workflow> {
        self.picker.workflow_at_filtered(index)
    }
}

/// 参数填写表单状态，对应 anvil 的 workflow 对话框：每个声明的参数一行输入，
/// 以声明的默认值预填。值的记账交给 [`ArgsForm`]——它在类型里保留了"未填写
/// 且文件未声明默认值"与"用户显式清空"的区别，而这正是四份拷贝共有的缺陷所
/// 在：过去每个 UI（含此处）都用 `""` 预填每一个声明的参数，于是
/// `workflows::render` 里那条实现过、也单测过的 missing-values 守卫在全家族
/// 的四个终端里都触发不了，`kill -9 {pid}` 会以 `kill -9 ` 送到提示符。
///
/// 现在的契约：空值只有在文件这么说时才成立（`default = ""` 就是这么说）。
/// 声明了默认值的参数被清空仍然是一次显式的空值；没有声明默认值的参数留空则
/// 是"未填写"，渲染时报 `missing values:`，[`Self::missing`] 让视图在按下
/// Insert 之前就把这些行标出来。
pub(crate) struct WorkflowArgsState {
    form: ArgsForm,
    /// 最近一次渲染错误，内联显示在表单上（correction 卡片的 feedback 惯例）。
    pub feedback: Option<String>,
}

impl WorkflowArgsState {
    pub(crate) fn new(workflow: Workflow) -> Self {
        Self {
            form: ArgsForm::new(workflow),
            feedback: None,
        }
    }

    /// 表单正在填写的 workflow（视图取名称、描述与命令模板）。
    pub(crate) fn workflow(&self) -> &Workflow {
        self.form.workflow()
    }

    /// 第 `index` 行输入框当前应显示的文本。未填写且未声明默认值的行是空串——
    /// 它显示成什么，就是它的含义。
    pub(crate) fn value(&self, index: usize) -> &str {
        self.form.value(index)
    }

    /// Still-outstanding argument names, computed by the shared form. The view
    /// snapshots this once before drawing its at-most-64 rows, so its required
    /// labels cannot drift from the renderer's rule.
    pub(crate) fn missing(&self) -> Vec<&str> {
        self.form.missing()
    }

    pub(crate) fn set_value(&mut self, index: usize, value: String) {
        self.form.set(index, value);
        self.feedback = None;
    }

    /// Return one row to the value declared by the workflow. For an argument
    /// without a default this restores the genuinely-unset state; assigning an
    /// empty string cannot express that distinction.
    pub(crate) fn reset_value(&mut self, index: usize) {
        self.form.clear(index);
        self.feedback = None;
    }

    /// 用当前值渲染模板。未填写的参数不会被当作空串提交，因此这里的
    /// `missing values:` 是真的缺值，逐字显示在表单的错误标签上。
    pub(crate) fn render(&self) -> Result<String, String> {
        self.form.render()
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
        let mut state = WorkflowPickerState::new(vec![
            workflow("Deploy to staging", "", &[]),
            workflow("Ship it", "run the deploy playbook", &[]),
            workflow("Tunnel", "ssh forward", &["deploy", "net"]),
            workflow("Unrelated", "nothing", &[]),
        ]);
        state.set_query("deploy");
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
        assert_eq!(state.selected(), 1);
        assert_eq!(
            state.workflow_at_filtered(1).map(|wf| wf.name.as_str()),
            Some("two")
        );
        state.select_next();
        assert_eq!(state.selected(), 0);
        assert!(state.workflow_at_filtered(2).is_none());
    }

    #[test]
    fn iced_query_input_crosses_the_shared_query_boundary() {
        let mut state = WorkflowPickerState::new(vec![workflow("alpha", "", &[])]);
        state.set_query(format!(
            "{}\nignored",
            "x".repeat(jterm_core::workflows::MAX_PICKER_QUERY_BYTES + 16)
        ));
        assert!(state.query().len() <= jterm_core::workflows::MAX_PICKER_QUERY_BYTES);
        assert!(!state.query().contains('\n'));
        assert_eq!(state.selected(), 0);
        assert_eq!(state.picker.policy(), PICKER_POLICY);
        assert!(!state.picker.policy().search_command());
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
    fn args_form_prefills_defaults_and_withholds_the_undeclared_ones() {
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
        assert_eq!(form.value(0), "api");
        assert_eq!(form.value(1), "");

        // 声明了默认值的行不缺；没有声明默认值又没填的行缺，视图据此标注，
        // 渲染据此拒绝。这里曾经断言 `deploy api --env=` 渲染成功——那条断言
        // 是四个终端共有的缺陷留下的化石，不是要保住的行为。
        assert!(!form.missing().contains(&"service"));
        assert!(form.missing().contains(&"env"));
        let error = form.render().unwrap_err();
        assert!(error.contains("missing values: env"), "got {error}");

        form.set_value(1, "staging".to_string());
        assert!(!form.missing().contains(&"env"));
        assert_eq!(form.render().unwrap(), "deploy api --env=staging");

        // Reset is not the same operation as typing an empty string: the first
        // row returns to its declared default and the second returns to unset.
        form.set_value(0, "worker".to_string());
        form.reset_value(0);
        assert_eq!(form.value(0), "api");
        form.reset_value(1);
        assert!(form.missing().contains(&"env"));
        assert!(form.render().unwrap_err().contains("missing values: env"));
        form.set_value(1, "staging".to_string());

        // 清空一个声明了默认值的参数仍然是显式的空值：文件说过空值在这里有
        // 意义，它就不会回退到默认值，也不算缺值。
        form.set_value(0, String::new());
        assert!(!form.missing().contains(&"service"));
        assert_eq!(form.render().unwrap(), "deploy  --env=staging");

        // 只输入空白等于没输入：没有声明默认值的参数不接受空白冒充。
        form.set_value(1, "   ".to_string());
        assert!(form.missing().contains(&"env"));
        assert!(form.render().unwrap_err().contains("missing values: env"));

        // 值同样要过 review-only 边界：控制字符直接失败。
        form.set_value(1, "staging".to_string());
        form.set_value(0, "ok\nrm -rf /".to_string());
        assert!(form.render().unwrap_err().contains("unsafe"));
    }
}
