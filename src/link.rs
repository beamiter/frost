/// 链接检测和交互模块
use regex::Regex;
use std::path::{Path, PathBuf};

/// Hard cap on actionable highlights produced from one terminal row.
const MAX_LINKS_PER_LINE: usize = 64;

/// Preserve balanced URL parentheses while dropping only excess trailing closers.
/// Count once: rescanning the whole URL after each ')' made link refresh quadratic
/// for process-controlled output with a long run of closing parentheses.
fn trim_unbalanced_url_parentheses(url: &str) -> &str {
    if !url.ends_with(')') {
        return url;
    }
    let (opens, closes) = url.bytes().fold((0usize, 0usize), |(opens, closes), byte| {
        (
            opens + usize::from(byte == b'('),
            closes + usize::from(byte == b')'),
        )
    });
    let excess = closes.saturating_sub(opens);
    let trim = url
        .bytes()
        .rev()
        .take(excess)
        .take_while(|byte| *byte == b')')
        .count();
    &url[..url.len() - trim]
}

/// 链接类型
#[derive(Clone, Debug, Copy, PartialEq, Eq)]
pub enum LinkType {
    /// URL (http/https)
    Url,
    /// 本地文件路径
    FilePath,
    /// IP 地址
    IpAddress,
}

/// 单个链接的信息
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    /// 链接所在行
    pub line: usize,
    /// 列起始位置
    pub col_start: usize,
    /// 列结束位置（不含）
    pub col_end: usize,
    /// 链接类型
    pub link_type: LinkType,
    /// 链接文本/URL
    pub text: String,
}

/// 链接检测配置
#[derive(Clone, Debug)]
pub struct LinkDetectionConfig {
    /// 是否检测 URL
    pub detect_urls: bool,
    /// 是否检测文件路径
    pub detect_file_paths: bool,
    /// 是否检测 IP 地址
    pub detect_ip_addresses: bool,
}

impl Default for LinkDetectionConfig {
    fn default() -> Self {
        Self {
            detect_urls: true,
            detect_file_paths: true,
            detect_ip_addresses: true,
        }
    }
}

/// 链接检测引擎
pub struct LinkDetector {
    config: LinkDetectionConfig,
    url_regex: Regex,
    ip_regex: Regex,
    file_path_regex: Regex,
}

impl LinkDetector {
    pub fn new(config: LinkDetectionConfig) -> Self {
        // Recognize common URL-shaped text first; `is_openable_url` below is
        // still the sole policy deciding which matches become actionable.
        // Parens are allowed in the body (e.g. Wikipedia `/wiki/Foo_(bar)`); an
        // unbalanced trailing `)` is trimmed in code below.
        let url_regex =
            Regex::new(r"[A-Za-z][A-Za-z0-9+.-]*://[^\s<>\[\]{}|\\^`]*[^\s<>\[\]{}|\\^`.,;:!?\-]")
                .unwrap();

        // IP 地址正则：x.x.x.x 格式
        let ip_regex = Regex::new(
            r"\b(?:(?:25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)\.){3}(?:25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)\b"
        ).unwrap();

        // File paths: absolute, ~/..., ./..., ../..., and the relative paths
        // commonly printed by compilers (`src/main.rs:42:7`). A colon is
        // excluded from the first relative component so URL schemes are not
        // reclassified as file paths.
        let file_path_regex = Regex::new(
            r"(?:^|\s)(?:(?:/|\./|\.\./|~/)[^\s<>\[\]{}|\\^`()]*|[^/:\s<>\[\]{}|\\^`()]+(?:/[^/\s<>\[\]{}|\\^`()]+)+)"
        ).unwrap();

        Self {
            config,
            url_regex,
            ip_regex,
            file_path_regex,
        }
    }

    /// Map a byte offset to a display column. Offsets past the line or off a
    /// UTF-8 boundary are skipped instead of slicing-panic.
    fn byte_offset_to_char_offset(line: &str, byte_offset: usize) -> Option<usize> {
        Some(line.get(..byte_offset)?.chars().count())
    }

    fn at_link_cap(links: &[Link]) -> bool {
        links.len() >= MAX_LINKS_PER_LINE
    }

