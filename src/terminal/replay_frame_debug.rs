use std::fmt::Debug;
use std::sync::LazyLock;

use jterm_engine::ScrollbackLine;
use regex::Regex;
use serde_json::Value;

struct FrozenScrollbackRow<'a>(&'a ScrollbackLine);

impl Debug for FrozenScrollbackRow<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let row = self.0;
        f.debug_struct("ScrollbackLine")
            .field("data", &row.content())
            .field("is_wrapped", &row.is_wrapped)
            .field("cols", &row.columns())
            .field("raw_row_id", &row.raw_row_id())
            .field("projected_active_len", &row.reflow_content_len())
            .field(
                "projected_wide_continuations",
                &row.reflow_wide_continuations(),
            )
            .finish()
    }
}

/// Keep the original row names and compressed bytes in the frozen observers.
pub(super) fn scrollback_lane<'a>(rows: impl IntoIterator<Item = &'a ScrollbackLine>) -> Value {
    let rows: Vec<_> = rows.into_iter().map(FrozenScrollbackRow).collect();
    Value::String(format!("{rows:?}"))
}

/// Preserve the original frame-stamp representation in sealed replay lanes.
/// The engine now wraps admitted IDs internally. Keep their exact values and
/// every other field in the snapshot that the original observer recorded.
pub(super) fn frame_lane(value: impl Debug) -> Value {
    static STAMP: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(concat!(
            r"CellFrame \{ stamp: AdmittedStamp \{ epoch: PublicationEpoch\(([0-9]+)\), ",
            r"revision: FrameRevision\(([0-9]+)\), layout: LayoutRevision\(([0-9]+)\), ",
            r"screen: (Primary|Alternate) \}",
        ))
        .expect("fixed admitted frame stamp pattern")
    });
    let rendered = format!("{value:?}");
    if !rendered.contains("stamp: AdmittedStamp") {
        return Value::String(rendered);
    }
    Value::String(
        STAMP
            .replace_all(
                &rendered,
                "CellFrame { stamp: Stamp { epoch: $1, revision: $2, layout: $3, screen: $4 }",
            )
            .into_owned(),
    )
}
