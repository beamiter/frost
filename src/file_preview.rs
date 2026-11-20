//! Read-only, bounded local previews. Never invoke an opener, shell or web renderer.
use crate::preview_navigation::{match_background, ImageZoom, SourceMatches};
use crate::{native_shell::InspectorTab, sidebar, Frost, Message, ToastKind};
use iced::widget::{
    button, checkbox, column, container, pick_list, rich_text, row, scrollable, span, text,
    text_input, Space,
};
use iced::{Element, Length, Task};
use std::io::Read;
use std::path::{Component, Path, PathBuf};

const BYTE_LIMIT: u64 = 2 * 1024 * 1024;
const PIXEL_LIMIT: u64 = 4 * 1024 * 1024;

#[derive(Debug, Clone)]
pub(crate) enum Content {
    Text(String),
    Image {
        handle: iced::widget::image::Handle,
        width: u32,
        height: u32,
    },
}
#[derive(Debug, Clone)]
pub(crate) struct Preview {
    pub path: PathBuf,
    pub content: Content,
    pub location: Option<(usize, Option<usize>)>,
    lines: Vec<usize>,
}

/// Source coordinates are one-based. Invalid/overflowing coordinates never
/// influence path resolution or allocate a padding buffer.
fn source_location(path: &str) -> Option<(&str, usize, Option<usize>)> {
    let (prefix, last) = path.rsplit_once(':')?;
    let positive = |value: &str| {
        if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        value.parse::<usize>().ok().filter(|value| *value > 0)
    };
    let last = positive(last)?;
    match prefix.rsplit_once(':') {
        Some((path, line)) if line.bytes().all(|b| b.is_ascii_digit()) => {
            Some((path, positive(line)?, Some(last)))
        }
        _ => Some((prefix, last, None)),
    }
}

fn source_line<'a>(source: &'a str, offsets: &[usize], index: usize) -> &'a str {
    let start = offsets[index];
    let end = offsets.get(index + 1).copied().unwrap_or(source.len());
    let line = &source[start..end];
    match line.strip_suffix('\n') {
        Some(line) => line.strip_suffix('\r').unwrap_or(line),
        None => line,
    }
}

/// Match only the frozen, still-live request, never just the current filename.
pub(crate) fn accepts_request(
    expected: Option<(usize, u64)>,
    actual: (usize, u64),
    active: Option<usize>,
) -> bool {
    expected == Some(actual) && active == Some(actual.0)
}

fn relative_path(path: &str, cwd: &Path) -> Result<PathBuf, String> {
    if path.len() > 4096 || cwd.as_os_str().len() > 4096 {
        return Err("Preview path is too long".into());
    }
    if !cwd.is_absolute()
        || cwd.to_str().is_none_or(|value| {
            value.chars().any(|c| {
                c.is_control() || jterm_core::review_input::is_visual_spoofing_character(c)
            })
        })
        || path
            .chars()
            .any(|c| c.is_control() || jterm_core::review_input::is_visual_spoofing_character(c))
    {
        return Err("A safe absolute terminal working directory is required".into());
    }
    let path = Path::new(path);
    let path = if path.is_absolute() {
        path.strip_prefix(cwd)
            .map_err(|_| "Preview is limited to the terminal working directory")?
    } else {
        path
    };
    let mut result = PathBuf::new();
    for part in path.components() {
        match part {
            Component::Normal(s) => result.push(s),
            Component::CurDir => {}
            Component::ParentDir if result.pop() => {}
            _ => return Err("Preview path leaves the terminal working directory".into()),
        }
    }
    if result.as_os_str().is_empty() {
        return Err("Choose a regular file".into());
    }
    Ok(result)
}

#[derive(Debug)]
struct OpenError {
    message: String,
    missing_final_component: bool,
}
impl OpenError {
    fn rejected(error: impl std::fmt::Display) -> Self {
        Self {
            message: error.to_string(),
            missing_final_component: false,
        }
    }
    #[cfg(target_os = "linux")]
    fn component(error: std::io::Error, last: bool) -> Self {
        Self {
            missing_final_component: last && error.kind() == std::io::ErrorKind::NotFound,
            message: error.to_string(),
        }
    }
}

// Walk from a pinned directory descriptor. Refuse symlinks at every component,
// including races after validation; O_NONBLOCK prevents FIFO/device open hangs.
#[cfg(target_os = "linux")]
fn open_regular(cwd: &Path, relative: &Path) -> Result<std::fs::File, OpenError> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::OpenOptionsExt;
    let mut dir = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC)
        .open(cwd)
        .map_err(OpenError::rejected)?;
    let components: Vec<_> = relative.components().collect();
    for (index, component) in components.iter().enumerate() {
        let name = CString::new(component.as_os_str().as_bytes()).map_err(OpenError::rejected)?;
        let last = index + 1 == components.len();
        let flags = (if last { libc::O_PATH } else { libc::O_RDONLY })
            | libc::O_CLOEXEC
            | libc::O_NOFOLLOW
            | libc::O_NONBLOCK
            | if last { 0 } else { libc::O_DIRECTORY };
        // SAFETY: name is NUL-terminated; dir owns a valid descriptor. Successful
        // openat returns a new descriptor whose sole owner is the File below.
        let fd = unsafe { libc::openat(dir.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            return Err(OpenError::component(std::io::Error::last_os_error(), last));
        }
        let file = unsafe { std::fs::File::from_raw_fd(fd) };
        if last {
            if !file.metadata().map_err(OpenError::rejected)?.is_file() {
                return Err(OpenError::rejected("Only regular files can be previewed"));
            }
            // Reopen the checked, pinned inode, not the attacker-controlled path.
            // O_PATH above never opens a device/FIFO for I/O.
            return std::fs::File::open(format!("/proc/self/fd/{}", file.as_raw_fd()))
                .map_err(OpenError::rejected);
        }
        dir = file;
    }
    Err(OpenError::rejected("Choose a regular file"))
}
#[cfg(not(target_os = "linux"))]
fn open_regular(_: &Path, _: &Path) -> Result<std::fs::File, OpenError> {
    Err(OpenError::rejected(
        "Safe local preview is not supported on this platform",
    ))
}