    /// 在单行文本中检测所有链接
    pub fn detect_links_in_line(&self, line: &str, line_idx: usize) -> Vec<Link> {
        let mut links = Vec::new();
        let mut url_spans = Vec::new();

        // Detect every URL-shaped span even when URL activation is disabled:
        // an IP/file-looking substring inside a refused scheme must never fall
        // back to a different link type.
        for mat in self.url_regex.find_iter(line) {
            let url = trim_unbalanced_url_parentheses(mat.as_str());
            let Some(col_start) = Self::byte_offset_to_char_offset(line, mat.start()) else {
                continue;
            };
            let Some(col_end) = Self::byte_offset_to_char_offset(line, mat.start() + url.len())
            else {
                continue;
            };
            if col_end < col_start {
                continue;
            }
            // Reserve the whole URL-shaped span even when policy rejects it.
            // Otherwise `https://user@192.0.2.1` could activate its inner IP.
            url_spans.push((col_start, col_end));
            if self.config.detect_urls && is_openable_url(url) {
                if Self::at_link_cap(&links) {
                    break;
                }
                links.push(Link {
                    line: line_idx,
                    col_start,
                    col_end,
                    link_type: LinkType::Url,
                    text: url.to_string(),
                });
            }
        }

        // 检测 IP 地址
        if self.config.detect_ip_addresses {
            for mat in self.ip_regex.find_iter(line) {
                if Self::at_link_cap(&links) {
                    break;
                }
                let Some(col_start) = Self::byte_offset_to_char_offset(line, mat.start()) else {
                    continue;
                };
                let Some(col_end) = Self::byte_offset_to_char_offset(line, mat.end()) else {
                    continue;
                };
                if col_end < col_start {
                    continue;
                }
                // 避免与 URL 重复
                if !url_spans
                    .iter()
                    .any(|&(start, end)| start <= col_start && col_end <= end)
                    && !links
                        .iter()
                        .any(|l| l.col_start <= col_start && col_end <= l.col_end)
                {
                    links.push(Link {
                        line: line_idx,
                        col_start,
                        col_end,
                        link_type: LinkType::IpAddress,
                        text: mat.as_str().to_string(),
                    });
                }
            }
        }

        // 检测文件路径
        if self.config.detect_file_paths {
            for mat in self.file_path_regex.find_iter(line) {
                if Self::at_link_cap(&links) {
                    break;
                }
                let raw = mat.as_str();
                let matched_text = raw.trim().trim_end_matches([',', ';', '!', '?']);
                if matched_text.is_empty() {
                    continue;
                }
                // The regex captures a leading `^|\s` boundary; skip it so the
                // highlight columns cover only the path, not the preceding space.
                let lead_ws = raw.len() - raw.trim_start().len();
                let start_byte = mat.start() + lead_ws;
                let Some(col_start) = Self::byte_offset_to_char_offset(line, start_byte) else {
                    continue;
                };
                let Some(col_end) =
                    Self::byte_offset_to_char_offset(line, start_byte + matched_text.len())
                else {
                    continue;
                };
                if col_end < col_start {
                    continue;
                }

                // 避免与 URL 重复
                if !url_spans
                    .iter()
                    .any(|&(start, end)| start <= col_start && col_end <= end)
                    && !links
                        .iter()
                        .any(|l| l.col_start <= col_start && col_end <= l.col_end)
                    && Self::is_valid_file_path(matched_text)
                {
                    links.push(Link {
                        line: line_idx,
                        col_start,
                        col_end,
                        link_type: LinkType::FilePath,
                        text: matched_text.to_string(),
                    });
                }
            }
        }

        links
    }

    /// 判断文本是否为有效的文件路径
    fn is_valid_file_path(text: &str) -> bool {
        let trimmed = text.trim();

        if trimmed.is_empty() || matches!(trimmed, "/" | "//" | "./" | "../" | "~/") {
            return false;
        }

        if trimmed.starts_with("//")
            || trimmed.contains("://")
            || trimmed.chars().all(|ch| matches!(ch, '/' | '.'))
            || trimmed.chars().any(|character| {
                character.is_control()
                    || jterm_core::review_input::is_visual_spoofing_character(character)
            })
        {
            return false;
        }

        // Must be rooted explicitly or contain at least one relative separator.
        trimmed.starts_with('/')
            || trimmed.starts_with("./")
            || trimmed.starts_with("../")
            || trimmed.starts_with("~/")
            || trimmed.contains('/')
    }

