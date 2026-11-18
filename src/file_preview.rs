//! Read-only, bounded local previews. Never invoke an opener, shell or web renderer.
use std::io::Read;
use std::path::{Component, Path, PathBuf};

const BYTE_LIMIT: u64 = 2 * 1024 * 1024;
const PIXEL_LIMIT: u64 = 4 * 1024 * 1024;

#[derive(Debug, Clone)]
pub(crate) enum Content {
    Text(String),
    Image(iced::widget::image::Handle),
}
#[derive(Debug, Clone)]
pub(crate) struct Preview {
    pub path: PathBuf,
    pub content: Content,
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

// Walk from a pinned directory descriptor. Refuse symlinks at every component,
// including races after validation; O_NONBLOCK prevents FIFO/device open hangs.
#[cfg(target_os = "linux")]
fn open_regular(cwd: &Path, relative: &Path) -> Result<std::fs::File, String> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::OpenOptionsExt;
    let mut dir = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC)
        .open(cwd)
        .map_err(|e| e.to_string())?;
    let components: Vec<_> = relative.components().collect();
    for (index, component) in components.iter().enumerate() {
        let name = CString::new(component.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
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
            return Err(std::io::Error::last_os_error().to_string());
        }
        let file = unsafe { std::fs::File::from_raw_fd(fd) };
        if last {
            if !file.metadata().map_err(|e| e.to_string())?.is_file() {
                return Err("Only regular files can be previewed".into());
            }
            // Reopen the checked, pinned inode, not the attacker-controlled path.
            // O_PATH above never opens a device/FIFO for I/O.
            return std::fs::File::open(format!("/proc/self/fd/{}", file.as_raw_fd()))
                .map_err(|e| e.to_string());
        }
        dir = file;
    }
    Err("Choose a regular file".into())
}
#[cfg(not(target_os = "linux"))]
fn open_regular(_: &Path, _: &Path) -> Result<std::fs::File, String> {
    Err("Safe local preview is not supported on this platform".into())
}

pub(crate) fn load(path: &str, cwd: &Path) -> Result<Preview, String> {
    let mut relative = relative_path(path, cwd)?;
    let mut file = match open_regular(cwd, &relative) {
        Ok(file) => file,
        Err(original) => {
            // Compiler source locations are interpreted only after the exact
            // displayed filename fails. Never silently strip arbitrary colons.
            let Some((prefix, number)) = path.rsplit_once(':') else {
                return Err(original);
            };
            if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
                return Err(original);
            }
            let prefix = match prefix.rsplit_once(':') {
                Some((prefix, line))
                    if !line.is_empty() && line.bytes().all(|b| b.is_ascii_digit()) =>
                {
                    prefix
                }
                _ => prefix,
            };
            relative = relative_path(prefix, cwd)?;
            open_regular(cwd, &relative)?
        }
    };
    if file.metadata().map_err(|e| e.to_string())?.len() > BYTE_LIMIT {
        return Err("Preview exceeds the 2 MiB limit".into());
    }
    let mut bytes = Vec::new();
    file.by_ref()
        .take(BYTE_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > BYTE_LIMIT {
        return Err("Preview exceeds the 2 MiB limit".into());
    }
    let extension = relative
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let content = if matches!(extension.as_str(), "png" | "jpg" | "jpeg") {
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
        Content::Image(iced::widget::image::Handle::from_rgba(
            width,
            height,
            rgba.into_raw(),
        ))
    } else {
        if bytes.len() > 64 * 1024 {
            return Err("Text preview exceeds the 64 KiB limit".into());
        }
        let text =
            String::from_utf8(bytes).map_err(|_| "File is not UTF-8 text or a supported image")?;
        if text.chars().any(|c| {
            (c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
                || jterm_core::review_input::is_visual_spoofing_character(c)
        }) {
            return Err("Text contains unsafe control or formatting characters".into());
        }
        Content::Text(text)
    };
    Ok(Preview {
        path: cwd.join(relative),
        content,
    })
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
            Content::Image(_)
        ));
        image::RgbaImage::new(4097, 1)
            .save(f.0.join("wide.png"))
            .unwrap();
        assert!(load("wide.png", &f.0).is_err());
    }
}
