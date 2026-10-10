use serde::{Deserialize, Serialize};

/// 高级搜索配置
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct SearchConfig {
    pub use_regex: bool,
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub multi_line: bool,
}

/// 替换选项
#[derive(Clone, Debug, Default)]
pub struct ReplaceOptions {
    pub replace_all: bool,
}

/// Shared prompt/clipboard replacement result envelope. Fail rather than
/// truncate when expansion exceeds the same budget as prompt insertion.
pub(crate) const MAX_REPLACEMENT_RESULT_BYTES: usize = crate::review_text::MAX_PROMPT_INSERT_BYTES;

fn replacement_growth(current: usize, additional: usize, limit: usize) -> Result<usize, String> {
    current
        .checked_add(additional)
        .filter(|size| *size <= limit)
        .ok_or_else(|| format!("replacement result exceeds the {limit}-byte limit"))
}

fn append_replacement(result: &mut String, part: &str, limit: usize) -> Result<(), String> {
    replacement_growth(result.len(), part.len(), limit)?;
    result.push_str(part);
    Ok(())
}

/// Count expansion bytes without creating an expanded temporary. Mirrors the
/// pinned regex 1.13.1 capture interpolation grammar, with differential tests
/// below: longest ASCII unbraced name, arbitrary braced name, $$ and malformed
/// references. Numeric parse failure (including overflow) falls back to name.
fn capture_expansion_len(
    captures: &regex::Captures<'_>,
    mut template: &str,
    limit: usize,
) -> Result<usize, String> {
    let mut size = 0;
    while let Some(dollar) = template.find('$') {
        size = replacement_growth(size, dollar, limit)?;
        template = &template[dollar..];
        if template.as_bytes().get(1) == Some(&b'$') {
            size = replacement_growth(size, 1, limit)?;
            template = &template[2..];
            continue;
        }
        let reference = if template.as_bytes().get(1) == Some(&b'{') {
            template[2..]
                .find('}')
                .map(|end| (&template[2..2 + end], end + 3))
        } else {
            let end = 1 + template.as_bytes()[1..]
                .iter()
                .take_while(|byte| byte.is_ascii_alphanumeric() || **byte == b'_')
                .count();
            (end > 1).then(|| (&template[1..end], end))
        };
        let Some((name, end)) = reference else {
            size = replacement_growth(size, 1, limit)?;
            template = &template[1..];
            continue;
        };
        let capture = match name.parse::<usize>() {
            Ok(index) => captures.get(index),
            Err(_) => captures.name(name),
        };
        if let Some(value) = capture {
            size = replacement_growth(size, value.len(), limit)?;
        }
        template = &template[end..];
    }
    replacement_growth(size, template.len(), limit)
}

/// 搜索和替换引擎
pub struct SearchAndReplaceEngine;

impl SearchAndReplaceEngine {
    /// 执行搜索和替换
    pub fn search_and_replace(
        text: &str,
        search_pattern: &str,
        replacement: &str,
        config: &SearchConfig,
        options: &ReplaceOptions,
    ) -> Result<(String, usize), String> {
        Self::search_and_replace_with_limit(
            text,
            search_pattern,
            replacement,
            config,
            options,
            MAX_REPLACEMENT_RESULT_BYTES,
        )
    }

    fn search_and_replace_with_limit(
        text: &str,
        search_pattern: &str,
        replacement: &str,
        config: &SearchConfig,
        options: &ReplaceOptions,
        limit: usize,
    ) -> Result<(String, usize), String> {
        // No-match/empty-pattern outputs must obey the same envelope too.
        if search_pattern.is_empty() {
            replacement_growth(0, text.len(), limit)?;
            return Ok((text.to_string(), 0));
        }
        if config.use_regex {
            Self::regex_replace(text, search_pattern, replacement, config, options, limit)
        } else {
            Self::literal_replace(text, search_pattern, replacement, config, options, limit)
        }
    }