    /// 在整个网格中检测链接（带缓存）
    #[allow(dead_code)]
    pub fn detect_all_links(&self, grid: &crate::terminal::TerminalGrid) -> Vec<Link> {
        let mut all_links = Vec::new();

        for (line_idx, line) in grid.iter().enumerate() {
            let line_str: String = line.iter().map(|cell| cell.character).collect();
            let links = self.detect_links_in_line(&line_str, line_idx);
            all_links.extend(links);
        }

        all_links
    }

    /// 在当前可视内容中检测链接，处理跨行换行的URL。
    #[allow(dead_code)]
    pub fn detect_links_in_visible_cells(
        &self,
        visible_cells: &[Vec<crate::terminal::TerminalCell>],
    ) -> Vec<Link> {
        self.detect_links_in_visible_cells_with_wrapping(visible_cells, &[])
    }

    /// 在当前可视内容中检测链接，支持传入row_wrapped标志以正确处理跨行链接。
    pub fn detect_links_in_visible_cells_with_wrapping(
        &self,
        visible_cells: &[Vec<crate::terminal::TerminalCell>],
        row_wrapped: &[bool],
    ) -> Vec<Link> {
        let mut all_links = Vec::new();
        let wraps_valid = row_wrapped.len() == visible_cells.len();
        let mut text = String::new();
        // One entry per logical character, not per terminal cell. Its physical
        // extent includes any following wide-character continuation cells.
        let mut columns: Vec<(usize, usize)> = Vec::new();
        let mut rows: Vec<(usize, usize, usize)> = Vec::new();

        for (row, cells) in visible_cells.iter().enumerate() {
            let start = columns.len();
            for (col, cell) in cells.iter().enumerate() {
                if cell.flags.wide_continuation() {
                    if columns.len() > start {
                        columns.last_mut().unwrap().1 = col + 1;
                    }
                    continue;
                }
                text.push(cell.character);
                columns.push((col, col + 1));
            }
            rows.push((row, start, columns.len()));
            // Missing/malformed metadata means separate physical lines. Ordinary
            // spaces, including row-end padding, remain text; never guess a wrap.
            if wraps_valid && row_wrapped[row] && row + 1 < visible_cells.len() {
                continue;
            }
            for link in self.detect_links_in_line(&text, 0) {
                // Start at the first overlapping row instead of rescanning the
                // entire wrapped paragraph for every detected link.
                let first = rows.partition_point(|&(_, _, end)| end <= link.col_start);
                for &(line, start, end) in &rows[first..] {
                    if start >= link.col_end {
                        break;
                    }
                    let from = start.max(link.col_start);
                    let to = end.min(link.col_end);
                    if from < to {
                        all_links.push(Link {
                            line,
                            col_start: columns[from].0,
                            col_end: columns[to - 1].1,
                            link_type: link.link_type,
                            text: link.text.clone(),
                        });
                    }
                }
            }
            text.clear();
            columns.clear();
            rows.clear();
        }
        all_links
    }
}

/// The one policy every clickable target must satisfy.
///
/// Shared with the whole jterm family as `jterm_core::link::is_openable_url`:
/// a link's text comes from the attached process, so a click is the terminal
/// acting on untrusted data. Only an absolute HTTP(S) URL with an authority
/// and no userinfo qualifies; whitespace, controls, backslashes, and visually
/// ambiguous characters are refused so the target reads as the origin it
/// resolves to.
pub(crate) fn is_openable_url(url: &str) -> bool {
    jterm_core::link::is_openable_url(url)
}

/// 打开链接
pub fn open_link(link: &Link, cwd: Option<&Path>) -> Result<(), Box<dyn std::error::Error>> {
    match link.link_type {
        LinkType::Url => {
            open_url(&link.text)?;
        }
        LinkType::FilePath => {
            open_file_path(&link.text, cwd)?;
        }
        LinkType::IpAddress => {
            // IP 地址可以用浏览器打开或显示 whois 信息
            open_url(&format!("http://{}", link.text))?;
        }
    }
    Ok(())
}

/// Absolute openers, in preference order, per platform.
///
/// A clicked link is terminal-controlled data, so the program that receives it
/// must not be chosen by a mutable `PATH`: a directory the user happened to
/// open, or an entry an unrelated install dropped in `~/.local/bin`, would
/// otherwise decide what a click runs.
#[cfg(target_os = "linux")]
const OPENER_CANDIDATES: &[&str] = &["/usr/bin/xdg-open", "/bin/xdg-open"];
#[cfg(target_os = "macos")]
const OPENER_CANDIDATES: &[&str] = &["/usr/bin/open"];
#[cfg(target_os = "windows")]
const OPENER_CANDIDATES: &[&str] = &[r"C:\Windows\explorer.exe"];