#[derive(Clone)]
struct ReadTarget {
    base: PathBuf,
    relative: PathBuf,
    fallback: Option<(PathBuf, usize, Option<usize>)>,
}

impl ReadTarget {
    fn reference(path: &str, cwd: &Path) -> Result<Self, String> {
        let relative = relative_path(path, cwd)?;
        let fallback = source_location(path).and_then(|(path, line, column)| {
            relative_path(path, cwd)
                .ok()
                .map(|path| (path, line, column))
        });
        Ok(Self {
            base: cwd.into(),
            relative,
            fallback,
        })
    }

    fn tree_file(path: &Path) -> Result<Self, String> {
        let relative = path
            .strip_prefix("/")
            .map_err(|_| "Choose an absolute local file")?;
        if relative.as_os_str().is_empty()
            || relative
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err("Choose an absolute local file without parent components".into());
        }
        Ok(Self {
            base: PathBuf::from("/"),
            relative: relative.to_path_buf(),
            fallback: None,
        })
    }

    fn load(self) -> Result<Preview, String> {
        let Self {
            base,
            mut relative,
            fallback,
        } = self;
        let mut location = None;
        let file = match open_regular(&base, &relative) {
            Ok(file) => file,
            Err(original) => {
                // Only a missing final component permits the disclosed fallback.
                let Some((path, line, column)) =
                    fallback.filter(|_| original.missing_final_component)
                else {
                    return Err(original.message);
                };
                relative = path;
                location = Some((line, column));
                open_regular(&base, &relative).map_err(|error| error.message)?
            }
        };
        read_preview(file, base.join(relative), location)
    }
}

#[cfg(test)]
pub(crate) fn load(path: &str, cwd: &Path) -> Result<Preview, String> {
    ReadTarget::reference(path, cwd)?.load()
}

fn read_preview(
    mut file: std::fs::File,
    path: PathBuf,
    location: Option<(usize, Option<usize>)>,
) -> Result<Preview, String> {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("");
    let image = ["png", "jpg", "jpeg"]
        .iter()
        .any(|expected| extension.eq_ignore_ascii_case(expected));
    let (limit, size_error) = if image {
        (BYTE_LIMIT, "PNG/JPEG preview exceeds the 2 MiB limit")
    } else {
        (64 * 1024, "Text preview exceeds the 64 KiB limit")
    };
    let length = file.metadata().map_err(|error| error.to_string())?.len();
    if length > limit {
        return Err(size_error.into());
    }
    let mut bytes = Vec::with_capacity(length as usize);
    file.by_ref()
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > limit {
        return Err(size_error.into());
    }
    let content = if image {
        let mut reader = image::ImageReader::new(std::io::Cursor::new(&bytes))
            .with_guessed_format()
            .map_err(|e| e.to_string())?;
        if !matches!(
            reader.format(),
            Some(image::ImageFormat::Png | image::ImageFormat::Jpeg)
        ) {
            return Err("Only PNG and JPEG image previews are supported".into());
        }
        let (width, height) = reader.into_dimensions().map_err(|e| e.to_string())?;
        if width == 0 || height == 0 || width > 4096 || height > 4096 {
            return Err("Image preview requires nonzero edges up to 4096 pixels".into());
        }
        if u64::from(width) * u64::from(height) > PIXEL_LIMIT {
            return Err("Image exceeds the 4 megapixel limit".into());
        }
        reader = image::ImageReader::new(std::io::Cursor::new(&bytes))
            .with_guessed_format()
            .map_err(|e| e.to_string())?;
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(4096);
        limits.max_image_height = Some(4096);
        limits.max_alloc = Some(32 * 1024 * 1024);
        reader.limits(limits);
        let rgba = reader.decode().map_err(|e| e.to_string())?.into_rgba8();
        if rgba.dimensions() != (width, height) {
            return Err("Image decoder reported inconsistent dimensions".into());
        }
        Content::Image {
            handle: iced::widget::image::Handle::from_rgba(width, height, rgba.into_raw()),
            width,
            height,
        }
    } else {
        let text =
            String::from_utf8(bytes).map_err(|_| "File is not UTF-8 text or a supported image")?;
        // A single initial UTF-8 signature identifies the text encoding, not
        // visible content. Do not strip embedded/repeated BOMs or normalize paths.
        let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
        if text.chars().any(|c| {
            !matches!(c, '\n' | '\r' | '\t')
                && (c.is_control() || jterm_core::review_input::is_visual_spoofing_character(c))
        }) {
            return Err("Text contains unsafe control or formatting characters".into());
        }
        Content::Text(text.to_owned())
    };
    let mut lines = Vec::new();
    if let Content::Text(value) = &content {
        lines.push(0);
        lines.extend(value.match_indices('\n').map(|(offset, _)| offset + 1));
    }
    Ok(Preview {
        path,
        content,
        location,
        lines,
    })
}

