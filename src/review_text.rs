//! frost-specific review-input extras layered on the shared
//! `jterm_core::review_input` primitives (the visual-spoof predicate and the
//! whole-string spoof check live there now). What stays local is what core
//! does not cover: per-surface byte limits, the parameterized single-line
//! validator, the multiline sanitizers, and bounded display escaping.

use std::fmt;

pub(crate) const MAX_AGENT_COMMAND_BYTES: usize = 16 * 1024;
/// Budget for a command reconstructed from the OSC 133 replay channel — the
/// text an agent may be handed to re-run. That channel's writer is
/// `jterm_core::execution_journal`, so read its number rather than
/// re-declaring one: a local copy is exactly how this constant ended up
/// disagreeing between the siblings.
///
/// This is NOT the shared-history budget. The family's command-history JSONL
/// is written by `jterm_core::command_history`, which accepts four times as
/// much; a reader that applies the replay budget to that file silently drops
/// every longer record a sibling wrote. That one lives beside its reader, as
/// `history_picker::MAX_SHARED_HISTORY_COMMAND_BYTES`.
pub(crate) const MAX_HISTORY_COMMAND_BYTES: usize =
    jterm_core::execution_journal::MAX_COMMAND_BYTES;
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
    if text.contains('\u{fffd}') || jterm_core::review_input::contains_visual_spoofing(text) {
        return Err(ReviewTextError::VisualSpoof);
    }
    Ok(text)
}

/// Bound the Agent proposal-edit iced field so an oversized or control-bearing
/// paste truncates instead of wiping the reviewed command.
pub(crate) fn bound_agent_edit_command(value: impl Into<String>) -> String {
    let mut value: String = value
        .into()
        .chars()
        .filter_map(|character| {
            if character.is_control() {
                None
            } else if jterm_core::review_input::is_visual_spoofing_character(character) {
                Some('\u{fffd}')
            } else {
                Some(character)
            }
        })
        .collect();
    if value.len() > MAX_AGENT_COMMAND_BYTES {
        let mut end = MAX_AGENT_COMMAND_BYTES;
        while end > 0 && !value.is_char_boundary(end) {
            end -= 1;
        }
        value.truncate(end);
    }
    value
}

pub(crate) fn agent_edit_command_is_unsafe(value: &str) -> bool {
    value.contains('\u{fffd}') || jterm_core::review_input::contains_visual_spoofing(value)
}