fn trusted_opener() -> Result<PathBuf, Box<dyn std::error::Error>> {
    for candidate in OPENER_CANDIDATES {
        let path = Path::new(candidate);
        let Ok(metadata) = std::fs::metadata(path) else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            let mode = metadata.permissions().mode();
            if mode & 0o111 == 0
                || mode & 0o022 != 0
                || (metadata.uid() == unsafe { libc::geteuid() } && mode & 0o200 != 0)
            {
                continue;
            }
        }
        return Ok(path.to_path_buf());
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "no trusted system opener is available",
    )
    .into())
}

/// Spawn the opener detached from this process's streams and reap it, so a
/// clicked link leaves neither a zombie nor a child holding our stdio.
fn spawn_opener(argv: &[&std::ffi::OsStr]) -> Result<(), Box<dyn std::error::Error>> {
    let mut child = std::process::Command::new(trusted_opener()?)
        .args(argv)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// 打开 URL（使用系统默认浏览器）
fn open_url(url: &str) -> Result<(), Box<dyn std::error::Error>> {
    if !is_openable_url(url) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "unsupported or unsafe URL",
        )
        .into());
    }
    spawn_opener(&[std::ffi::OsStr::new(url)])
}

/// 打开文件路径（使用系统默认应用）
fn open_file_path(path: &str, cwd: Option<&Path>) -> Result<(), Box<dyn std::error::Error>> {
    let expanded_path = resolve_existing_file_path(path, cwd)?;
    let args = file_opener_args(&expanded_path)?;
    let argv: Vec<&std::ffi::OsStr> = args.iter().map(|arg| arg.as_os_str()).collect();
    spawn_opener(&argv)
}

/// xdg-open has no `--` operand separator. An absolute path is already
/// option-safe and keeps relative names such as `-notes.txt` out of its option
/// parser. macOS `open` does accept `--`; preserve that platform's convention.
fn file_opener_args(path: &std::path::Path) -> std::io::Result<Vec<std::ffi::OsString>> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    #[cfg(target_os = "macos")]
    let args = vec![std::ffi::OsString::from("--"), absolute.into_os_string()];
    #[cfg(not(target_os = "macos"))]
    let args = vec![absolute.into_os_string()];
    Ok(args)
}

/// Expand a leading `~/` without invoking a shell.
fn expand_path(path: &str) -> PathBuf {
    if let Some(relative) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(relative);
        }
    }
    PathBuf::from(path)
}

/// Drop a conventional compiler/editor source location (`:line` or
/// `:line:column`) while leaving arbitrary colons alone.
fn strip_source_location(path: &str) -> Option<&str> {
    let (without_last, last) = path.rsplit_once(':')?;
    if last.is_empty() || !last.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    if let Some((without_line, line)) = without_last.rsplit_once(':') {
        if !line.is_empty() && line.bytes().all(|byte| byte.is_ascii_digit()) {
            return Some(without_line);
        }
    }
    Some(without_last)
}

fn absolutize_file_path(path: PathBuf, cwd: Option<&Path>) -> PathBuf {
    if path.is_absolute() {
        path
    } else if let Some(cwd) = cwd {
        cwd.join(path)
    } else {
        path
    }
}

/// Resolve a link exactly as displayed first. If that path does not exist,
/// retry after removing a trailing source location. Relative paths are scoped
/// to the focused terminal's cwd instead of frost's launcher directory.
fn resolve_existing_file_path(
    path: &str,
    cwd: Option<&Path>,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    if path.chars().any(|character| {
        character.is_control() || jterm_core::review_input::is_visual_spoofing_character(character)
    }) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "file link contains unsafe formatting",
        )
        .into());
    }
    let exact = absolutize_file_path(expand_path(path), cwd);
    if exact.exists() {
        return Ok(exact);
    }
    if let Some(source_path) = strip_source_location(path) {
        let source = absolutize_file_path(expand_path(source_path), cwd);
        if source.exists() {
            return Ok(source);
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        format!("file link does not exist: {}", exact.display()),
    )
    .into())
}