#[derive(Debug, Clone)]
pub(crate) enum Action {
    Confirm,
    Close,
    Reload,
    CopyPath,
    CopyText,
    CopyLine,
    EditLine(String),
    Jump,
    ToggleFind,
    EditFind(String),
    SetMatchCase(bool),
    FindStep {
        previous: bool,
    },
    Scrolled {
        x: f32,
        y: f32,
        viewport_height: f32,
    },
    SetZoom(ImageZoom),
}

enum Phase {
    Confirm,
    Reading,
    Failed(Option<String>),
    Ready(Preview),
}

struct Page {
    session: usize,
    token: u64,
    view_token: u64,
    target: ReadTarget,
    phase: Phase,
    notice: Option<String>,
    line_input: String,
    selected_line: Option<usize>,
    reported_column: Option<usize>,
    find_open: bool,
    find_query: String,
    match_case: bool,
    matches: SourceMatches,
    scroll_x: f32,
    scroll_y: f32,
    viewport_height: f32,
    zoom: ImageZoom,
}

impl Page {
    fn selected_line_text(&self) -> Option<&str> {
        let Phase::Ready(Preview {
            content: Content::Text(text),
            lines,
            ..
        }) = &self.phase
        else {
            return None;
        };
        if text.is_empty() {
            return None;
        }
        let index = self.selected_line?.checked_sub(1)?;
        lines.get(index)?;
        Some(source_line(text, lines, index))
    }

    fn update_matches(&mut self) {
        self.matches.clear_active();
        if let Phase::Ready(Preview {
            content: Content::Text(text),
            ..
        }) = &self.phase
        {
            self.matches.update(text, &self.find_query, self.match_case);
        }
    }

    fn match_prefix(&self) -> Option<String> {
        let hit = self.find_open.then(|| self.matches.active()).flatten()?;
        if self.selected_line != Some(hit.line) {
            return None;
        }
        let Phase::Ready(preview) = &self.phase else {
            return None;
        };
        let digits = preview.lines.len().to_string().len();
        let prefix = self.selected_line_text()?.get(..hit.start)?;
        Some(format!("{:>digits$}  {prefix}", hit.line))
    }

    fn select_line(&mut self, line: usize) {
        self.selected_line = Some(line);
        self.reported_column = None;
        self.line_input = line.to_string();
        self.notice = None;
    }

    /// Publish a new snapshot after the worker finishes. A failed reload leaves
    /// the existing content, scroll position and search results usable.
    fn finish_read(&mut self, result: Result<Preview, String>) -> bool {
        let preview = match result {
            Ok(preview) => preview,
            Err(error) => {
                if matches!(self.phase, Phase::Ready(_)) {
                    self.notice =
                        Some(format!("Reload failed. Showing the last preview.\n{error}"));
                } else {
                    self.phase = Phase::Failed(Some(error));
                }
                return false;
            }
        };
        if let Ok(relative) = preview.path.strip_prefix(&self.target.base) {
            self.target.relative = relative.to_path_buf();
            self.target.fallback = None;
        }
        if let Some((line, column)) = preview.location {
            self.select_line(line);
            self.reported_column = column;
        }
        match &preview.content {
            Content::Text(text) => self.matches.update(text, &self.find_query, self.match_case),
            Content::Image { .. } => self.matches = SourceMatches::default(),
        }
        let reveal = preview.location.is_some();
        self.phase = Phase::Ready(preview);
        reveal
    }
}

#[derive(Default)]
pub(crate) struct Panel {
    page: Option<Page>,
    reader: Option<(usize, u64)>,
    next_token: u64,
}

impl Panel {
    pub(crate) fn is_visible_for(&self, active: Option<usize>) -> bool {
        self.page
            .as_ref()
            .is_some_and(|page| Some(page.session) == active)
    }

    fn accept_completion(
        &mut self,
        session: usize,
        token: u64,
        active: Option<usize>,
    ) -> Option<&mut Page> {
        if self.reader != Some((session, token)) {
            return None;
        }
        self.reader = None;
        self.page.as_mut().filter(|page| {
            accepts_request(Some((page.session, page.token)), (session, token), active)
        })
    }

    pub(crate) fn close(&mut self) {
        // The reading slot remains occupied until its blocking task returns.
        self.page = None;
    }
}

fn display_path(path: &Path) -> String {
    let value = path
        .to_str()
        .map(str::to_owned)
        .unwrap_or_else(|| format!("{:?}", path.as_os_str()));
    jterm_core::review_input::safe_inline_display(&value, 32 * 1024)
}

fn location_label(line: usize, column: Option<usize>) -> String {
    match column {
        Some(column) => format!("Line {line}, column {column} (reported)"),
        None => format!("Line {line}"),
    }
}

fn source_scroll_id(token: u64) -> iced::widget::Id {
    iced::widget::Id::from(format!("local-file-preview-source-{token}"))
}

fn image_scroll_id(token: u64, zoom: ImageZoom) -> iced::widget::Id {
    iced::widget::Id::from(format!("local-file-preview-image-{token}-{zoom}"))
}

fn find_input_id(token: u64) -> iced::widget::Id {
    iced::widget::Id::from(format!("local-file-preview-find-{token}"))
}

fn both_axes() -> scrollable::Direction {
    scrollable::Direction::Both {
        vertical: scrollable::Scrollbar::default(),
        horizontal: scrollable::Scrollbar::default(),
    }
}