    /// 文字替换。逐字符扫描（语义与 ember 引擎一致）：大小写折叠按字符比较
    /// （`match_len_at`），全词由 `is_whole_word` 判定；替换后从匹配末尾继续，
    /// 已替换文本绝不重扫（替换串仍含 pattern 时不会死循环）。
    fn literal_replace(
        text: &str,
        pattern: &str,
        replacement: &str,
        config: &SearchConfig,
        options: &ReplaceOptions,
        limit: usize,
    ) -> Result<(String, usize), String> {
        // Empty pattern would match endlessly; treat as a no-op.
        if pattern.is_empty() {
            replacement_growth(0, text.len(), limit)?;
            return Ok((text.to_string(), 0));
        }

        let mut result = String::with_capacity(text.len().min(limit));
        let mut count = 0;
        let mut i = 0;

        while i < text.len() {
            let take_more = options.replace_all || count == 0;
            if take_more {
                if let Some(mlen) = match_len_at(text, i, pattern, config.case_sensitive) {
                    if mlen > 0 && (!config.whole_word || is_whole_word(text, i, i + mlen)) {
                        append_replacement(&mut result, replacement, limit)?;
                        i += mlen;
                        count += 1;
                        continue;
                    }
                }
            }
            // Copy a single UTF-8 character from the original text.
            // text.get 避免 i 落在非字符边界时的切片 panic(mlen 理论上总落边界,
            // 但此处容错跳过一字节,绝不 panic)。
            let Some(ch) = text.get(i..).and_then(|s| s.chars().next()) else {
                i += 1;
                continue;
            };
            replacement_growth(result.len(), ch.len_utf8(), limit)?;
            result.push(ch);
            i += ch.len_utf8();
        }

        Ok((result, count))
    }

    /// 正则表达式替换
    fn regex_replace(
        text: &str,
        pattern: &str,
        replacement: &str,
        config: &SearchConfig,
        options: &ReplaceOptions,
        limit: usize,
    ) -> Result<(String, usize), String> {
        use regex::RegexBuilder;

        let effective = if config.whole_word {
            format!(r"\b(?:{pattern})\b")
        } else {
            pattern.to_string()
        };

        let regex = RegexBuilder::new(&effective)
            .case_insensitive(!config.case_sensitive)
            .multi_line(config.multi_line)
            .build()
            .map_err(crate::review_text::safe_regex_error)?;

        let mut result = String::with_capacity(text.len().min(limit));
        let mut count = 0;
        let mut consumed = 0;
        for captures in regex.captures_iter(text) {
            let matched = captures
                .get(0)
                .expect("regex captures include the full match");
            append_replacement(&mut result, &text[consumed..matched.start()], limit)?;
            let expansion = capture_expansion_len(&captures, replacement, limit)?;
            replacement_growth(result.len(), expansion, limit)?;
            // Exact preflight above covers every append made by expand;
            // neither an oversized temporary nor a partial result escapes.
            captures.expand(replacement, &mut result);
            consumed = matched.end();
            count += 1;
            if !options.replace_all {
                break;
            }
        }
        append_replacement(&mut result, &text[consumed..], limit)?;

        Ok((result, count))
    }

    /// Preview windows around literal substring matches.
    ///
    /// An empty pattern is a no-op: `str::contains("")` is true for every line
    /// and would otherwise dump the whole buffer. Context is clamped so a
    /// hostile `context_lines` cannot allocate a quadratic preview.
    #[cfg(test)]
    fn get_match_context(text: &str, pattern: &str, context_lines: usize) -> Vec<String> {
        if pattern.is_empty() {
            return Vec::new();
        }
        const MAX_CONTEXT_LINES: usize = 8;
        const MAX_MATCH_WINDOWS: usize = 64;
        let context_lines = context_lines.min(MAX_CONTEXT_LINES);
        let lines: Vec<&str> = text.lines().collect();
        let mut result = Vec::new();
        let mut windows = 0;

        for (idx, line) in lines.iter().enumerate() {
            if !line.contains(pattern) {
                continue;
            }
            if windows >= MAX_MATCH_WINDOWS {
                result.push(format!(
                    "  … {} more match window(s) omitted",
                    // Saturating: we stop counting once the cap is hit.
                    lines
                        .iter()
                        .skip(idx)
                        .filter(|line| line.contains(pattern))
                        .count()
                ));
                break;
            }
            windows += 1;
            let start = idx.saturating_sub(context_lines);
            let end = std::cmp::min(idx + context_lines + 1, lines.len());

            for (offset, line) in lines[start..end].iter().enumerate() {
                let i = start + offset;
                let prefix = if i == idx { "→ " } else { "  " };
                result.push(format!("{}{:3}: {}", prefix, i + 1, line));
            }
            result.push(String::new());
        }

        result
    }
}