#[cfg(test)]
mod tests {

    #[test]
    fn parsed_version_components_are_not_ip_links() {
        let detector = LinkDetector::new(LinkDetectionConfig::default());
        for text in ["1.2.3.4.5", "x.1.2.3.4", "界 1.2.3.4.5"] {
            for width in [80, 8] {
                let (cells, wraps) = parsed_link_cells(text, width);
                let links = detector.detect_links_in_visible_cells_with_wrapping(&cells, &wraps);
                assert!(links.is_empty(), "{text:?} width={width}: {links:?}");
            }
        }
    }

    #[test]
    fn parsed_real_ip_punctuation_and_wrapped_columns_are_preserved() {
        let detector = LinkDetector::new(LinkDetectionConfig::default());
        let (cells, wraps) = parsed_link_cells("界 192.0.2.1.", 80);
        let expected = vec![Link {
            line: 0, col_start: 3, col_end: 12,
            link_type: LinkType::IpAddress, text: "192.0.2.1".into(),
        }];
        for metadata in [&wraps[..], &[][..], &[true][..]] {
            assert_eq!(detector.detect_links_in_visible_cells_with_wrapping(&cells, metadata), expected);
        }
        let (cells, wraps) = parsed_link_cells("界 192.0.2.1.", 8);
        assert_eq!(detector.detect_links_in_visible_cells_with_wrapping(&cells, &wraps), vec![
            Link { line: 0, col_start: 3, col_end: 8, link_type: LinkType::IpAddress, text: "192.0.2.1".into() },
            Link { line: 1, col_start: 0, col_end: 4, link_type: LinkType::IpAddress, text: "192.0.2.1".into() },
        ]);
    }

    use super::*;

    fn parsed_link_cells(
        text: &str,
        cols: usize,
    ) -> (Vec<Vec<crate::terminal::TerminalCell>>, Vec<bool>) {
        let mut terminal = crate::terminal::TerminalState::new(cols, 3);
        terminal.process_input(text.as_bytes());
        let viewport = terminal.get_projected_viewport(true);
        (viewport.cells().to_vec(), viewport.row_wrapped().to_vec())
    }

    #[test]
    fn parsed_visible_links_keep_the_actionable_link_cap() {
        let detector = LinkDetector::new(LinkDetectionConfig::default());
        let input = "https://e.co/x ".repeat(65);
        let (cells, wraps) = parsed_link_cells(&input, 1024);
        for metadata in [&wraps[..], &[][..]] {
            let links = detector.detect_links_in_visible_cells_with_wrapping(&cells, metadata);
            assert_eq!(links.len(), MAX_LINKS_PER_LINE);
            assert!(links.iter().all(|link| link.text == "https://e.co/x"));
        }
    }

    #[test]
    fn parsed_wide_links_keep_targets_and_physical_columns() {
        let detector = LinkDetector::new(LinkDetectionConfig::default());
        for (text, target, start, end, kind) in [
            (
                "https://example.com/界x",
                "https://example.com/界x",
                0,
                23,
                LinkType::Url,
            ),
            (
                "https://example.com/界",
                "https://example.com/界",
                0,
                22,
                LinkType::Url,
            ),
            ("/tmp/界x", "/tmp/界x", 0, 8, LinkType::FilePath),
            (
                "界 https://example.com/x",
                "https://example.com/x",
                3,
                24,
                LinkType::Url,
            ),
        ] {
            let (cells, wraps) = parsed_link_cells(text, 80);
            assert!(cells[0].iter().any(|cell| cell.flags.wide_continuation()));
            let expected = vec![Link {
                line: 0,
                col_start: start,
                col_end: end,
                link_type: kind,
                text: target.into(),
            }];
            for metadata in [&wraps[..], &[][..], &[true][..]] {
                assert_eq!(
                    detector.detect_links_in_visible_cells_with_wrapping(&cells, metadata),
                    expected,
                    "input {text:?}, wrap metadata {metadata:?}",
                );
            }
        }
    }

