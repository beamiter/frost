//! Host effects at the shared engine alternate-screen boundaries.
use super::*;

impl jterm_engine::native_alternate::NativeAlternateHost for TerminalState {
    fn alternate_state(&mut self) -> jterm_engine::native_alternate::NativeAlternateState<'_> {
        jterm_engine::native_alternate::NativeAlternateState {
            screen: &mut self.vt.screen,
            protocol: &mut self.vt.protocol,
            global_background: &mut self.global_bg,
        }
    }

    fn alternate_hook(&mut self, hook: jterm_engine::native_alternate::NativeAlternateHook) {
        use jterm_engine::native_alternate::NativeAlternateHook as Hook;
        match hook {
            Hook::PrepareEntry => {
                self.scroll_offset = 0;
                self.provisional_alt_snapshot = None;
            }
            Hook::OwnershipChanged { entering } => {
                self.watch_batch.interrupted = true;
                self.watch_batch.boundary = true;
                self.projection_scroll_guard.revoke();
                if entering {
                    self.organism_idle_cwd = None;
                }
            }
            Hook::FinalizeAlternate => self.provisional_alt_snapshot = None,
            Hook::ClearSelection => self.clear_text_selection(),
            Hook::ClearAlternate => self.clear_screen(),
            Hook::Repaint { rows } => {
                self.grid_version += rows.len() as u64 + 1;
                self.row_versions.fill(self.grid_version);
            }
            Hook::PrepareExit | Hook::GridSwapped | Hook::EntryCleared { .. } => {
                unreachable!("Ember-only alternate-screen hook")
            }
        }
    }
}

impl jterm_engine::native_alternate::FrostAlternateHost for TerminalState {
    fn frost_alternate_state(&mut self) -> jterm_engine::native_alternate::FrostAlternateState<'_> {
        use jterm_engine::native_alternate::{FrostAlternateState, NativeAlternateState};
        FrostAlternateState {
            native: NativeAlternateState {
                screen: &mut self.vt.screen,
                protocol: &mut self.vt.protocol,
                global_background: &mut self.global_bg,
            },
            disabled: self.disable_alt_screen,
            saved_background: &mut self.saved_primary_global_bg,
            colors: &mut self.colors,
            saved_colors: &mut self.saved_primary_colors,
        }
    }
}