fn is_word_char(c: char) -> bool {
    if c.is_ascii() {
        return c.is_ascii_alphanumeric() || c == '_';
    }
    // Share the regex mode's Unicode word boundary: combining marks,
    // connector punctuation and non-Latin letters all belong to words.
    static WORD_CHAR: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"\A\w\z").expect("valid word class"));
    WORD_CHAR.is_match(c.encode_utf8(&mut [0; 4]))
}

/// True when `text[start..end]` is bounded by non-word characters (or the ends
/// of the string), i.e. it forms a whole word.
fn is_whole_word(text: &str, start: usize, end: usize) -> bool {
    let before_ok = text[..start]
        .chars()
        .next_back()
        .is_none_or(|c| !is_word_char(c));
    let after_ok = text[end..].chars().next().is_none_or(|c| !is_word_char(c));
    before_ok && after_ok
}

/// If `pattern` matches `text` at byte offset `idx`, returns the byte length of
/// the match within `text`; otherwise `None`. Honors case sensitivity using
/// Unicode-aware case folding.
fn match_len_at(text: &str, idx: usize, pattern: &str, case_sensitive: bool) -> Option<usize> {
    let mut chars = text[idx..].chars();
    let mut consumed = 0;
    for pc in pattern.chars() {
        let tc = chars.next()?;
        let eq = if case_sensitive {
            tc == pc
        } else {
            tc.to_lowercase().eq(pc.to_lowercase())
        };
        if !eq {
            return None;
        }
        consumed += tc.len_utf8();
    }
    Some(consumed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacement_result_limits_apply_before_literal_or_capture_growth() {
        for use_regex in [false, true] {
            let config = SearchConfig {
                use_regex,
                ..SearchConfig::default()
            };
            let all = ReplaceOptions { replace_all: true };
            assert_eq!(
                SearchAndReplaceEngine::search_and_replace_with_limit(
                    "aa", "a", "界", &config, &all, 6
                )
                .unwrap(),
                ("界界".into(), 2)
            );
            assert!(SearchAndReplaceEngine::search_and_replace_with_limit(
                "aa", "a", "界", &config, &all, 5
            )
            .is_err());
            assert_eq!(
                SearchAndReplaceEngine::search_and_replace_with_limit(
                    "aa",
                    "a",
                    "界",
                    &config,
                    &ReplaceOptions::default(),
                    4
                )
                .unwrap(),
                ("界a".into(), 1)
            );
            assert!(SearchAndReplaceEngine::search_and_replace_with_limit(
                "unchanged",
                "z",
                "x",
                &config,
                &all,
                8
            )
            .is_err());
            assert!(SearchAndReplaceEngine::search_and_replace_with_limit(
                "unchanged",
                "",
                "x",
                &config,
                &all,
                8
            )
            .is_err());
            assert_eq!(
                SearchAndReplaceEngine::search_and_replace_with_limit(
                    "aaaa", "a", "", &config, &all, 0
                )
                .unwrap(),
                (String::new(), 4)
            );
        }
        let config = SearchConfig {
            use_regex: true,
            ..SearchConfig::default()
        };
        assert!(SearchAndReplaceEngine::search_and_replace_with_limit(
            "abcdefgh",
            "(.*)",
            "$1$1",
            &config,
            &ReplaceOptions::default(),
            15
        )
        .is_err());
        assert_eq!(
            SearchAndReplaceEngine::search_and_replace_with_limit(
                "abcdefgh",
                "(.*)",
                "$1$1",
                &config,
                &ReplaceOptions::default(),
                16
            )
            .unwrap()
            .0,
            "abcdefghabcdefgh"
        );
        assert!(replacement_growth(usize::MAX, 1, usize::MAX).is_err());
    }

    #[test]
    fn exact_capture_preflight_matches_pinned_regex_interpolation() {
        let regex = regex::Regex::new(r"(?P<word>é+)(x)?").unwrap();
        let captures = regex.captures("éé").unwrap();
        for template in [
            "",
            "$",
            "$$",
            "$$$1",
            "$0",
            "$1$1",
            "$2",
            "$missing",
            "$1a",
            "${1}a",
            "${word}",
            "$word",
            "${}",
            "${missing name}",
            "${word",
            "${word}}",
            "$999999999999999999999999999999",
            "${+1}",
            "界$word🙂",
            "$é",
            "${é}",
            "${1}${2}$$",
        ] {
            let mut expanded = String::new();
            captures.expand(template, &mut expanded);
            assert_eq!(
                capture_expansion_len(&captures, template, expanded.len()).unwrap(),
                expanded.len(),
                "{template:?}"
            );
            if !expanded.is_empty() {
                assert!(
                    capture_expansion_len(&captures, template, expanded.len() - 1).is_err(),
                    "{template:?}"
                );
            }
        }
    }

    #[test]
    fn bounded_regex_preserves_zero_width_unicode_and_first_all_results() {
        for pattern in ["^", "$", "a*", "(?P<letter>.)", "(é)?", "z"] {
            let regex = regex::Regex::new(pattern).unwrap();
            for text in ["", "éa🙂", "aaa"] {
                for replacement in ["X", "$0$0", "${letter}$$", "${1}", "$unknown", "界"] {
                    for replace_all in [false, true] {
                        let expected = if replace_all {
                            regex.replace_all(text, replacement)
                        } else {
                            regex.replace(text, replacement)
                        }
                        .into_owned();
                        let actual = SearchAndReplaceEngine::search_and_replace_with_limit(
                            text,
                            pattern,
                            replacement,
                            &SearchConfig {
                                use_regex: true,
                                ..SearchConfig::default()
                            },
                            &ReplaceOptions { replace_all },
                            expected.len(),
                        )
                        .unwrap();
                        assert_eq!(
                            actual.0, expected,
                            "{pattern:?} {text:?} {replacement:?} {replace_all}"
                        );
                        let count = if replace_all {
                            regex.find_iter(text).count()
                        } else {
                            usize::from(regex.is_match(text))
                        };
                        assert_eq!(actual.1, count);
                        if !expected.is_empty() {
                            assert!(SearchAndReplaceEngine::search_and_replace_with_limit(
                                text,
                                pattern,
                                replacement,
                                &SearchConfig {
                                    use_regex: true,
                                    ..SearchConfig::default()
                                },
                                &ReplaceOptions { replace_all },
                                expected.len() - 1
                            )
                            .is_err());
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn whole_word_literal_uses_unicode_regex_boundaries() {
        for (text, pattern, expected, count) in [
            ("cafe\u{301} cafe", "cafe", "cafe\u{301} X", 1),
            ("a\u{301}\u{327} a", "a", "a\u{301}\u{327} X", 1),
            (
                "\u{301} \u{301}a a\u{301} \u{301}",
                "\u{301}",
                "X \u{301}a a\u{301} X",
                2,
            ),
            ("界2 界٢ 界 界_ _界", "界", "界2 界٢ X 界_ _界", 1),
            ("٣a ٣ ٣_ _٣", "٣", "٣a X ٣_ _٣", 1),
            ("a_ _ _٣ _界", "_", "a_ X _٣ _界", 1),
            ("a\u{203f} a \u{203f}a", "a", "a\u{203f} X \u{203f}a", 1),
        ] {
            for use_regex in [false, true] {
                let config = SearchConfig {
                    use_regex,
                    whole_word: true,
                    case_sensitive: true,
                    ..Default::default()
                };
                let actual = SearchAndReplaceEngine::search_and_replace(
                    text,
                    pattern,
                    "X",
                    &config,
                    &ReplaceOptions { replace_all: true },
                )
                .unwrap();
                assert_eq!(
                    actual,
                    (expected.to_owned(), count),
                    "text={text:?}, regex={use_regex}"
                );
            }
        }
    }

    #[test]
    fn empty_find_is_noop_in_every_mode() {
        for use_regex in [false, true] {
            for whole_word in [false, true] {
                for replace_all in [false, true] {
                    let config = SearchConfig {
                        use_regex,
                        whole_word,
                        ..Default::default()
                    };
                    let actual = SearchAndReplaceEngine::search_and_replace(
                        "abc",
                        "",
                        "x",
                        &config,
                        &ReplaceOptions { replace_all },
                    )
                    .unwrap();
                    assert_eq!(actual, ("abc".to_owned(), 0));
                }
            }
        }
    }

    #[test]
    fn replacement_does_not_rescan_inserted_text_and_regex_expands_captures() {
        let options = ReplaceOptions { replace_all: true };
        let actual = SearchAndReplaceEngine::search_and_replace(
            "aa",
            "a",
            "aa",
            &SearchConfig::default(),
            &options,
        )
        .unwrap();
        assert_eq!(actual, ("aaaa".to_owned(), 2));
        let config = SearchConfig {
            use_regex: true,
            multi_line: true,
            ..Default::default()
        };
        let actual = SearchAndReplaceEngine::search_and_replace(
            "one\ntwo", r"^(\w+)$", "$1!", &config, &options,
        )
        .unwrap();
        assert_eq!(actual, ("one!\ntwo!".to_owned(), 2));
    }

    #[test]
    fn test_literal_replace() {
        let config = SearchConfig::default();
        let options = ReplaceOptions::default();

        let (result, count) = SearchAndReplaceEngine::search_and_replace(
            "hello world hello",
            "hello",
            "hi",
            &config,
            &options,
        )
        .unwrap();

        assert_eq!(count, 1);
        assert_eq!(result, "hi world hello");
    }

    #[test]
    fn test_replace_all() {
        let config = SearchConfig::default();
        let options = ReplaceOptions { replace_all: true };

        let (result, count) = SearchAndReplaceEngine::search_and_replace(
            "hello world hello",
            "hello",
            "hi",
            &config,
            &options,
        )
        .unwrap();

        assert_eq!(count, 2);
        assert_eq!(result, "hi world hi");
    }

    #[test]
    fn test_whole_word_literal() {
        let config = SearchConfig {
            whole_word: true,
            ..Default::default()
        };
        let options = ReplaceOptions { replace_all: true };

        let (result, count) = SearchAndReplaceEngine::search_and_replace(
            "cat category cat",
            "cat",
            "dog",
            &config,
            &options,
        )
        .unwrap();

        assert_eq!(count, 2);
        assert_eq!(result, "dog category dog");
    }

    #[test]
    fn test_regex_case_insensitive_and_whole_word() {
        let config = SearchConfig {
            use_regex: true,
            case_sensitive: false,
            whole_word: true,
            ..Default::default()
        };
        let options = ReplaceOptions { replace_all: true };

        let (result, count) = SearchAndReplaceEngine::search_and_replace(
            "Err error ERRORS",
            "err",
            "X",
            &config,
            &options,
        )
        .unwrap();

        // "Err" matches (whole word, case-insensitive); "error" and "ERRORS"
        // do not because of the word boundary.
        assert_eq!(count, 1);
        assert_eq!(result, "X error ERRORS");
    }

    #[test]
    fn test_literal_replace_case_insensitive_unicode() {
        let config = SearchConfig {
            case_sensitive: false,
            ..SearchConfig::default()
        };
        let options = ReplaceOptions::default();

        let (result, count) =
            SearchAndReplaceEngine::search_and_replace("İB", "b", "X", &config, &options).unwrap();

        assert_eq!(count, 1);
        assert_eq!(result, "İX");
    }

    #[test]
    fn empty_regex_pattern_is_a_noop() {
        let config = SearchConfig {
            use_regex: true,
            ..Default::default()
        };
        let options = ReplaceOptions { replace_all: true };
        let (result, count) =
            SearchAndReplaceEngine::search_and_replace("abc", "", "x", &config, &options).unwrap();
        assert_eq!(count, 0);
        assert_eq!(result, "abc");
    }

    #[test]
    fn empty_pattern_does_not_preview_every_line() {
        let preview = SearchAndReplaceEngine::get_match_context("a\nb\nc", "", 2);
        assert!(preview.is_empty());
    }

    #[test]
    fn match_context_marks_hit_and_keeps_neighbors() {
        let preview = SearchAndReplaceEngine::get_match_context("alpha\nbeta\ngamma", "beta", 1);
        assert_eq!(
            preview,
            vec![
                "    1: alpha".to_string(),
                "→   2: beta".to_string(),
                "    3: gamma".to_string(),
                String::new(),
            ]
        );
    }

    #[test]
    fn match_context_clamps_window_and_caps_matches() {
        let text = (0..80)
            .map(|i| format!("hit-{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let preview = SearchAndReplaceEngine::get_match_context(&text, "hit-", usize::MAX);
        let arrows = preview.iter().filter(|line| line.starts_with("→ ")).count();
        assert_eq!(arrows, 64);
        assert!(
            preview
                .iter()
                .any(|line| line.contains("more match window(s) omitted")),
            "overflow must be stated rather than silently dropped"
        );
        assert!(
            !preview.iter().any(|line| line.contains("hit-79")),
            "context clamp must not expand each window to the whole file"
        );
    }
}