impl Frost {
    pub(crate) fn begin_file_preview(&mut self, session_id: usize, path: &str) -> Task<Message> {
        let Some(session) = self
            .sessions
            .get(self.active)
            .filter(|s| s.id == session_id)
        else {
            return Task::none();
        };
        if session.managed_remote
            || session.observed_ssh_command().is_some()
            || session.transcript_read_only()
        {
            self.push_toast(
                "Local preview is unavailable for remote or restored output",
                ToastKind::Warning,
            );
            return Task::none();
        }
        let Some(cwd) = session.cwd_cache.clone().or_else(|| session.cwd()) else {
            self.push_toast(
                "Terminal working directory is required for local preview",
                ToastKind::Warning,
            );
            return Task::none();
        };
        self.present_file_preview(
            session_id,
            ReadTarget::reference(path, Path::new(&cwd)),
            true,
        )
    }

    pub(crate) fn preview_file_tree_path(&mut self, path: &Path) -> Task<Message> {
        let Some(session) = self.sessions.get(self.active) else {
            return Task::none();
        };
        self.present_file_preview(session.id, ReadTarget::tree_file(path), false)
    }

    fn present_file_preview(
        &mut self,
        session_id: usize,
        target: Result<ReadTarget, String>,
        confirm: bool,
    ) -> Task<Message> {
        let target = match target {
            Ok(target) => target,
            Err(error) => {
                self.push_toast(error, ToastKind::Warning);
                return Task::none();
            }
        };
        let Some(token) = self.file_preview.next_token.checked_add(1) else {
            return Task::none();
        };
        self.file_preview.next_token = token;
        self.file_preview.page = Some(Page {
            session: session_id,
            token,
            view_token: token,
            target,
            phase: if confirm {
                Phase::Confirm
            } else {
                Phase::Failed(None)
            },
            notice: None,
            line_input: String::new(),
            selected_line: None,
            reported_column: None,
            find_open: false,
            find_query: String::new(),
            match_case: true,
            matches: SourceMatches::default(),
            scroll_x: 0.0,
            scroll_y: 0.0,
            viewport_height: (self.win_size.height - 240.0).max(80.0),
            zoom: ImageZoom::Fit,
        });
        self.reference_inspector_tab = InspectorTab::Files;
        self.reference_navigation_drawer = false;
        self.reference_inspector_open = true;
        self.reference_inspector_drawer = true;
        self.sidebar_files_focused = false;
        self.sync_reference_inspector_geometry();
        if confirm {
            Task::none()
        } else {
            self.start_file_preview_read()
        }
    }