    #[test]
    fn parsed_wide_links_split_wrapped_cells_without_joining_real_spaces() {
        let detector = LinkDetector::new(LinkDetectionConfig::default());
        let target = "https://example.com/界x";
        // The wide glyph fills the first row exactly; its continuation is a
        // cell, not a space in the target. Each physical segment keeps the URL.
        let (cells, wraps) = parsed_link_cells(target, 22);
        assert!(wraps[0]);
        assert!(cells[0][21].flags.wide_continuation());
        let links = detector.detect_links_in_visible_cells_with_wrapping(&cells, &wraps);
        assert_eq!(
            links,
            vec![
                Link {
                    line: 0,
                    col_start: 0,
                    col_end: 22,
                    link_type: LinkType::Url,
                    text: target.into()
                },
                Link {
                    line: 1,
                    col_start: 0,
                    col_end: 1,
                    link_type: LinkType::Url,
                    text: target.into()
                },
            ]
        );
        let fallback = detector.detect_links_in_visible_cells_with_wrapping(&cells, &[true]);
        assert_eq!(fallback.len(), 1);
        assert_eq!(fallback[0].text, "https://example.com/界");
        assert_eq!(fallback[0].col_end, 22);

        // Ordinary internal and trailing spaces are never trimmed away to
        // manufacture a target across a soft wrap or explicit newline.
        for (input, width) in [
            ("https://example.com/界 x", 80),
            ("https://example.com/界 x", 23),
            ("https://example.com/界 \r\nx", 23),
        ] {
            let (cells, wraps) = parsed_link_cells(input, width);
            let links = detector.detect_links_in_visible_cells_with_wrapping(&cells, &wraps);
            assert_eq!(links.len(), 1, "{input:?}");
            assert_eq!(links[0].text, "https://example.com/界");
            assert_eq!(
                (links[0].line, links[0].col_start, links[0].col_end),
                (0, 0, 22)
            );
        }
    }

    #[test]
    fn parenthesis_trimming_matches_legacy_for_all_short_inputs() {
        fn legacy(mut url: &str) -> &str {
            while url.ends_with(')') && url.matches(')').count() > url.matches('(').count() {
                url = &url[..url.len() - 1];
            }
            url
        }
        // Include multi-byte text so byte trimming never splits a codepoint.
        let alphabet = ['(', ')', '雪', 'a'];
        for len in 0..=8 {
            for mut encoded in 0..4usize.pow(len) {
                let mut input = String::from("https://example.com/");
                for _ in 0..len {
                    input.push(alphabet[encoded % 4]);
                    encoded /= 4;
                }
                assert_eq!(trim_unbalanced_url_parentheses(&input), legacy(&input));
            }
        }
    }

    #[test]
    fn long_trailing_parentheses_keep_the_balanced_url_and_columns() {
        let detector = LinkDetector::new(LinkDetectionConfig::default());
        let target = "https://example.com/wiki/Foo_(雪)";
        let line = format!("雪 {target}{}", ")".repeat(100_000));
        let links = detector.detect_links_in_line(&line, 7);
        assert_eq!(
            links,
            vec![Link {
                line: 7,
                col_start: 2,
                col_end: 2 + target.chars().count(),
                link_type: LinkType::Url,
                text: target.into(),
            }]
        );
    }

    #[test]
    fn wrapped_unicode_rows_keep_exact_link_cells_and_logical_boundaries() {
        let detector = LinkDetector::new(LinkDetectionConfig::default());
        let rows = ["雪 https://", "example.com/", "Foo_(雪)))", "192.0.2.1"];
        let cells: Vec<Vec<crate::terminal::TerminalCell>> = rows
            .iter()
            .map(|row| {
                row.chars()
                    .map(|character| crate::terminal::TerminalCell {
                        character,
                        ..Default::default()
                    })
                    .collect()
            })
            .collect();
        let links = detector
            .detect_links_in_visible_cells_with_wrapping(&cells, &[true, true, false, false]);
        let target = "https://example.com/Foo_(雪)";
        assert_eq!(
            links,
            vec![
                Link {
                    line: 0,
                    col_start: 2,
                    col_end: 10,
                    link_type: LinkType::Url,
                    text: target.into()
                },
                Link {
                    line: 1,
                    col_start: 0,
                    col_end: 12,
                    link_type: LinkType::Url,
                    text: target.into()
                },
                Link {
                    line: 2,
                    col_start: 0,
                    col_end: 7,
                    link_type: LinkType::Url,
                    text: target.into()
                },
                Link {
                    line: 3,
                    col_start: 0,
                    col_end: 9,
                    link_type: LinkType::IpAddress,
                    text: "192.0.2.1".into()
                },
            ]
        );
        // An incomplete wrap map must retain the existing per-row fallback.
        assert_eq!(
            detector.detect_links_in_visible_cells_with_wrapping(&cells, &[true]),
            detector.detect_links_in_visible_cells(&cells),
        );
    }

