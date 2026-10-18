//! Fixed-size, privacy-safe startup repair accounting. No persisted user text.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum RepairKind {
    ExtraSessions,
    ExtraTabs,
    InvalidCwd,
    InvalidTitle,
    InvalidTab,
    InvalidTabsContainer,
    LegacyTree,
    LegacySplit,
    ActiveSession,
    ActiveTab,
    LayoutDepth,
    LayoutNodes,
    LayoutPanes,
    LayoutAdjusted,
    ReferencesAdjusted,
}

const KINDS: [RepairKind; 15] = [
    RepairKind::ExtraSessions,
    RepairKind::ExtraTabs,
    RepairKind::InvalidCwd,
    RepairKind::InvalidTitle,
    RepairKind::InvalidTab,
    RepairKind::InvalidTabsContainer,
    RepairKind::LegacyTree,
    RepairKind::LegacySplit,
    RepairKind::ActiveSession,
    RepairKind::ActiveTab,
    RepairKind::LayoutDepth,
    RepairKind::LayoutNodes,
    RepairKind::LayoutPanes,
    RepairKind::LayoutAdjusted,
    RepairKind::ReferencesAdjusted,
];

impl RepairKind {
    fn label(self) -> &'static str {
        match self {
            Self::ExtraSessions => "extra sessions skipped (restored only the first 32)",
            Self::ExtraTabs => "extra tab layouts skipped",
            Self::InvalidCwd => "invalid working directories removed",
            Self::InvalidTitle => "invalid tab titles removed",
            Self::InvalidTab => "invalid tab layouts discarded",
            Self::InvalidTabsContainer => "invalid tab layouts container discarded",
            Self::LegacyTree => "invalid legacy pane layout discarded",
            Self::LegacySplit => "invalid legacy split layout discarded",
            Self::ActiveSession => "active session index repaired",
            Self::ActiveTab => "active tab index repaired",
            Self::LayoutDepth => "layouts discarded at the depth limit",
            Self::LayoutNodes => "layouts discarded at the node limit",
            Self::LayoutPanes => "layouts discarded at the pane limit",
            Self::LayoutAdjusted => "layout ratios or focus were adjusted",
            Self::ReferencesAdjusted => "invalid or duplicate pane references were adjusted",
        }
    }
}

/// Counts actual repairs on retained objects and whole-object discards.
/// A discarded object counts once; uninspected defects inside it do not count.
/// Resource categories record the first observed rejection, not inferred depth.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RecoveryReport {
    counts: [usize; KINDS.len()],
}

impl RecoveryReport {
    pub fn add(&mut self, kind: RepairKind, count: usize) {
        self.counts[kind as usize] = if matches!(
            kind,
            RepairKind::LayoutAdjusted | RepairKind::ReferencesAdjusted
        ) {
            usize::from(count > 0 || self.counts[kind as usize] > 0)
        } else {
            self.counts[kind as usize].saturating_add(count)
        };
    }

    pub fn count(&self, kind: RepairKind) -> usize {
        self.counts[kind as usize]
    }

    pub fn merge(&mut self, other: Self) {
        for kind in KINDS {
            self.add(kind, other.count(kind));
        }
    }

    pub fn is_empty(&self) -> bool {
        self.counts.iter().all(|count| *count == 0)
    }

    pub fn messages(&self) -> Vec<String> {
        KINDS
            .into_iter()
            .filter_map(|kind| {
                let count = self.count(kind);
                (count > 0).then(|| {
                    if matches!(
                        kind,
                        RepairKind::LayoutAdjusted | RepairKind::ReferencesAdjusted
                    ) {
                        kind.label().to_string()
                    } else {
                        format!("{count} {}", kind.label())
                    }
                })
            })
            .collect()
    }

    /// Static ASCII labels and bounded integers only; no paths or input text.
    pub fn summary(&self) -> String {
        self.messages().join("; ")
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PreservationOutcome {
    #[default]
    Unchanged,
    Preserved,
    Blocked,
}

impl PreservationOutcome {
    pub fn writes_blocked(self) -> bool {
        self == Self::Blocked
    }

    pub fn notice(self) -> Option<&'static str> {
        match self {
            Self::Unchanged => None,
            Self::Preserved => Some("The original saved session was preserved before repairs are saved."),
            Self::Blocked => Some("The original saved session could not be preserved. Automatic session saving is paused for this run; terminals remain usable."),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_report_is_silent_and_merge_counts_exactly() {
        let mut report = RecoveryReport::default();
        assert!(report.is_empty());
        assert_eq!(report.summary(), "");
        report.add(RepairKind::InvalidTitle, 2);
        let mut second = RecoveryReport::default();
        second.add(RepairKind::ExtraSessions, 4);
        report.merge(second);
        assert_eq!(report.count(RepairKind::InvalidTitle), 2);
        assert_eq!(report.count(RepairKind::ExtraSessions), 4);
    }

    #[test]
    fn maximum_report_is_fixed_size_bounded_and_path_free() {
        let mut report = RecoveryReport::default();
        for kind in KINDS {
            report.add(kind, usize::MAX);
            report.add(kind, 1);
        }
        assert_eq!(
            std::mem::size_of_val(&report),
            15 * std::mem::size_of::<usize>()
        );
        assert!(report.summary().len() < 1024);
        assert!(report.summary().is_ascii());
        assert!(!report.summary().contains('/'));
        for kind in KINDS {
            assert_eq!(
                report.count(kind),
                if matches!(
                    kind,
                    RepairKind::LayoutAdjusted | RepairKind::ReferencesAdjusted
                ) {
                    1
                } else {
                    usize::MAX
                }
            );
        }
    }

    #[test]
    fn preservation_outcome_is_explicit_and_only_failure_blocks() {
        assert!(!PreservationOutcome::Unchanged.writes_blocked());
        assert!(!PreservationOutcome::Preserved.writes_blocked());
        assert!(PreservationOutcome::Blocked.writes_blocked());
        assert!(PreservationOutcome::Unchanged.notice().is_none());
        assert!(PreservationOutcome::Preserved
            .notice()
            .unwrap()
            .contains("preserved"));
        assert!(PreservationOutcome::Blocked
            .notice()
            .unwrap()
            .contains("paused"));
    }
}