    fn start_file_preview_read(&mut self) -> Task<Message> {
        let Some(page) = &mut self.file_preview.page else {
            return Task::none();
        };
        if self.file_preview.reader.is_some() {
            page.notice = Some("Another preview is still reading. Try again shortly.".into());
            return Task::none();
        }
        let Some(token) = self.file_preview.next_token.checked_add(1) else {
            return Task::none();
        };
        self.file_preview.next_token = token;
        let session = page.session;
        let target = page.target.clone();
        page.token = token;
        if !matches!(page.phase, Phase::Ready(_)) {
            page.phase = Phase::Reading;
        }
        page.notice = None;
        self.file_preview.reader = Some((session, token));
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || target.load())
                    .await
                    .unwrap_or_else(|_| Err("File preview worker failed".into()))
            },
            move |result| Message::FilePreviewLoaded(session, token, result),
        )
    }

    pub(crate) fn file_preview_loaded(
        &mut self,
        session: usize,
        token: u64,
        result: Result<Preview, String>,
    ) -> Task<Message> {
        let active = self.sessions.get(self.active).map(|session| session.id);
        let Some(page) = self.file_preview.accept_completion(session, token, active) else {
            return Task::none();
        };
        if page.finish_read(result) {
            self.reveal_file_preview_line()
        } else {
            Task::none()
        }
    }

    pub(crate) fn update_file_preview(&mut self, token: u64, action: Action) -> Task<Message> {
        let active = self.sessions.get(self.active);
        let Some(page) = self.file_preview.page.as_mut().filter(|page| {
            page.token == token && Some(page.session) == active.map(|session| session.id)
        }) else {
            return Task::none();
        };
        match action {
            Action::Confirm if matches!(page.phase, Phase::Confirm) => {
                let still_local = active.is_some_and(|session| {
                    !session.managed_remote
                        && !session.transcript_read_only()
                        && session.observed_ssh_command().is_none()
                        && session
                            .cwd_cache
                            .clone()
                            .or_else(|| session.cwd())
                            .as_deref()
                            == page.target.base.to_str()
                });
                if !still_local {
                    page.notice =
                        Some("Terminal context changed. Ctrl-click the file again.".into());
                    return Task::none();
                }
                return self.start_file_preview_read();
            }
            Action::Reload if matches!(page.phase, Phase::Ready(_) | Phase::Failed(_)) => {
                return self.start_file_preview_read();
            }
            Action::Close => self.file_preview.close(),
            Action::CopyPath => {
                if let Phase::Ready(preview) = &page.phase {
                    return iced::clipboard::write(display_path(&preview.path));
                }
            }
            Action::CopyText => {
                if let Phase::Ready(Preview {
                    content: Content::Text(value),
                    ..
                }) = &page.phase
                {
                    return iced::clipboard::write(value.clone());
                }
            }
            Action::CopyLine => {
                if let Some(line) = page.selected_line_text() {
                    return iced::clipboard::write(line.to_owned());
                }
            }
            Action::EditLine(value) => {
                page.line_input = value.chars().take(20).collect();
            }
            Action::Jump => {
                if let Phase::Ready(preview) = &page.phase {
                    let count = preview.lines.len();
                    if let Ok(line) = page.line_input.trim().parse::<usize>() {
                        if (1..=count).contains(&line) {
                            page.select_line(line);
                            page.matches.clear_active();
                            return self.reveal_file_preview_line();
                        }
                    }
                    page.notice = Some(format!("Choose a line from 1 to {count}."));
                }
            }
            Action::ToggleFind => {
                page.find_open = !page.find_open;
                if page.find_open {
                    return iced::widget::operation::focus(find_input_id(page.view_token));
                }
            }
            Action::EditFind(query) => {
                page.find_query = query.chars().take(256).collect();
                page.update_matches();
            }
            Action::SetMatchCase(case) => {
                page.match_case = case;
                page.update_matches();
            }
            Action::FindStep { previous } if page.find_open => {
                if let Some(hit) = page.matches.step(page.selected_line, previous) {
                    page.select_line(hit.line);
                    return self.reveal_file_preview_line();
                }
            }
            Action::Scrolled {
                x,
                y,
                viewport_height,
            } => {
                page.scroll_x = x.max(0.0);
                page.scroll_y = y.max(0.0);
                if viewport_height > 1.0 {
                    page.viewport_height = viewport_height;
                }
            }
            Action::SetZoom(zoom) => page.zoom = zoom,
            _ => {}
        }
        Task::none()
    }

    fn preview_line_height(&self) -> f32 {
        self.chrome_font_size().max(12.0) + 5.0
    }

    fn reveal_file_preview_line(&mut self) -> Task<Message> {
        let row_height = self.preview_line_height();
        let font_size = self.chrome_font_size().max(12.0);
        let Some(page) = &mut self.file_preview.page else {
            return Task::none();
        };
        let Phase::Ready(preview) = &page.phase else {
            return Task::none();
        };
        let count = preview.lines.len();
        let Some(line) = page.selected_line.filter(|line| (1..=count).contains(line)) else {
            return Task::none();
        };
        let stride = row_height + sidebar::SIDEBAR_TREE_ROW_GAP;
        let viewport = page.viewport_height.max(stride);
        let max_offset = (count as f32 * stride - viewport).max(0.0);
        let centered = (line - 1) as f32 * stride - (viewport - stride) / 2.0;
        page.scroll_x = page.match_prefix().map_or(0.0, |prefix| {
            (source_text_width(&prefix, font_size) - 28.0).max(0.0)
        });
        page.scroll_y = centered.clamp(0.0, max_offset);
        self.restore_file_preview_scroll()
    }

    pub(crate) fn restore_file_preview_scroll(&self) -> Task<Message> {
        let Some(page) = self.file_preview.page.as_ref().filter(|page| {
            matches!(
                page.phase,
                Phase::Ready(Preview {
                    content: Content::Text(_),
                    ..
                })
            )
        }) else {
            return Task::none();
        };
        iced::widget::operation::scroll_to(
            source_scroll_id(page.view_token),
            scrollable::AbsoluteOffset {
                x: page.scroll_x,
                y: page.scroll_y,
            },
        )
    }

    pub(crate) fn file_preview_view(&self) -> Element<'_, Message> {
        let active = self.sessions.get(self.active).map(|session| session.id);
        let Some(page) = self
            .file_preview
            .page
            .as_ref()
            .filter(|page| Some(page.session) == active)
        else {
            return self.sidebar_files_view();
        };
        let event = |action| Message::FilePreviewAction(page.token, action);
        let control = |label| {
            button(text(label).size(self.chrome_font_size()))
                .padding([4, 8])
                .style(button::secondary)
        };
        let header = row![
            text("Local file preview").width(Length::Fill),
            control("Close").on_press(event(Action::Close)),
        ]
        .spacing(6)
        .align_y(iced::Alignment::Center);
        let mut body = column![header].spacing(8).padding(10);
        if matches!(page.phase, Phase::Confirm) {
            let exact = page.target.base.join(&page.target.relative);
            let mut disclosure = column![
                text(
                    "Confirm that the terminal reference belongs to this computer before reading."
                ),
                text(display_path(&exact)).font(iced::Font::MONOSPACE),
            ]
            .spacing(8);
            if let Some((file, line, column)) = &page.target.fallback {
                disclosure = disclosure.push(text(format!(
                    "If this filename is missing, read {} ({})",
                    display_path(&page.target.base.join(file)),
                    location_label(*line, *column),
                )));
            }
            disclosure =
                disclosure.push(button("Read local file").on_press(event(Action::Confirm)));
            if let Some(notice) = &page.notice {
                disclosure = disclosure.push(text(notice).style(text::warning));
            }
            return body.push(scrollable(disclosure)).into();
        }
        let path = match &page.phase {
            Phase::Ready(preview) => preview.path.clone(),
            _ => page.target.base.join(&page.target.relative),
        };
        let path_hint = text(sidebar::bound_sidebar_path_label(&path)).size(12);
        let reading = self.file_preview.reader == Some((page.session, page.token));
        let mut details = column![iced::widget::tooltip(
            path_hint,
            text(display_path(&path)).size(12),
            iced::widget::tooltip::Position::Bottom,
        )]
        .spacing(4);
        if reading && matches!(page.phase, Phase::Ready(_)) {
            details = details.push(
                text("Reloading local file…")
                    .size(12)
                    .style(text::secondary),
            );
        }
        if let Some(notice) = &page.notice {
            details = details.push(text(notice).size(12).style(text::warning));
        }
        body = body.push(details);
        match &page.phase {
            Phase::Reading => body.push(text("Reading local file…")).into(),
            Phase::Failed(error) => {
                let body = body.push(control("Retry").on_press(event(Action::Reload)));
                if let Some(error) = error {
                    body.push(text(error).style(text::danger)).into()
                } else {
                    body.into()
                }
            }
            Phase::Ready(preview) => {
                let tools = row![
                    control("Reload").on_press_maybe((!reading).then(|| event(Action::Reload))),
                    control("Copy path").on_press(event(Action::CopyPath)),
                ]
                .spacing(4);
                match &preview.content {
                    Content::Text(value) => {
                        let tools = tools
                            .push(control("Copy text").on_press(event(Action::CopyText)))
                            .push(control("Copy line").on_press_maybe(
                                page.selected_line_text().map(|_| event(Action::CopyLine)),
                            ));
                        body = body.push(tools.wrap());
                        if value.is_empty() {
                            return body.push(text("Empty file")).into();
                        }
                        let line_input = text_input("Line", &page.line_input)
                            .size(self.chrome_font_size())
                            .width(Length::Fixed(78.0))
                            .on_input(move |value| event(Action::EditLine(value)))
                            .on_submit(event(Action::Jump));
                        let navigation = row![
                            line_input,
                            control("Go to line").on_press(event(Action::Jump)),
                            control(if page.find_open { "Hide find" } else { "Find" })
                                .on_press(event(Action::ToggleFind)),
                        ]
                        .spacing(4)
                        .wrap();
                        let mut search = column![navigation].spacing(4);
                        if page.find_open {
                            let available = !page.matches.is_empty();
                            let query = text_input("Find text", &page.find_query)
                                .id(find_input_id(page.view_token))
                                .size(self.chrome_font_size())
                                .width(Length::Fixed(99.0))
                                .on_input(move |value| event(Action::EditFind(value)))
                                .on_submit(event(Action::FindStep { previous: false }));
                            search = search.push(
                                row![
                                    query,
                                    control("Prev").on_press_maybe(
                                        available
                                            .then(|| event(Action::FindStep { previous: true }))
                                    ),
                                    control("Next").on_press_maybe(
                                        available
                                            .then(|| event(Action::FindStep { previous: false }))
                                    ),
                                    checkbox(page.match_case)
                                        .label("Match case")
                                        .on_toggle(move |value| event(Action::SetMatchCase(value))),
                                ]
                                .spacing(4)
                                .wrap(),
                            );
                            if !page.find_query.is_empty() {
                                search = search.push(text(page.matches.status()).size(12));
                            }
                        }
                        body.push(search)
                            .push(text(source_status(page, preview.lines.len())).size(12))
                            .push(self.file_preview_source(page, preview, value))
                            .height(Length::Fill)
                            .into()
                    }
                    Content::Image {
                        handle,
                        width,
                        height,
                    } => {
                        let zoom = pick_list(
                            ImageZoom::OPTIONS.as_slice(),
                            Some(page.zoom),
                            move |zoom| event(Action::SetZoom(zoom)),
                        )
                        .text_size(self.chrome_font_size());
                        let image = iced::widget::image(handle.clone());
                        let image: Element<'_, Message> = if let Some(scale) = page.zoom.scale() {
                            let scale = scale / self.scale_factor();
                            scrollable(
                                image
                                    .width(*width as f32 * scale)
                                    .height(*height as f32 * scale),
                            )
                            .id(image_scroll_id(page.view_token, page.zoom))
                            .direction(both_axes())
                            .height(Length::Fill)
                            .into()
                        } else {
                            image
                                .width(Length::Fill)
                                .height(Length::Fill)
                                .content_fit(iced::ContentFit::ScaleDown)
                                .into()
                        };
                        body.push(tools.wrap())
                            .push(
                                row![text(format!("{width} × {height} pixels")).size(12), zoom,]
                                    .spacing(8)
                                    .wrap(),
                            )
                            .push(image)
                            .height(Length::Fill)
                            .into()
                    }
                }
            }
            Phase::Confirm => body.into(),
        }
    }

    fn file_preview_source<'a>(
        &self,
        page: &'a Page,
        preview: &'a Preview,
        value: &'a str,
    ) -> Element<'a, Message> {
        let row_height = self.preview_line_height();
        let (start, count, pad_top, pad_bottom) = sidebar::sidebar_tree_window(
            page.scroll_y,
            page.viewport_height,
            preview.lines.len(),
            row_height,
        );
        let font_size = self.chrome_font_size().max(12.0);
        let digits = preview.lines.len().to_string().len();
        let active = page.matches.active();
        let mut lines = column![Space::new().height(pad_top)];
        for index in start..start + count {
            let line = source_line(value, &preview.lines, index);
            let row = index + 1;
            let hits = if page.find_open {
                page.matches.hits(Some(row))
            } else {
                &[]
            };
            let mut spans = vec![span(format!("{row:>digits$}  "))];
            let mut start = 0;
            for hit in hits {
                if start < hit.start {
                    spans.push(span(&line[start..hit.start]));
                }
                let [red, green, blue] = match_background(active == Some(*hit));
                spans.push(
                    span(&line[hit.start..hit.end])
                        .background(iced::Color::from_rgb8(red, green, blue))
                        .color(iced::Color::from_rgb8(35, 23, 8)),
                );
                start = hit.end;
            }
            spans.push(span(&line[start..]));
            let label = rich_text::<(), Message, _, _>(spans)
                .size(font_size)
                .font(iced::Font::MONOSPACE)
                .wrapping(text::Wrapping::None);
            let line = container(label)
                .padding(iced::Padding::new(0.0).right(18.0))
                .height(row_height + sidebar::SIDEBAR_TREE_ROW_GAP)
                .align_y(iced::alignment::Vertical::Center);
            lines = lines.push(if page.selected_line == Some(index + 1) {
                line.style(container::rounded_box)
            } else {
                line
            });
        }
        let token = page.token;
        scrollable(lines.push(Space::new().height(pad_bottom)))
            .id(source_scroll_id(page.view_token))
            .direction(both_axes())
            .height(Length::Fill)
            .on_scroll(move |viewport| {
                Message::FilePreviewAction(
                    token,
                    Action::Scrolled {
                        x: viewport.absolute_offset().x,
                        y: viewport.absolute_offset().y,
                        viewport_height: viewport.bounds().height,
                    },
                )
            })
            .into()
    }
}

