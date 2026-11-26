//! Frozen native OSC5522 policies before shared owner adoption.
use super::*;
impl TerminalState {
    fn frozen_osc5522(&mut self, metadata: &str, _payload: Option<&str>) {
        crate::debug_log!(
            "[OSC5522] metadata={} payload_bytes={}",
            crate::debug::format_bytes(metadata.as_bytes()),
            _payload.map_or(0, str::len)
        );

        let mut message_type = None;
        let mut mime = None;
        let mut password = None;

        for part in metadata.split(':') {
            if let Some(value) = part.strip_prefix("type=") {
                message_type = Some(value);
            } else if let Some(value) = part.strip_prefix("mime=") {
                mime = Self::decode_base64(value);
            } else if let Some(value) = part.strip_prefix("password=") {
                password = Self::decode_base64(value);
            } else if let Some(value) = part.strip_prefix("pw=") {
                password = Self::decode_base64(value);
            }
        }

        if message_type != Some("read") {
            return;
        }

        let kind = if let Some(mime_type) = mime {
            if Self::osc_5522_mime_is_unsafe(&mime_type) {
                self.append_osc_5522_status("type=read:status=ENOSYS", None);
                return;
            }
            if let Some(expected) = &self.osc5522.pending_paste_password {
                if password.as_deref() != Some(expected.as_str()) {
                    self.append_osc_5522_status("type=read:status=EPERM", None);
                    return;
                }
            }
            self.osc5522.pending_paste_password = None;
            ClipboardReadKind::MimeData(mime_type)
        } else {
            ClipboardReadKind::MimeList
        };

        // A PTY can emit thousands of tiny requests in one read batch. Keep the
        // amount of UI work / async clipboard tasks strictly bounded.
        const MAX_PENDING_CLIPBOARD_REQUESTS: usize = 8;
        if self.osc5522.pending_clipboard_requests.len() < MAX_PENDING_CLIPBOARD_REQUESTS {
            self.osc5522
                .pending_clipboard_requests
                .push(ClipboardReadRequest { kind });
        }
    }
    fn osc_5522_mime_is_unsafe(mime: &str) -> bool {
        mime.is_empty()
            || mime.len() > 128
            || mime.chars().any(|character| {
                !character.is_ascii()
                    || character.is_ascii_control()
                    || character == '\u{fffd}'
                    || jterm_core::review_input::is_visual_spoofing_character(character)
            })
    }
    fn frozen_paste_event(&mut self, mime_types: &[String]) -> Vec<u8> {
        let password = uuid::Uuid::new_v4().to_string();
        self.osc5522.pending_paste_password = Some(password.clone());
        let encoded_password =
            base64::engine::general_purpose::STANDARD.encode(password.as_bytes());
        let mut output = Vec::new();

        output.extend_from_slice(b"\x1b]5522;type=read:status=OK:password=");
        output.extend_from_slice(encoded_password.as_bytes());
        output.extend_from_slice(b"\x1b\\");

        for mime_type in mime_types {
            let encoded_mime =
                base64::engine::general_purpose::STANDARD.encode(mime_type.as_bytes());
            output.extend_from_slice(b"\x1b]5522;type=read:status=DATA:mime=");
            output.extend_from_slice(encoded_mime.as_bytes());
            output.extend_from_slice(b"\x1b\\");
        }

        output.extend_from_slice(b"\x1b]5522;type=read:status=DONE\x1b\\");
        output
    }
}

fn b64(s: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(s.as_bytes())
}
#[test]
fn osc5522_owner_matches_frozen_frost_policy() {
    for password in [None, Some("token")] {
        for capacity in [0, 7, 8] {
            for mime in [
                None,
                Some("text/plain".to_owned()),
                Some("text plain".to_owned()),
                Some("".to_owned()),
                Some("a".repeat(128)),
                Some("a".repeat(129)),
                Some("text/\u{202e}plain".to_owned()),
            ] {
                for supplied in ["token", "wrong", ""] {
                    for key in ["pw", "password"] {
                        let make = || {
                            let mut t = TerminalState::new(8, 3);
                            t.osc_reply_terminator = b"\x07";
                            t.osc5522.pending_paste_password = password.map(str::to_owned);
                            t.osc5522.pending_clipboard_requests = vec![
                                ClipboardReadRequest {
                                    kind: ClipboardReadKind::MimeList
                                };
                                capacity
                            ];
                            t
                        };
                        let mut a = make();
                        let mut e = make();
                        let mut metadata = format!("type=read:{key}={}", b64(supplied));
                        if let Some(mime) = &mime {
                            metadata.push_str(&format!(":mime={}", b64(mime)));
                        }
                        a.handle_osc_5522(&metadata, Some("ignored payload"));
                        e.frozen_osc5522(&metadata, Some("ignored payload"));
                        assert_eq!(a.output_buffer, e.output_buffer);
                        assert_eq!(
                            a.osc5522.pending_clipboard_requests,
                            e.osc5522.pending_clipboard_requests
                        );
                        assert_eq!(
                            a.osc5522.pending_paste_password,
                            e.osc5522.pending_paste_password
                        );
                    }
                }
            }
        }
    }
}
#[test]
fn osc5522_paste_issue_matches_frozen_frost_wire_and_password() {
    let mut a = TerminalState::new(8, 3);
    let mut e = TerminalState::new(8, 3);
    let mimes = vec![
        "text/plain".into(),
        "text/plain".into(),
        "text plain".into(),
        "image/png".into(),
    ];
    let a_wire = a.build_paste_event(&mimes);
    let e_wire = e.frozen_paste_event(&mimes);
    let normalize = |wire: Vec<u8>, t: &TerminalState| {
        String::from_utf8(wire).unwrap().replace(
            &b64(t.osc5522.pending_paste_password.as_ref().unwrap()),
            "TOKEN",
        )
    };
    assert_eq!(normalize(a_wire, &a), normalize(e_wire, &e));
    assert_eq!(
        a.take_clipboard_read_requests(),
        e.take_clipboard_read_requests()
    );
}
