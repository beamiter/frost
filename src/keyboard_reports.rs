//! Physical key reporting shared by Frost's terminal and Vim9 workbench.
use crate::terminal::TerminalState;
use iced::keyboard::{
    self,
    key::{Code, Named, Physical},
    Key,
};
use iced_winit::futures::event::KeyboardMetadata;
use jterm_engine::input::{
    encode_kitty_text_press, kitty_keypad_report, kitty_modifier_report, kitty_named_report,
    kitty_text_report, KeyboardLocks, KittyEventType, KittyKeyReport, KittyModifierKey,
    KittyTextKey, Modifiers, KITTY_DISAMBIGUATE, KITTY_REPORT_ALL_KEYS,
    KITTY_REPORT_ASSOCIATED_TEXT, KITTY_REPORT_EVENT_TYPES,
};
use smallvec::SmallVec;

#[derive(Default)]
pub(crate) struct Reports {
    generation: Option<u64>,
    held: SmallVec<[(Physical, KittyKeyReport); 8]>,
}

pub(crate) struct Press<'a> {
    pub key: &'a Key,
    pub modified_key: &'a Key,
    pub physical: Physical,
    pub location: keyboard::Location,
    pub modifiers: keyboard::Modifiers,
    pub text: Option<&'a str>,
    pub repeat: bool,
}

pub(crate) struct Prepared {
    pub bytes: Vec<u8>,
    hold: Option<(Physical, KittyKeyReport)>,
}

impl Reports {
    pub fn clear(&mut self) {
        self.generation = None;
        self.held.clear();
    }

    fn sync(&mut self, generation: u64) {
        if self.generation != Some(generation) {
            self.held.clear();
            self.generation = Some(generation);
        }
    }

    pub fn discard(&mut self, physical: Physical) {
        self.take(physical);
    }

    fn take(&mut self, physical: Physical) -> Option<KittyKeyReport> {
        let index = self.held.iter().position(|(key, _)| *key == physical)?;
        Some(self.held.swap_remove(index).1)
    }

    pub fn prepare(
        &mut self,
        terminal: &TerminalState,
        press: Press<'_>,
        metadata: Option<KeyboardMetadata>,
    ) -> Option<Prepared> {
        if let Some(report) = metadata.and_then(|metadata| self.press(terminal, &press, metadata)) {
            return Some(report);
        }
        let bytes = super::encode_key_input(
            press.key,
            press.modifiers,
            press.text,
            terminal.is_application_cursor_keys(),
            super::KeyboardEnhancements::from_terminal(terminal),
            super::keypad_position(
                press.key,
                press.location,
                Some(press.physical),
                metadata.and_then(KeyboardMetadata::keypad_num_lock),
            ),
        )?;
        Some(Prepared { bytes, hold: None })
    }

    fn press(
        &mut self,
        terminal: &TerminalState,
        press: &Press<'_>,
        metadata: KeyboardMetadata,
    ) -> Option<Prepared> {
        let (flags, generation) = terminal.keyboard_report_state();
        if flags
            & (u16::from(KITTY_DISAMBIGUATE) | KITTY_REPORT_ALL_KEYS | KITTY_REPORT_EVENT_TYPES)
            == 0
        {
            self.clear();
            return None;
        }
        self.sync(generation);
        let modifiers = wire_modifiers(metadata.modifiers);
        let plain = !modifiers.ctrl && !modifiers.alt && !modifiers.super_key;
        let text = if plain
            && flags & KITTY_REPORT_ALL_KEYS != 0
            && (flags & KITTY_REPORT_ASSOCIATED_TEXT != 0 || matches!(press.key, Key::Unidentified))
        {
            press.text.and_then(super::kitty_committed_text)
        } else {
            None
        };
        let report = match select_report(press, metadata, modifiers, flags) {
            Some(report) => report,
            None => {
                if matches!(press.key, Key::Unidentified) {
                    let bytes = encode_kitty_text_press(
                        None,
                        modifiers,
                        KeyboardLocks::default(),
                        flags,
                        text.as_deref(),
                    )?;
                    return Some(Prepared { bytes, hold: None });
                }
                return None;
            }
        };

        let track =
            flags & KITTY_REPORT_EVENT_TYPES != 0 && matches!(press.physical, Physical::Code(_));
        let (report, phase) = if track && press.repeat {
            let Some((_, initial)) = self.held.iter().find(|(key, _)| *key == press.physical)
            else {
                // A host shortcut or a previous owner took the original press.
                return Some(Prepared {
                    bytes: Vec::new(),
                    hold: None,
                });
            };
            (
                initial.with_repeat_alternate(report),
                KittyEventType::Repeat,
            )
        } else {
            (report, KittyEventType::Press)
        };
        let locks = KeyboardLocks {
            caps_lock: flags & KITTY_REPORT_ALL_KEYS != 0 && metadata.caps_lock.unwrap_or(false),
            num_lock: (flags & KITTY_REPORT_ALL_KEYS != 0 || metadata.keypad_press)
                && metadata.num_lock.unwrap_or(false),
        };
        let bytes = report.encode(phase, modifiers, locks, flags, text.as_deref())?;
        Some(Prepared {
            bytes,
            hold: (track && phase == KittyEventType::Press).then_some((press.physical, report)),
        })
    }

