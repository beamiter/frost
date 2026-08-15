//! Local compatibility with the current review-input contract while frost
//! remains exact-pinned to an older published `jterm_core`/`jagent` pair.

use std::fmt;

pub(crate) const MAX_AGENT_COMMAND_BYTES: usize = 16 * 1024;
pub(crate) const MAX_HISTORY_COMMAND_BYTES: usize = 256 * 1024;
pub(crate) const MAX_PROMPT_INSERT_BYTES: usize = 256 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReviewTextError {
    Empty,
    TooLarge { limit: usize },
    ControlCharacter,
    VisualSpoof,
}

impl fmt::Display for ReviewTextError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("the command is empty"),
            Self::TooLarge { limit } => {
                write!(formatter, "the command exceeds the {limit}-byte limit")
            }
            Self::ControlCharacter => {
                formatter.write_str("the command contains a terminal control character")
            }
            Self::VisualSpoof => formatter
                .write_str("the command contains invisible or bidirectional formatting characters"),
        }
    }
}

pub(crate) fn is_visual_spoof(character: char) -> bool {
    (character.is_whitespace() && character != ' ')
        || matches!(
            character,
            '\u{00ad}'
                | '\u{034f}'
                | '\u{061c}'
                | '\u{115f}'..='\u{1160}'
                | '\u{17b4}'..='\u{17b5}'
                | '\u{180b}'..='\u{180f}'
                | '\u{200b}'..='\u{200f}'
                | '\u{2028}'..='\u{202e}'
                | '\u{2060}'..='\u{206f}'
                | '\u{3164}'
                | '\u{fe00}'..='\u{fe0f}'
                | '\u{feff}'
                | '\u{ffa0}'
                | '\u{1bca0}'..='\u{1bca3}'
                | '\u{1d173}'..='\u{1d17a}'
                | '\u{e0001}'
                | '\u{e0020}'..='\u{e007f}'
                | '\u{e0100}'..='\u{e01ef}'
        )
}

pub(crate) fn contains_visual_spoofing(text: &str) -> bool {
    text.chars().any(is_visual_spoof)
}

pub(crate) fn validate_single_line(text: &str, max_bytes: usize) -> Result<&str, ReviewTextError> {
    if text.len() > max_bytes {
        return Err(ReviewTextError::TooLarge { limit: max_bytes });
    }
    if text.trim_matches(' ').is_empty() {
        return Err(ReviewTextError::Empty);
    }
    if text.chars().any(char::is_control) {
        return Err(ReviewTextError::ControlCharacter);
    }
    if contains_visual_spoofing(text) {
        return Err(ReviewTextError::VisualSpoof);
    }
    Ok(text)
}

fn is_c0_or_c1(character: char) -> bool {
    matches!(character as u32, 0x00..=0x1f | 0x7f..=0x9f)
}

/// Prepare clipboard/search/sidebar text for insertion into the shell editor.
/// LF and tab are structural product input; CR/CRLF normalize to LF. Every
/// other C0/C1 scalar is removed, while non-control visual spoofing fails
/// closed because this pinned frontend has no Unicode-risk confirmation UI.
pub(crate) fn sanitize_prompt_payload(
    text: &str,
    max_bytes: usize,
) -> Result<String, ReviewTextError> {
    if text.len() > max_bytes {
        return Err(ReviewTextError::TooLarge { limit: max_bytes });
    }
    let mut sanitized = String::with_capacity(text.len());
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '\r' => {
                if characters.peek() == Some(&'\n') {
                    characters.next();
                }
                sanitized.push('\n');
            }
            '\n' | '\t' => sanitized.push(character),
            control if is_c0_or_c1(control) => {}
            visual if is_visual_spoof(visual) => return Err(ReviewTextError::VisualSpoof),
            visible => sanitized.push(visible),
        }
    }
    Ok(sanitized)
}

/// Prepare a replayed history command for the task validation path. CR/CRLF
/// normalize to LF and LF/tab stay structural; every other C0/C1 scalar is
/// removed and non-control visual spoofing fails closed. The result must
/// retain some non-whitespace text.
pub(crate) fn sanitize_history_replay(
    text: &str,
    max_bytes: usize,
) -> Result<String, ReviewTextError> {
    if text.len() > max_bytes {
        return Err(ReviewTextError::TooLarge { limit: max_bytes });
    }
    let mut sanitized = String::with_capacity(text.len());
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '\r' => {
                if characters.peek() == Some(&'\n') {
                    characters.next();
                }
                sanitized.push('\n');
            }
            '\n' | '\t' => sanitized.push(character),
            control if is_c0_or_c1(control) => {}
            visual if is_visual_spoof(visual) => return Err(ReviewTextError::VisualSpoof),
            visible => sanitized.push(visible),
        }
    }
    if sanitized
        .trim_matches(|character| matches!(character, ' ' | '\n' | '\t'))
        .is_empty()
    {
        return Err(ReviewTextError::Empty);
    }
    Ok(sanitized)
}

