use std::fmt::Debug;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

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