    pub fn admit(&mut self, prepared: Prepared, accepted: bool) {
        if let Some((physical, report)) = prepared.hold {
            self.discard(physical);
            if accepted {
                self.held.push((physical, report));
            }
        }
    }

    pub fn release(
        &mut self,
        terminal: &TerminalState,
        physical: Physical,
        metadata: KeyboardMetadata,
    ) -> Option<Vec<u8>> {
        if self.held.is_empty() {
            return None;
        }
        let (flags, generation) = terminal.keyboard_report_state();
        self.sync(generation);
        let report = self.take(physical)?;
        let locks = KeyboardLocks {
            caps_lock: flags & KITTY_REPORT_ALL_KEYS != 0 && metadata.caps_lock.unwrap_or(false),
            num_lock: (flags & KITTY_REPORT_ALL_KEYS != 0
                || super::keypad_position(
                    &Key::Unidentified,
                    keyboard::Location::Numpad,
                    Some(physical),
                    metadata.num_lock,
                )
                .is_some())
                && metadata.num_lock.unwrap_or(false),
        };
        report.encode(
            KittyEventType::Release,
            wire_modifiers(metadata.modifiers),
            locks,
            flags,
            None,
        )
    }
}

fn wire_modifiers(modifiers: keyboard::Modifiers) -> Modifiers {
    Modifiers {
        shift: modifiers.shift(),
        alt: modifiers.alt(),
        ctrl: modifiers.control(),
        super_key: modifiers.logo(),
    }
}

fn select_report(
    press: &Press<'_>,
    metadata: KeyboardMetadata,
    modifiers: Modifiers,
    flags: u16,
) -> Option<KittyKeyReport> {
    if let Some((position, _)) = super::keypad_position(
        press.modified_key,
        press.location,
        Some(press.physical),
        metadata.num_lock,
    ) {
        return kitty_keypad_report(
            position,
            super::keypad_logical_key(press.modified_key),
            modifiers,
            flags,
        );
    }

    if let Key::Named(named) = press.key {
        if let Some(modifier) = modifier_key(*named, press.physical) {
            return kitty_modifier_report(modifier, flags);
        }
        if *named != Named::Space {
            return kitty_named_report(super::terminal_named_key(named), modifiers, flags);
        }
    }
    let base = printable(press.key)?;
    kitty_text_report(
        KittyTextKey {
            base,
            shifted: modifiers
                .shift
                .then(|| printable(press.modified_key))
                .flatten(),
            base_layout: pc101_key(press.physical),
        },
        modifiers,
        flags,
    )
}

fn printable(key: &Key) -> Option<char> {
    match key {
        Key::Character(value) => {
            let mut chars = value.chars();
            let first = chars.next()?;
            (chars.next().is_none() && !first.is_control()).then_some(first)
        }
        Key::Named(Named::Space) => Some(' '),
        _ => None,
    }
}

fn modifier_key(named: Named, physical: Physical) -> Option<KittyModifierKey> {
    use KittyModifierKey as Modifier;
    Some(match (named, physical) {
        (Named::Shift, Physical::Code(Code::ShiftRight)) => Modifier::ShiftRight,
        (Named::Shift, _) => Modifier::ShiftLeft,
        (Named::Control, Physical::Code(Code::ControlRight)) => Modifier::ControlRight,
        (Named::Control, _) => Modifier::ControlLeft,
        (Named::Alt | Named::AltGraph, Physical::Code(Code::AltRight)) => Modifier::AltRight,
        (Named::Alt | Named::AltGraph, _) => Modifier::AltLeft,
        (Named::Super | Named::Meta, Physical::Code(Code::SuperRight)) => Modifier::SuperRight,
        (Named::Super | Named::Meta, _) => Modifier::SuperLeft,
        (Named::CapsLock, _) => Modifier::CapsLock,
        (Named::NumLock, _) => Modifier::NumLock,
        (Named::ScrollLock, _) => Modifier::ScrollLock,
        _ => return None,
    })
}