fn source_text_width(value: &str, font_size: f32) -> f32 {
    use iced::advanced::text::{Paragraph, Renderer, Shaping, Text, Wrapping};
    <iced::Renderer as Renderer>::Paragraph::with_text(Text {
        content: value,
        bounds: iced::Size::INFINITE,
        size: iced::Pixels(font_size),
        line_height: Default::default(),
        font: iced::Font::MONOSPACE,
        align_x: Default::default(),
        align_y: iced::alignment::Vertical::Top,
        shaping: Shaping::Advanced,
        wrapping: Wrapping::None,
    })
    .min_width()
}

fn source_status(page: &Page, count: usize) -> String {
    match page.selected_line {
        Some(line) if line > count => format!(
            "{} is outside this file · {count} lines",
            location_label(line, page.reported_column)
        ),
        Some(line) => format!(
            "{} · {count} lines",
            location_label(line, page.reported_column)
        ),
        None => format!("{count} lines"),
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    use std::os::unix::fs::symlink;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!(
                "frost-preview-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn request_identity_rejects_stale_closed_and_foreign_session_results() {
        assert!(accepts_request(Some((1, 2)), (1, 2), Some(1)));
        assert!(!accepts_request(Some((1, 3)), (1, 2), Some(1)));
        assert!(!accepts_request(None, (1, 2), Some(1)));
        assert!(!accepts_request(Some((1, 2)), (1, 2), Some(2)));
        assert!(!accepts_request(Some((1, 2)), (2, 2), Some(2)));
    }
    #[test]
    fn stale_completion_cannot_release_a_different_read_or_reopen_a_closed_preview() {
        let mut panel = Panel {
            reader: Some((1, 5)),
            ..Panel::default()
        };
        panel.close();
        assert!(panel.accept_completion(1, 4, Some(1)).is_none());
        assert_eq!(panel.reader, Some((1, 5)));
        assert!(panel.accept_completion(1, 5, Some(1)).is_none());
        assert!(panel.reader.is_none());
        assert!(panel.page.is_none());
    }

    #[test]
    fn utf8_signature_is_only_allowed_once_at_start_of_text_content() {
        let f = Fixture::new();
        for name in ["note.md", "app.tsx"] {
            std::fs::write(f.0.join(name), "\u{feff}first\r\nsecond\n").unwrap();
            let preview = load(&format!("{name}:2"), &f.0).unwrap();
            let Content::Text(text) = &preview.content else {
                panic!("expected text")
            };
            assert_eq!(text, "first\r\nsecond\n");
            assert_eq!(source_line(text, &preview.lines, 1), "second");
        }
        for body in ["\u{feff}", "plain text"] {
            std::fs::write(f.0.join("note.md"), body).unwrap();
            assert!(matches!(load("note.md", &f.0).unwrap().content,
                Content::Text(text) if text == body.strip_prefix('\u{feff}').unwrap_or(body)));
        }
        for body in [
            "\u{feff}\u{feff}text",
            "a\u{feff}b",
            "\u{feff}a\u{202e}b",
            "\u{feff}a\0b",
        ] {
            std::fs::write(f.0.join("note.md"), body).unwrap();
            assert!(load("note.md", &f.0).is_err());
        }
        std::fs::write(f.0.join("\u{feff}note.md"), "text").unwrap();
        assert!(load("\u{feff}note.md", &f.0).is_err());
        let mut oversized = vec![b'a'; 64 * 1024];
        oversized.splice(..0, [0xef, 0xbb, 0xbf]);
        std::fs::write(f.0.join("note.md"), oversized).unwrap();
        assert!(load("note.md", &f.0).unwrap_err().contains("64 KiB"));
    }

    #[test]
    fn source_text_is_static_and_source_locations_are_supported() {
        let f = Fixture::new();
        std::fs::write(f.0.join("app.tsx"), "<script>do_not_execute()</script>").unwrap();
        let p = load("app.tsx:2:3", &f.0).unwrap();
        assert!(
            matches!(p.content, Content::Text(ref s) if s == "<script>do_not_execute()</script>")
        );
        std::fs::write(f.0.join("note.md"), "[link](https://example.org)").unwrap();
        assert!(matches!(
            load("note.md", &f.0).unwrap().content,
            Content::Text(_)
        ));
    }
    #[test]
    fn indexed_source_preserves_exact_colon_names_and_requested_coordinates() {
        let f = Fixture::new();
        std::fs::write(
            f.0.join("app.tsx"),
            "first\n界é\nthird\nfourth\nfifth\nsixth",
        )
        .unwrap();
        let p = load("app.tsx:2:99", &f.0).unwrap();
        assert_eq!(p.location, Some((2, Some(99))));
        let Content::Text(text) = &p.content else {
            panic!("expected text")
        };
        assert_eq!(source_line(text, &p.lines, 1), "界é");
        assert_eq!(source_line(text, &p.lines, 5), "sixth");
        let outside = load("app.tsx:999", &f.0).unwrap();
        assert_eq!(outside.location, Some((999, None)));
        assert_eq!(outside.lines.len(), 6);
        for suffix in ["0", "2:0", "184467440737095516160", "2:bad"] {
            assert!(load(&format!("app.tsx:{suffix}"), &f.0).is_err());
        }
        std::fs::write(f.0.join("app.tsx:2:99"), "literal filename").unwrap();
        let exact = load("app.tsx:2:99", &f.0).unwrap();
        assert!(exact.location.is_none());
        assert!(ReadTarget::tree_file(&f.0.join("app.tsx:5"))
            .unwrap()
            .load()
            .is_err());
    }

    #[test]
    fn indexed_source_preserves_empty_eof_lines_and_crlf() {
        let f = Fixture::new();
        for (body, line, expected) in [
            ("", 1, ""),
            ("first\n", 2, ""),
            ("first\r\nsecond\r\n", 2, "second"),
        ] {
            std::fs::write(f.0.join("note.md"), body).unwrap();
            let preview = load(&format!("note.md:{line}"), &f.0).unwrap();
            let Content::Text(text) = &preview.content else {
                panic!("expected text")
            };
            assert_eq!(source_line(text, &preview.lines, line - 1), expected);
        }
    }

    #[test]
    fn source_location_fallback_requires_missing_final_file() {
        use std::os::unix::fs::PermissionsExt;
        let f = Fixture::new();
        std::fs::write(f.0.join("app.tsx"), "source").unwrap();
        assert_eq!(
            load("app.tsx:2:3", &f.0).unwrap().location,
            Some((2, Some(3)))
        );
        let exact = f.0.join("app.tsx:2");
        symlink("app.tsx", &exact).unwrap();
        assert!(
            load("app.tsx:2", &f.0).is_err(),
            "a refused symlink must not select another file"
        );
        std::fs::remove_file(&exact).unwrap();
        std::fs::create_dir(&exact).unwrap();
        assert!(
            load("app.tsx:2", &f.0).is_err(),
            "a directory must not select another file"
        );
        std::fs::remove_dir(&exact).unwrap();
        std::fs::write(&exact, "exact").unwrap();
        assert!(load("app.tsx:2", &f.0).unwrap().location.is_none());
        std::fs::set_permissions(&exact, std::fs::Permissions::from_mode(0o0)).unwrap();
        if unsafe { libc::geteuid() } != 0 {
            assert!(
                load("app.tsx:2", &f.0).is_err(),
                "permission denial must not select another file"
            );
        }
        assert!(
            !OpenError::component(std::io::Error::from_raw_os_error(libc::EACCES), true)
                .missing_final_component
        );
        assert!(
            !open_regular(&f.0, Path::new("missing/app.tsx:2"))
                .unwrap_err()
                .missing_final_component
        );
        std::fs::set_permissions(&exact, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(load("missing/app.tsx:2", &f.0).is_err());
    }

    #[test]
    fn rejects_escape_symlink_directory_and_fifo_without_reading() {
        let f = Fixture::new();
        std::fs::write(f.0.join("safe"), "ok").unwrap();
        symlink(f.0.join("safe"), f.0.join("alias")).unwrap();
        symlink(&f.0, f.0.join("dir_alias")).unwrap();
        let fifo = std::ffi::CString::new(f.0.join("fifo").to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        for p in [
            "../outside",
            "/etc/passwd",
            "alias",
            "dir_alias/safe",
            "fifo",
            ".",
        ] {
            assert!(load(p, &f.0).is_err(), "{p}");
        }
        assert!(load("safe", Path::new("relative")).is_err());
        assert!(load("safe", Path::new("/tmp/\u{202e}spoof")).is_err());
    }
    #[test]
    fn rejects_oversized_binary_and_control_text() {
        let f = Fixture::new();
        for (name, bytes) in [
            ("big.txt", vec![b'a'; 65537]),
            ("bad.txt", vec![0xff]),
            ("escape.txt", b"\x1b[31m".to_vec()),
            ("bidi.txt", "\u{202e}spoof".as_bytes().to_vec()),
        ] {
            std::fs::write(f.0.join(name), bytes).unwrap();
            assert!(load(name, &f.0).is_err());
        }
        let huge = std::fs::File::create(f.0.join("huge.png")).unwrap();
        huge.set_len(BYTE_LIMIT + 1).unwrap();
        assert!(load("huge.png", &f.0).is_err());
    }
    #[test]
    fn decodes_small_png_without_loading_remote_resources() {
        let f = Fixture::new();
        image::RgbaImage::new(2, 3)
            .save(f.0.join("image.png"))
            .unwrap();
        assert!(matches!(
            load("image.png", &f.0).unwrap().content,
            Content::Image { .. }
        ));
        image::RgbaImage::new(4097, 1)
            .save(f.0.join("wide.png"))
            .unwrap();
        assert!(load("wide.png", &f.0).is_err());
    }
}
