use super::TerminalState;
use jterm_engine::grid::CursorShape;
use serde_json::Value;
use std::collections::BTreeMap;

/// Read the frozen Frost observer lanes from the active engine state.
/// The old mode order is used only to compare against the original `Debug` data.
pub(super) fn observe(
    terminal: &TerminalState,
    out: &mut BTreeMap<String, Value>,
    lane: impl Fn(String) -> Value,
) {
    let protocol = &terminal.vt.protocol;
    let mut bits = 0u64;
    for (index, mode) in [
        7, 25, 1000, 1001, 1002, 1003, 1004, 1006, 1049, 2004, 2026, 2031, 5522, 1, 4, 6, 1005,
        1015, 66, 47, 1047, 1048,
    ]
    .into_iter()
    .enumerate()
    {
        let enabled = match mode {
            4 => terminal.vt.screen.insert_mode,
            6 => terminal.vt.screen.origin_mode,
            _ => protocol.modes().contains(&mode),
        };
        bits |= u64::from(enabled) << index;
    }
    let link = terminal.vt.screen.current_hyperlink;
    out.extend(
        [
            ("modes", format!("TerminalModes {{ bits: {bits} }}")),
            (
                "current_hyperlink",
                format!("{:?}", (!link.is_none()).then_some(link.as_raw())),
            ),
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

pub(super) fn saved_primary_cursor_shape(terminal: &TerminalState) -> CursorShape {
    terminal
        .vt
        .screen
        .saved_primary_cursor_shape
        .unwrap_or_default()
}
