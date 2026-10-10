//! Ephemeral Settings callbacks. No profile data, persistence, I/O or networking.
use std::sync::Arc;

#[derive(Clone, Debug)]
pub(crate) struct RowTarget {
    identity: Arc<()>,
    index: usize,
}

#[derive(Default)]
pub(crate) struct RemoteHostEditor {
    identity: Arc<()>,
}

impl RemoteHostEditor {
    pub fn identity(&self) -> Arc<()> {
        self.identity.clone()
    }

    pub fn target(&self, index: usize) -> RowTarget {
        RowTarget {
            identity: self.identity(),
            index,
        }
    }

    pub fn retire(&mut self) {
        self.identity = Arc::new(());
    }

    pub fn accepts(&self, identity: &Arc<()>, visible: bool) -> bool {
        visible && Arc::ptr_eq(&self.identity, identity)
    }

    pub fn resolve(&self, target: &RowTarget, visible: bool, rows: usize) -> Option<usize> {
        (self.accepts(&target.identity, visible) && target.index < rows).then_some(target.index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removal_rejects_double_delete_and_old_shifted_field_callbacks() {
        let mut editor = RemoteHostEditor::default();
        let mut rows = vec!["A", "B", "C"];
        let remove_a = editor.target(0);
        let edit_b = editor.target(1);
        let remove_b = editor.target(1);
        rows.remove(editor.resolve(&remove_a, true, rows.len()).unwrap());
        editor.retire();
        assert_eq!(editor.resolve(&remove_a, true, rows.len()), None);
        assert_eq!(editor.resolve(&edit_b, true, rows.len()), None);
        assert_eq!(editor.resolve(&remove_b, true, rows.len()), None);
        assert_eq!(rows, ["B", "C"]);
        assert_eq!(editor.resolve(&editor.target(0), true, rows.len()), Some(0));
    }

    #[test]
    fn ordinary_successive_typing_keeps_the_current_row_target() {
        let editor = RemoteHostEditor::default();
        let target = editor.target(0);
        let mut rows = [String::new()];
        for text in ["h", "ho", "host"] {
            let index = editor.resolve(&target, true, rows.len()).unwrap();
            rows[index] = text.to_string();
        }
        assert_eq!(rows[0], "host");
        assert_eq!(editor.resolve(&editor.target(1), true, rows.len()), None);
    }

    #[test]
    fn reset_reopen_blur_and_structural_add_retire_old_actions_even_for_same_rows() {
        let mut editor = RemoteHostEditor::default();
        for _boundary in [
            "reset",
            "close/reopen",
            "blur/refocus",
            "add",
            "replacement",
        ] {
            let row = editor.target(0);
            let add = editor.identity();
            assert_eq!(editor.resolve(&row, false, 1), None);
            assert!(!editor.accepts(&add, false));
            editor.retire();
            assert_eq!(editor.resolve(&row, true, 1), None);
            assert!(!editor.accepts(&add, true));
            assert!(editor.accepts(&editor.identity(), true));
        }
    }
}