pub(crate) fn accepted_agent_edit_command(value: impl Into<String>) -> Option<String> {
    let value = bound_agent_edit_command(value);
    if agent_edit_command_is_unsafe(&value) {
        None
    } else {
        Some(value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AgentEditPrepareError {
    Empty,
    Unsafe,
}

/// Open the Agent edit field from a proposal command. Oversized text
/// truncates; spoofed or empty text refuses the whole edit.
pub(crate) fn prepared_agent_edit_command(
    command: impl Into<String>,
) -> Result<String, AgentEditPrepareError> {
    let command = bound_agent_edit_command(command);
    if agent_edit_command_is_unsafe(&command) {
        Err(AgentEditPrepareError::Unsafe)
    } else if command.trim_matches(' ').is_empty() {
        Err(AgentEditPrepareError::Empty)
    } else {
        Ok(command)
    }
}

fn is_c0_or_c1(character: char) -> bool {
    matches!(character as u32, 0x00..=0x1f | 0x7f..=0x9f)
}

/// Prepare clipboard/search/sidebar text for insertion into the shell editor.
/// LF and tab are structural product input; CR/CRLF normalize to LF. Every
/// other C0/C1 scalar is removed, while non-control visual spoofing fails
/// closed because this frontend has no Unicode-risk confirmation UI.
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
            visual
                if visual == '\u{fffd}'
                    || jterm_core::review_input::is_visual_spoofing_character(visual) =>
            {
                return Err(ReviewTextError::VisualSpoof);
            }
            visible => sanitized.push(visible),
        }
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

/// Regex compile failures quote the pattern. Keep that text on one UI line
/// without letting ESC or bidi marks in the draft alter surrounding chrome.
pub(crate) const MAX_REGEX_ERROR_BYTES: usize = 160;

pub(crate) fn safe_regex_error(error: impl fmt::Display) -> String {
    jterm_core::review_input::safe_inline_display(
        &format!("Invalid regex: {error}"),
        MAX_REGEX_ERROR_BYTES,
    )
}

/// Block-search and other picker compile failures quote the draft. Bound the
/// Display text without adding a second "Invalid regex" prefix.
pub(crate) fn bound_query_error(error: impl fmt::Display) -> String {
    jterm_core::review_input::safe_inline_display(&error.to_string(), MAX_REGEX_ERROR_BYTES)
}

/// Transient toast chrome: interpolated paths and error strings must not
/// restyle the overlay or grow without bound.
pub(crate) const MAX_TOAST_BYTES: usize = 256;

pub(crate) fn bound_toast_text(text: impl Into<String>) -> String {
    jterm_core::review_input::safe_inline_display(&text.into(), MAX_TOAST_BYTES)
}

/// Persistent startup diagnostics overlay: keep newlines so restore/config
/// notes stay readable, but drop other controls/spoofing and cap the payload.
pub(crate) const MAX_DIAGNOSTIC_BYTES: usize = 1024;

pub(crate) fn bound_diagnostic_text(text: impl Into<String>) -> String {
    let mut bounded = String::new();
    for ch in text.into().chars() {
        if ch == '\n' || ch == '\t' {
            if bounded.len().saturating_add(ch.len_utf8()) > MAX_DIAGNOSTIC_BYTES {
                break;
            }
            bounded.push(ch);
            continue;
        }
        if ch.is_control() {
            continue;
        }
        let ch = if jterm_core::review_input::is_visual_spoofing_character(ch) {
            '\u{fffd}'
        } else {
            ch
        };
        if bounded.len().saturating_add(ch.len_utf8()) > MAX_DIAGNOSTIC_BYTES {
            break;
        }
        bounded.push(ch);
    }
    bounded
}

/// Settings/chrome label for the configured AI provider. Display names are
/// untrusted (local Ollama tags, custom OpenAI-compatible servers).
pub(crate) const MAX_PROVIDER_LABEL_BYTES: usize = 256;

pub(crate) fn bound_provider_label(text: impl Into<String>) -> String {
    jterm_core::review_input::safe_inline_display(&text.into(), MAX_PROVIDER_LABEL_BYTES)
}

/// `/proc` comm names reach the pane header. Bound and neutralize them so a
/// hostile process name cannot reorder chrome.
pub(crate) const MAX_FOREGROUND_PROCESS_NAME_BYTES: usize = 256;

pub(crate) fn bound_foreground_process_name(name: impl AsRef<str>) -> Option<String> {
    let shown = jterm_core::review_input::safe_inline_display(
        name.as_ref(),
        MAX_FOREGROUND_PROCESS_NAME_BYTES,
    );
    let shown = shown.trim();
    if shown.is_empty() || shown.contains('\u{fffd}') {
        None
    } else {
        Some(shown.to_string())
    }
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
                if unsafe_character.is_control()
                    || jterm_core::review_input::is_visual_spoofing_character(unsafe_character) =>
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
        assert_eq!(
            validate_single_line("printf ok\u{fffd}", 256 * 1024),
            Err(ReviewTextError::VisualSpoof)
        );
        assert_eq!(
            sanitize_prompt_payload("echo ok\u{fffd}\n", 256 * 1024),
            Err(ReviewTextError::VisualSpoof)
        );
    }

    #[test]
    fn regex_error_replaces_controls_and_stays_bounded() {
        let error = format!(
            "unclosed group for `(\u{1b}[31m\u{202e}{}`",
            "x".repeat(400)
        );
        let shown = safe_regex_error(error);
        assert!(shown.starts_with("Invalid regex:"));
        assert!(!shown.contains('\u{1b}'));
        assert!(!shown.contains('\u{202e}'));
        assert!(shown.contains('\u{fffd}'));
        assert!(shown.len() <= MAX_REGEX_ERROR_BYTES);
        let picker = bound_query_error(format!(
            "Invalid regular expression: unclosed group for `(\u{1b}[31m{}`",
            "n".repeat(400)
        ));
        assert!(!picker.contains('\u{1b}'));
        assert!(picker.len() <= MAX_REGEX_ERROR_BYTES);
        assert!(picker.starts_with("Invalid regular expression:"));
    }

    #[test]
    fn toast_text_replaces_controls_and_stays_bounded() {
        let shown = bound_toast_text(format!(
            "Remote host \u{1b}[31m\u{202e}{}: failed",
            "n".repeat(400)
        ));
        assert!(!shown.contains('\u{1b}'));
        assert!(!shown.contains('\u{202e}'));
        assert!(shown.contains('\u{fffd}'));
        assert!(shown.len() <= MAX_TOAST_BYTES);
        assert!(shown.starts_with("Remote host"));
        let phase = bound_toast_text(format!(
            "Native session: Running\u{1b}[31m\u{202e}{}",
            "x".repeat(400)
        ));
        assert!(!phase.contains('\u{1b}'));
        assert!(!phase.contains('\u{202e}'));
        assert!(phase.len() <= MAX_TOAST_BYTES);
        assert!(phase.starts_with("Native session:"));
    }

    #[test]
    fn diagnostic_text_keeps_newlines_replaces_spoofing_and_stays_bounded() {
        let shown = bound_diagnostic_text(format!(
            "Could not read\n\u{1b}[31m\u{202e}{}",
            "x".repeat(2000)
        ));
        assert!(shown.contains('\n'));
        assert!(!shown.contains('\u{1b}'));
        assert!(!shown.contains('\u{202e}'));
        assert!(shown.contains('\u{fffd}'));
        assert!(shown.len() <= MAX_DIAGNOSTIC_BYTES);
        assert!(shown.starts_with("Could not read"));
        assert_eq!(bound_diagnostic_text("ok\tpath"), "ok\tpath");
    }

    #[test]
    fn provider_label_replaces_controls_and_stays_bounded() {
        let shown = bound_provider_label(format!("ollama\u{1b}[31m\u{202e}{}", "n".repeat(400)));
        assert!(!shown.contains('\u{1b}'));
        assert!(!shown.contains('\u{202e}'));
        assert!(shown.contains('\u{fffd}'));
        assert!(shown.len() <= MAX_PROVIDER_LABEL_BYTES);
        assert!(shown.starts_with("ollama"));
        let native = bound_provider_label(format!("Codex\u{202e}{}", "x".repeat(400)));
        assert!(!native.contains('\u{202e}'));
        assert!(native.len() <= MAX_PROVIDER_LABEL_BYTES);
        assert!(native.starts_with("Codex"));
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

    #[test]
    fn agent_edit_draft_truncates_instead_of_bouncing_and_drops_controls() {
        assert_eq!(bound_agent_edit_command("ls\n\u{1b} -la"), "ls -la");
        let spoofed = bound_agent_edit_command("git \u{202e}status");
        assert!(!spoofed.contains('\u{202e}'));
        assert!(spoofed.contains('\u{fffd}'));
        assert!(spoofed.starts_with("git "));
        assert!(accepted_agent_edit_command("git \u{202e}status").is_none());
        assert_eq!(
            accepted_agent_edit_command("git status").as_deref(),
            Some("git status")
        );
        let filled = bound_agent_edit_command(format!("{}y", "x".repeat(MAX_AGENT_COMMAND_BYTES)));
        assert_eq!(filled.len(), MAX_AGENT_COMMAND_BYTES);
        assert!(!filled.contains('y'));
        let overflow =
            bound_agent_edit_command(format!("{}z", "界".repeat(MAX_AGENT_COMMAND_BYTES)));
        assert!(overflow.len() <= MAX_AGENT_COMMAND_BYTES);
        assert!(overflow.is_char_boundary(overflow.len()));
        assert!(!overflow.contains('z'));
        assert!(bound_agent_edit_command("").is_empty());
        let oversized = format!("{}y", "x".repeat(MAX_AGENT_COMMAND_BYTES));
        let opened = prepared_agent_edit_command(oversized).expect("truncate opens edit");
        assert_eq!(opened.len(), MAX_AGENT_COMMAND_BYTES);
        assert!(!opened.contains('y'));
        assert_eq!(
            prepared_agent_edit_command("\u{1b}\n"),
            Err(AgentEditPrepareError::Empty)
        );
        assert_eq!(
            prepared_agent_edit_command("git \u{202e}status"),
            Err(AgentEditPrepareError::Unsafe)
        );
        assert_eq!(
            prepared_agent_edit_command("ls\n\u{1b} -la").as_deref(),
            Ok("ls -la")
        );
    }

    #[test]
    fn foreground_process_names_replace_spoofing_and_drop_empty() {
        assert_eq!(
            bound_foreground_process_name("cargo"),
            Some("cargo".to_string())
        );
        assert!(bound_foreground_process_name("nvim\u{202e}").is_none());
        assert!(bound_foreground_process_name("").is_none());
        assert!(bound_foreground_process_name("   ").is_none());
        let overflow = bound_foreground_process_name(format!(
            "{}z",
            "x".repeat(MAX_FOREGROUND_PROCESS_NAME_BYTES)
        ))
        .expect("truncated name");
        assert!(overflow.len() <= MAX_FOREGROUND_PROCESS_NAME_BYTES);
        assert!(!overflow.contains('z'));
    }
}