fn pc101_key(physical: Physical) -> Option<char> {
    let Physical::Code(code) = physical else {
        return None;
    };
    Some(match code {
        Code::KeyA => 'a',
        Code::KeyB => 'b',
        Code::KeyC => 'c',
        Code::KeyD => 'd',
        Code::KeyE => 'e',
        Code::KeyF => 'f',
        Code::KeyG => 'g',
        Code::KeyH => 'h',
        Code::KeyI => 'i',
        Code::KeyJ => 'j',
        Code::KeyK => 'k',
        Code::KeyL => 'l',
        Code::KeyM => 'm',
        Code::KeyN => 'n',
        Code::KeyO => 'o',
        Code::KeyP => 'p',
        Code::KeyQ => 'q',
        Code::KeyR => 'r',
        Code::KeyS => 's',
        Code::KeyT => 't',
        Code::KeyU => 'u',
        Code::KeyV => 'v',
        Code::KeyW => 'w',
        Code::KeyX => 'x',
        Code::KeyY => 'y',
        Code::KeyZ => 'z',
        Code::Digit0 => '0',
        Code::Digit1 => '1',
        Code::Digit2 => '2',
        Code::Digit3 => '3',
        Code::Digit4 => '4',
        Code::Digit5 => '5',
        Code::Digit6 => '6',
        Code::Digit7 => '7',
        Code::Digit8 => '8',
        Code::Digit9 => '9',
        Code::Space => ' ',
        Code::Minus => '-',
        Code::Equal => '=',
        Code::BracketLeft => '[',
        Code::BracketRight => ']',
        Code::Backslash => '\\',
        Code::Semicolon => ';',
        Code::Quote => '\'',
        Code::Backquote => '`',
        Code::Comma => ',',
        Code::Period => '.',
        Code::Slash => '/',
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kitty_release_and_repeat_require_an_admitted_press_in_the_current_generation() {
        let mut terminal = TerminalState::new(80, 24);
        terminal.process_input(b"\x1b[=26u");
        let key = Key::Character("a".into());
        let physical = Physical::Code(Code::KeyA);
        let modifiers = keyboard::Modifiers::empty();
        let metadata = KeyboardMetadata {
            modifiers,
            caps_lock: Some(false),
            num_lock: Some(false),
            keypad_press: false,
        };
        let press = |repeat| Press {
            key: &key,
            modified_key: &key,
            physical,
            location: keyboard::Location::Standard,
            modifiers,
            text: Some("a"),
            repeat,
        };
        let mut reports = Reports::default();
        assert!(reports.release(&terminal, physical, metadata).is_none());
        let rejected = reports
            .prepare(&terminal, press(false), Some(metadata))
            .unwrap();
        assert_eq!(rejected.bytes, b"\x1b[97;;97u");
        reports.admit(rejected, false);
        assert!(reports
            .prepare(&terminal, press(true), Some(metadata))
            .unwrap()
            .bytes
            .is_empty());

        let admitted = reports
            .prepare(&terminal, press(false), Some(metadata))
            .unwrap();
        reports.admit(admitted, true);
        assert_eq!(
            reports
                .prepare(&terminal, press(true), Some(metadata))
                .unwrap()
                .bytes,
            b"\x1b[97;1:2;97u",
        );
        assert_eq!(
            reports.release(&terminal, physical, metadata).unwrap(),
            b"\x1b[97;1:3u",
        );
        assert!(reports.release(&terminal, physical, metadata).is_none());

        let admitted = reports
            .prepare(&terminal, press(false), Some(metadata))
            .unwrap();
        reports.admit(admitted, true);
        terminal.process_input(b"\x1b[=0u\x1b[=26u");
        assert!(reports.release(&terminal, physical, metadata).is_none());
    }

    #[test]
    fn kitty_clean_associated_text_reuses_its_utf8_buffer() {
        use std::borrow::Cow;
        let text = "éф你";
        assert!(
            matches!(super::super::kitty_committed_text(text), Some(Cow::Borrowed(value))
            if std::ptr::eq(value.as_ptr(), text.as_ptr()))
        );
        assert_eq!(
            super::super::kitty_committed_text("é\n\u{202e}你").as_deref(),
            Some("é你"),
        );
    }
}