    #[test]
    fn file_opener_uses_an_absolute_option_safe_operand() {
        let args = file_opener_args(std::path::Path::new("-notes.txt")).unwrap();
        let expected = std::env::current_dir().unwrap().join("-notes.txt");
        #[cfg(target_os = "macos")]
        assert_eq!(
            args,
            [std::ffi::OsString::from("--"), expected.into_os_string()]
        );
        #[cfg(not(target_os = "macos"))]
        assert_eq!(args, [expected.into_os_string()]);
    }

    #[test]
    fn file_opener_keeps_absolute_paths_as_one_exact_operand() {
        let path = std::env::temp_dir().join("file with spaces;$(not-a-command).txt");
        let args = file_opener_args(&path).unwrap();
        assert_eq!(args.last().unwrap(), path.as_os_str());
        #[cfg(target_os = "linux")]
        assert_eq!(args.len(), 1, "xdg-open rejects a separate -- operand");
    }

    #[cfg(unix)]
    #[test]
    fn file_opener_preserves_non_utf8_path_bytes() {
        use std::os::unix::ffi::OsStringExt;
        let path =
            std::env::temp_dir().join(std::ffi::OsString::from_vec(b"notes-\xff.txt".to_vec()));
        assert_eq!(
            file_opener_args(&path).unwrap().last().unwrap(),
            path.as_os_str()
        );
    }

    #[test]
    fn test_url_detection() {
        let detector = LinkDetector::new(LinkDetectionConfig::default());
        let line = "Visit https://example.com for more info";
        let links = detector.detect_links_in_line(line, 0);

        assert_eq!(links.len(), 1);
        assert_eq!(links[0].link_type, LinkType::Url);
        assert_eq!(links[0].text, "https://example.com");
    }

    #[test]
    fn test_ip_detection() {
        let detector = LinkDetector::new(LinkDetectionConfig::default());
        let line = "Server at 192.168.1.1 is down";
        let links = detector.detect_links_in_line(line, 0);

        assert!(links.iter().any(|l| l.link_type == LinkType::IpAddress));
    }

    #[test]
    fn test_file_path_detection() {
        let detector = LinkDetector::new(LinkDetectionConfig::default());
        let line = "Check /etc/hosts file";
        let links = detector.detect_links_in_line(line, 0);

        assert!(links.iter().any(|l| l.link_type == LinkType::FilePath));
    }

    #[test]
    fn file_path_detection_skips_visual_spoofing() {
        let detector = LinkDetector::new(LinkDetectionConfig::default());
        let links = detector.detect_links_in_line("Check /tmp/\u{202e}hidden.rs later", 0);
        assert!(
            !links
                .iter()
                .any(|link| link.link_type == LinkType::FilePath),
            "bidi file paths must not become clickable"
        );
        let zwsp = detector.detect_links_in_line("open src/\u{200b}main.rs", 0);
        assert!(!zwsp.iter().any(|link| link.link_type == LinkType::FilePath));
        let safe = detector.detect_links_in_line("Check /etc/hosts file", 0);
        assert!(safe.iter().any(|link| link.link_type == LinkType::FilePath));
    }

    #[test]
    fn detects_relative_and_home_file_paths_without_reclassifying_urls() {
        let detector = LinkDetector::new(LinkDetectionConfig::default());
        let line = "at src/main.rs:42:7 and ~/notes/todo.md; see https://example.com/a/b";
        let links = detector.detect_links_in_line(line, 0);
        let files: Vec<&str> = links
            .iter()
            .filter(|link| link.link_type == LinkType::FilePath)
            .map(|link| link.text.as_str())
            .collect();

        assert_eq!(files, ["src/main.rs:42:7", "~/notes/todo.md"]);
        assert_eq!(
            links
                .iter()
                .filter(|link| link.link_type == LinkType::Url)
                .count(),
            1
        );
    }

    #[test]
    fn source_locations_resolve_against_the_terminal_cwd() {
        let unique = format!(
            "frost-link-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock should be after the Unix epoch")
                .as_nanos()
        );
        let root = std::env::temp_dir().join(unique);
        let source_dir = root.join("src");
        std::fs::create_dir_all(&source_dir).expect("create test source directory");
        let source = source_dir.join("main.rs");
        std::fs::write(&source, "fn main() {}\n").expect("write test source");

