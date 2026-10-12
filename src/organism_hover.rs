//! Native button observations only; live attention policy lives in jterm_core.
/// Event bookkeeping only; it does not capture or consume the native event.
pub(crate) fn update_pressed_button<B: PartialEq>(buttons: &mut Vec<B>, button: B, pressed: bool) {
    if pressed {
        if !buttons.contains(&button) {
            buttons.push(button);
        }
    } else {
        buttons.retain(|current| *current != button);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn observed_buttons_track_multiple_presses_without_duplicates() {
        let mut buttons = Vec::new();
        update_pressed_button(&mut buttons, 1, true);
        update_pressed_button(&mut buttons, 1, true);
        update_pressed_button(&mut buttons, 2, true);
        assert_eq!(buttons, vec![1, 2]);
        update_pressed_button(&mut buttons, 1, false);
        assert_eq!(buttons, vec![2]);
        update_pressed_button(&mut buttons, 2, false);
        assert!(buttons.is_empty());
        update_pressed_button(&mut buttons, 3, true);
        buttons.clear(); // Native blur/leave revokes stale observed button state.
        assert!(buttons.is_empty());
    }
}
