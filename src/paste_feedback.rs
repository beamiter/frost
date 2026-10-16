//! Report partial insertion from the encoder's result, never from raw clipboard text.
use jterm_core::pty_input::{self, PasteModes, PastePolicy};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PasteOutcome {
    Empty,
    EmptyFirstLine,
    Rejected,
    Written,
    WrittenFirstLine,
}

impl PasteOutcome {
    pub(crate) fn written(self) -> bool {
        matches!(self, Self::Written | Self::WrittenFirstLine)
    }

    pub(crate) fn feedback(self) -> Option<&'static str> {
        match self {
            Self::WrittenFirstLine => {
                Some("Only the first line was inserted because bracketed paste is disabled")
            }
            Self::EmptyFirstLine => Some(
                "Nothing was inserted: the first line contained no insertable text and bracketed paste is disabled",
            ),
            _ => None,
        }
    }
}

/// The callback owns queue admission and the existing PTY write. Empty encoded
/// payloads never reach it, including the no-Ctrl+U empty-first-line case.
pub(crate) fn encode_and_write(
    text: &str,
    modes: PasteModes,
    policy: PastePolicy,
    clear_line_first: bool,
    write: impl FnOnce(&[u8]) -> bool,
) -> PasteOutcome {
    let paste = pty_input::encode_prompt_insert(text, modes, policy, clear_line_first);
    if paste.is_empty() {
        return if paste.risk.truncated_to_first_line
            && !paste.risk.had_visual_spoofing
            && !paste.risk.exceeded_review_limit
        {
            PasteOutcome::EmptyFirstLine
        } else {
            PasteOutcome::Empty
        };
    }
    if !write(&paste.bytes) {
        PasteOutcome::Rejected
    } else if paste.risk.truncated_to_first_line {
        PasteOutcome::WrittenFirstLine
    } else {
        PasteOutcome::Written
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jterm_core::pty_input::UnbracketedMultiline;

    #[test]
    fn actual_encoder_and_writer_preserve_bytes_and_report_only_actual_truncation() {
        for bracketed in [false, true] {
            for clear in [false, true] {
                for text in [
                    "one\ntwo",
                    "one\r\ntwo",
                    "héllo 世界\n次",
                    "one",
                    "\nnext",
                    "\r\nnext",
                    "\x01\nnext",
                    "",
                    "\x1b[200~\x1b[201~",
                ] {
                    for fallback in [
                        UnbracketedMultiline::FirstLineOnly,
                        UnbracketedMultiline::SendVerbatim,
                    ] {
                        let modes = PasteModes { bracketed };
                        let policy = PastePolicy::prompt_insert(fallback);
                        let expected = pty_input::encode_prompt_insert(text, modes, policy, clear);
                        for accepted in [false, true] {
                            let mut writes = Vec::new();
                            let outcome = encode_and_write(text, modes, policy, clear, |bytes| {
                                writes.push(bytes.to_vec());
                                accepted
                            });
                            if expected.is_empty() {
                                assert!(writes.is_empty());
                                assert!(!outcome.written());
                                assert_eq!(
                                    outcome.feedback().is_some(),
                                    expected.risk.truncated_to_first_line
                                );
                                if expected.risk.truncated_to_first_line {
                                    assert!(outcome
                                        .feedback()
                                        .unwrap()
                                        .starts_with("Nothing was inserted"));
                                }
                            } else {
                                assert_eq!(
                                    writes.as_slice(),
                                    std::slice::from_ref(&expected.bytes)
                                );
                                assert_eq!(outcome.written(), accepted);
                                assert_eq!(
                                    outcome.feedback().is_some(),
                                    accepted && expected.risk.truncated_to_first_line
                                );
                                if !accepted {
                                    assert_eq!(outcome, PasteOutcome::Rejected);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn safety_refusal_is_not_misreported_as_empty_first_line() {
        for text in [
            "\n\u{202e}hidden".to_string(),
            format!("\n{}", "x".repeat(256 * 1024)),
        ] {
            let outcome = encode_and_write(
                &text,
                PasteModes { bracketed: false },
                PastePolicy::prompt_insert(UnbracketedMultiline::FirstLineOnly),
                true,
                |_| panic!("must not write"),
            );
            assert_eq!(outcome, PasteOutcome::Empty);
            assert_eq!(outcome.feedback(), None);
        }
    }
}