        let resolved = resolve_existing_file_path("src/main.rs:42:7", Some(&root))
            .expect("source location should resolve");
        assert_eq!(resolved, source);
        assert_eq!(strip_source_location("notes:today"), None);
        assert_eq!(strip_source_location("src/main.rs:42"), Some("src/main.rs"));

        std::fs::remove_dir_all(root).expect("remove test directory");
    }

    #[test]
    fn file_links_with_visual_spoofing_are_not_opened() {
        let unique = format!(
            "frost-link-spoof-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock should be after the Unix epoch")
                .as_nanos()
        );
        let root = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(&root).expect("create test directory");
        let spoofed = root.join("ok\u{202e}hidden.rs");
        std::fs::write(&spoofed, "fn main() {}\n").expect("write spoofed name");
        let err = resolve_existing_file_path(spoofed.to_str().expect("utf-8"), None)
            .expect_err("spoofed file names must not open");
        assert_eq!(
            err.downcast_ref::<std::io::Error>()
                .expect("io error")
                .kind(),
            std::io::ErrorKind::InvalidInput
        );
        assert!(resolve_existing_file_path("src/\u{200b}main.rs", Some(&root)).is_err());
        std::fs::remove_dir_all(root).expect("remove test directory");
    }

    #[test]
    fn test_comment_slashes_are_not_file_paths() {
        let detector = LinkDetector::new(LinkDetectionConfig::default());
        let line = "// Selection rule:";
        let links = detector.detect_links_in_line(line, 0);

        assert!(!links.iter().any(|l| l.link_type == LinkType::FilePath));
    }

    #[test]
    fn test_link_detection_config() {
        let config = LinkDetectionConfig {
            detect_urls: false,
            ..LinkDetectionConfig::default()
        };

        let detector = LinkDetector::new(config);
        let line = "Visit https://192.0.2.1/a for more info";
        let links = detector.detect_links_in_line(line, 0);

        assert!(!links.iter().any(|l| l.link_type == LinkType::Url));
        assert!(links.is_empty(), "inner IP must not bypass URL policy");
    }

    #[test]
    fn unsafe_url_shapes_never_fall_back_to_inner_ip_links() {
        let detector = LinkDetector::new(LinkDetectionConfig::default());
        for text in [
            "ftp://192.0.2.1/archive",
            "https://user:token@192.0.2.1/private",
            "file://192.0.2.1/etc/passwd",
        ] {
            assert!(
                detector.detect_links_in_line(text, 0).is_empty(),
                "unsafe enclosing URL became actionable: {text}"
            );
        }
    }

    #[test]
    fn openable_url_policy_is_applied_during_detection_and_opening() {
        assert!(is_openable_url("https://example.com/a?b=c#d"));
        assert!(is_openable_url("HTTP://example.com"));
        assert!(!is_openable_url("ftp://example.com/archive"));
        assert!(!is_openable_url("https://user@example.com/private"));
        assert!(!is_openable_url("https://example.com/line\nbreak"));
        assert!(!is_openable_url("https://example.com\\evil"));

        let detector = LinkDetector::new(LinkDetectionConfig::default());
        let links = detector
            .detect_links_in_line("ok https://example.com bad ftp://example.com/archive", 0);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].text, "https://example.com");
    }

    #[test]
    fn mid_codepoint_offsets_are_skipped_instead_of_panicking() {
        assert_eq!(LinkDetector::byte_offset_to_char_offset("雪", 1), None);
        assert_eq!(LinkDetector::byte_offset_to_char_offset("雪", 3), Some(1));
        assert_eq!(LinkDetector::byte_offset_to_char_offset("雪", 99), None);
        assert_eq!(LinkDetector::byte_offset_to_char_offset("ok", 0), Some(0));
    }

    #[test]
    fn one_row_keeps_at_most_sixty_four_actionable_links() {
        let detector = LinkDetector::new(LinkDetectionConfig::default());
        let line = (0..80)
            .map(|i| format!("https://example.com/{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        let links = detector.detect_links_in_line(&line, 0);
        assert_eq!(links.len(), MAX_LINKS_PER_LINE);
        assert!(links.iter().all(|link| link.link_type == LinkType::Url));
    }
}
