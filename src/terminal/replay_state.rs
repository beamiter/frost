use super::TerminalState;
use serde_json::Value;
use std::collections::BTreeMap;

/// Read shared engine state using the formats in Frost's frozen replay records.
pub(super) fn observe(
    terminal: &TerminalState,
    out: &mut BTreeMap<String, Value>,
    lane: impl Fn(String) -> Value,
) {
    let hyperlink = terminal.screen.current_hyperlink;
    let legacy_hyperlink = (!hyperlink.is_none()).then_some(hyperlink.as_raw());
    out.insert(
        "current_hyperlink".into(),
        lane(format!("{legacy_hyperlink:?}")),
    );

    let protocol = &terminal.protocol;
    let mut bits = 0u64;
    for (index, mode) in [
        7, 25, 1000, 1001, 1002, 1003, 1004, 1006, 1049, 2004, 2026, 2031, 5522, 1, 4, 6, 1005,
        1015, 66, 47, 1047, 1048,
    ]
    .into_iter()
    .enumerate()
    {
        let enabled = match mode {
            4 => terminal.insert_mode,
            6 => terminal.origin_mode,
            _ => protocol.modes().contains(&mode),
        };
        bits |= u64::from(enabled) << index;
    }
    out.extend(
        [
            ("modes", format!("TerminalModes {{ bits: {bits} }}")),
            (
                "keyboard_enhancement_flags",
                protocol.keyboard_enhancement_flags().to_string(),
            ),
            (
                "keyboard_enhancement_stack",
                format!("{:?}", protocol.keyboard_enhancement_stack()),
            ),
            (
                "alt_keyboard_enhancement_flags",
                protocol.inactive_keyboard_enhancement_flags().to_string(),
            ),
            (
                "alt_keyboard_enhancement_stack",
                format!("{:?}", protocol.inactive_keyboard_enhancement_stack()),
            ),
            (
                "xterm_modify_other_keys",
                protocol.xterm_modify_other_keys().to_string(),
            ),
            (
                "xterm_format_other_keys",
                protocol.xterm_format_other_keys().to_string(),
            ),
        ]
        .into_iter()
        .map(|(name, value)| (name.into(), lane(value))),
    );
}