/// Strip C0/C1 from an untrusted prompt-recall/Agent payload, then apply the
/// strict single-line and visual-spoof gate. This is defense in depth: normal
/// history and jagent proposals are rejected before this final payload seam.
pub(crate) fn sanitize_untrusted_single_line(
    text: &str,
    max_bytes: usize,
) -> Result<String, ReviewTextError> {
    if text.len() > max_bytes {
        return Err(ReviewTextError::TooLarge { limit: max_bytes });
    }
    let stripped: String = text
        .chars()
        .filter(|character| !is_c0_or_c1(*character))
        .collect();
    let stripped = stripped.trim_matches(' ').to_string();
    validate_single_line(&stripped, max_bytes)?;
    Ok(stripped)
}

pub(crate) fn visible_bounded(text: &str, max_bytes: usize) -> String {
    let mut visible = String::with_capacity(text.len().min(max_bytes));
    let mut truncated = false;
    for character in text.chars() {
        let replacement = match character {
            '\n' => "\\n".to_string(),
            '\r' => "\\r".to_string(),
            '\t' => "\\t".to_string(),
            unsafe_character
                if unsafe_character.is_control() || is_visual_spoof(unsafe_character) =>
            {
                format!("\\u{{{:X}}}", unsafe_character as u32)
            }
            safe => safe.to_string(),
        };
        if replacement.len() > max_bytes.saturating_sub(visible.len()) {
            truncated = true;
            break;
        }
        visible.push_str(&replacement);
    }
    if truncated && max_bytes >= 3 {
        while "…".len() > max_bytes.saturating_sub(visible.len()) {
            if visible.pop().is_none() {
                break;
            }
        }
        visible.push('…');
    }
    visible
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validator_rejects_the_complete_visual_spoof_contract() {
        let unsafe_characters = [
            '\u{00a0}',
            '\u{2003}',
            '\u{00ad}',
            '\u{034f}',
            '\u{061c}',
            '\u{115f}',
            '\u{1160}',
            '\u{17b4}',
            '\u{17b5}',
            '\u{180b}',
            '\u{180f}',
            '\u{200b}',
            '\u{200f}',
            '\u{2028}',
            '\u{202e}',
            '\u{2060}',
            '\u{206f}',
            '\u{3164}',
            '\u{fe00}',
            '\u{fe0f}',
            '\u{feff}',
            '\u{ffa0}',
            '\u{1bca0}',
            '\u{1bca3}',
            '\u{1d173}',
            '\u{1d17a}',
            '\u{e0001}',
            '\u{e0020}',
            '\u{e007f}',
            '\u{e0100}',
            '\u{e01ef}',
        ];
        for hidden in unsafe_characters {
            assert_eq!(
                validate_single_line(&format!("printf safe{hidden}"), 256 * 1024),
                Err(ReviewTextError::VisualSpoof),
                "{hidden:?}"
            );
        }
        assert!(validate_single_line("printf '编译🙂'", 256 * 1024).is_ok());
    }

    #[test]
    fn final_untrusted_payload_strips_c0_c1_then_rejects_spoofing() {
        assert_eq!(
            sanitize_untrusted_single_line("  echo\x1b[31m\tvalue  ", 4096).unwrap(),
            "echo[31mvalue"
        );
        assert_eq!(
            sanitize_untrusted_single_line("echo safe\u{2066}hidden", 4096),
            Err(ReviewTextError::VisualSpoof)
        );
    }

    #[test]
    fn prompt_payload_preserves_structure_but_rejects_hidden_unicode() {
        assert_eq!(
            sanitize_prompt_payload("one\r\ntwo\tthree\u{1b}[31m雪🙂", 4096).unwrap(),
            "one\ntwo\tthree[31m雪🙂"
        );
        for hidden in ['\u{00a0}', '\u{202e}', '\u{e0100}'] {
            assert_eq!(
                sanitize_prompt_payload(&format!("echo safe{hidden}hidden"), 4096),
                Err(ReviewTextError::VisualSpoof)
            );
        }
    }

    #[test]
    fn display_escapes_hidden_text_and_is_bounded() {
        assert_eq!(
            visible_bounded("safe\u{202e}\ttext", 64),
            "safe\\u{202E}\\ttext"
        );
        assert!(visible_bounded(&"\u{202e}".repeat(100), 32).len() <= 32);
    }
}
