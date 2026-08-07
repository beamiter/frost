//! Whole-session command-block export.
//!
//! The UI snapshots one terminal's bounded OSC 133 records, then hands the
//! owned snapshot to a worker. Serialization and durable file I/O therefore
//! never hold terminal state or block the renderer.

use crate::block_mode;
use crate::persistence;
use crate::terminal::{TerminalState, ZoneOutputExport};
use serde::Serialize;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Maximum command/output text cloned out of one terminal for an export.
/// Normal captured output is already held to an 8 MiB aggregate budget; this
/// also bounds live-row fallbacks whose snapshots were evicted.
const MAX_SESSION_EXPORT_SOURCE_BYTES: usize = 16 * 1024 * 1024;
/// JSON escaping can expand source text; keep the final file bounded too.
pub const MAX_SESSION_EXPORT_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionExportFormat {
    Markdown,
    Json,
}

impl SessionExportFormat {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Markdown => "md",
            Self::Json => "json",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Markdown => "Markdown",
            Self::Json => "JSON",
        }
    }
}

/// Stable, renderer-independent representation of one exported frost block.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SessionExportBlock {
    pub id: u64,
    pub command: Option<String>,
    pub output: String,
    pub output_truncated: bool,
    /// Both the retained output snapshot and its original scrollback rows are
    /// gone. Kept distinct from a command that genuinely printed nothing.
    pub output_unavailable: bool,
    pub exit_code: Option<i32>,
    pub duration_ms: Option<u64>,
    pub finished_at_ms: Option<u64>,
    pub tz_offset_secs: i32,
    pub cwd: Option<String>,
    pub command_truncated: bool,
    /// `false` for a lifecycle recovered at the next prompt after OSC 133 `D`
    /// was lost; such a retained block is useful but not a reported completion.
    pub completion_observed: bool,
}

impl SessionExportBlock {
    fn markdown(&self) -> String {
        block_mode::markdown_export_with_state(
            &block_mode::MarkdownBlock {
                command: self.command.as_deref(),
                output: &self.output,
                output_truncated: self.output_truncated,
                exit_code: self.exit_code,
                duration_ms: self.duration_ms,
                finished_at_ms: self.finished_at_ms,
                tz_offset_secs: self.tz_offset_secs,
                cwd: self.cwd.as_deref(),
            },
            self.command_truncated,
            self.output_unavailable,
            self.completion_observed,
        )
    }
}

fn too_large() -> io::Error {
    io::Error::new(
        io::ErrorKind::FileTooLarge,
        format!(
            "session block text exceeds {} MiB",
            MAX_SESSION_EXPORT_SOURCE_BYTES / (1024 * 1024)
        ),
    )
}

/// Clone the active terminal's retained finalized blocks oldest-first. Each zone's
/// captured snapshot wins over live row extraction through the terminal's
/// existing accessor, so records already outside scrollback still export.
pub fn snapshot_session(terminal: &TerminalState) -> io::Result<Vec<SessionExportBlock>> {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let mut source_bytes = 0usize;
    let mut blocks = Vec::with_capacity(terminal.command_zones.len());

    for zone in &terminal.command_zones {
        let (output, output_truncated, output_unavailable) =
            match terminal.zone_output_export_capped(zone.id) {
                Some(ZoneOutputExport::Available { text, truncated }) => (text, truncated, false),
                Some(ZoneOutputExport::Empty) => (String::new(), false, false),
                Some(ZoneOutputExport::Unavailable) => (String::new(), false, true),
                None => {
                    return Err(io::Error::new(
                        io::ErrorKind::NotFound,
                        "retained command block disappeared during export snapshot",
                    ));
                }
            };
        let record_bytes = output
            .len()
            .saturating_add(zone.command.as_ref().map_or(0, String::len))
            .saturating_add(zone.cwd.as_ref().map_or(0, String::len))
            .saturating_add(128);
        source_bytes = source_bytes
            .checked_add(record_bytes)
            .ok_or_else(too_large)?;
        if source_bytes > MAX_SESSION_EXPORT_SOURCE_BYTES {
            return Err(too_large());
        }
        let offset_at = zone.finished_at_ms.unwrap_or(now_ms);
        blocks.push(SessionExportBlock {
            id: zone.id,
            command: zone.command.clone(),
            output,
            output_truncated,
            output_unavailable,
            exit_code: zone.exit_code,
            duration_ms: zone.duration_ms,
            finished_at_ms: zone.finished_at_ms,
            tz_offset_secs: block_mode::local_offset_secs((offset_at / 1000) as i64),
            cwd: zone.cwd.clone(),
            command_truncated: zone.command_truncated,
            completion_observed: zone.completion_observed,
        });
    }
    Ok(blocks)
}

struct BoundedBuffer {
    bytes: Vec<u8>,
}

impl BoundedBuffer {
    fn new() -> Self {
        Self { bytes: Vec::new() }
    }

    fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

impl Write for BoundedBuffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.bytes.len().saturating_add(bytes.len()) > MAX_SESSION_EXPORT_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                format!(
                    "serialized session export exceeds {} MiB",
                    MAX_SESSION_EXPORT_BYTES / (1024 * 1024)
                ),
            ));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn serialize_session(
    blocks: &[SessionExportBlock],
    format: SessionExportFormat,
) -> io::Result<Vec<u8>> {
    let mut writer = BoundedBuffer::new();
    match format {
        SessionExportFormat::Json => {
            serde_json::to_writer_pretty(&mut writer, blocks)
                .map_err(|error| io::Error::other(error.to_string()))?;
            writer.write_all(b"\n")?;
        }
        SessionExportFormat::Markdown => {
            writeln!(writer, "# Terminal Session Export\n")?;
            writeln!(writer, "Total blocks: {}\n", blocks.len())?;
            writeln!(writer, "---\n")?;
            for (index, block) in blocks.iter().enumerate() {
                writeln!(writer, "## Block #{}\n", index + 1)?;
                writer.write_all(block.markdown().as_bytes())?;
                writeln!(writer, "\n---\n")?;
            }
        }
    }
    Ok(writer.finish())
}

fn export_file_name(stamp: &str, extension: &str, attempt: u32) -> String {
    if attempt == 0 {
        format!("session-{stamp}.{extension}")
    } else {
        format!("session-{stamp}-{attempt}.{extension}")
    }
}

fn write_session_export(
    directory: &Path,
    stamp: &str,
    format: SessionExportFormat,
    contents: &[u8],
) -> io::Result<PathBuf> {
    persistence::ensure_private_directory(directory)?;
    for attempt in 0..100u32 {
        let path = directory.join(export_file_name(stamp, format.extension(), attempt));
        match persistence::write_new_private_file(&path, contents, MAX_SESSION_EXPORT_BYTES as u64)
        {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "too many session exports share this timestamp",
    ))
}

fn export_stamp() -> String {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let offset = block_mode::local_offset_secs((now_ms / 1000) as i64);
    block_mode::compact_timestamp_at_offset(now_ms, offset)
}

/// Serialize and durably write one owned session snapshot. Intended for a
/// blocking worker; no live terminal or renderer state is touched here.
pub fn export_session_to_file(
    blocks: &[SessionExportBlock],
    format: SessionExportFormat,
) -> io::Result<PathBuf> {
    let contents = serialize_session(blocks, format)?;
    let directory = dirs::data_dir()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no user data directory"))?
        .join("frost")
        .join("exports");
    write_session_export(&directory, &export_stamp(), format, &contents)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(name: &str) -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            Self(std::env::temp_dir().join(format!(
                "frost-block-export-{name}-{}-{unique}",
                std::process::id()
            )))
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn block(id: u64, command: &str, output: &str) -> SessionExportBlock {
        SessionExportBlock {
            id,
            command: Some(command.to_string()),
            output: output.to_string(),
            output_truncated: false,
            output_unavailable: false,
            exit_code: Some(0),
            duration_ms: Some(1250),
            finished_at_ms: Some(0),
            tz_offset_secs: 0,
            cwd: Some("/tmp/project".to_string()),
            command_truncated: false,
            completion_observed: true,
        }
    }

    #[test]
    fn session_documents_keep_order_and_shared_block_metadata() {
        let blocks = [block(7, "cargo test", "ok"), block(9, "pwd", "/tmp")];
        let markdown =
            String::from_utf8(serialize_session(&blocks, SessionExportFormat::Markdown).unwrap())
                .unwrap();
        assert!(markdown.starts_with("# Terminal Session Export\n\nTotal blocks: 2\n"));
        assert!(markdown.find("cargo test").unwrap() < markdown.find("pwd").unwrap());
        assert!(markdown.contains("- Finished: 1970-01-01 00:00:00 +00:00"));

        let json: serde_json::Value =
            serde_json::from_slice(&serialize_session(&blocks, SessionExportFormat::Json).unwrap())
                .unwrap();
        assert_eq!(json[0]["id"], 7);
        assert_eq!(json[1]["command"], "pwd");
        assert_eq!(json[0]["output"], "ok");
        assert_eq!(json[0]["output_unavailable"], false);
        assert_eq!(json[0]["completion_observed"], true);
    }

    #[test]
    fn exports_disclose_truncated_commands_and_unavailable_output() {
        let mut retained = block(7, "partial-command", "");
        retained.command_truncated = true;
        retained.output_unavailable = true;
        retained.completion_observed = false;

        let markdown = String::from_utf8(
            serialize_session(&[retained.clone()], SessionExportFormat::Markdown).unwrap(),
        )
        .unwrap();
        assert!(markdown.contains("- Note: command truncated by shell"));
        assert!(markdown.contains("- Note: output unavailable"));
        assert!(markdown.contains("- Note: command completion not observed"));

        let json: serde_json::Value = serde_json::from_slice(
            &serialize_session(&[retained], SessionExportFormat::Json).unwrap(),
        )
        .unwrap();
        assert_eq!(json[0]["command_truncated"], true);
        assert_eq!(json[0]["output_unavailable"], true);
        assert_eq!(json[0]["completion_observed"], false);
    }

    #[test]
    fn exports_are_private_and_same_second_writes_never_replace() {
        let dir = TestDir::new("collision");
        fs::create_dir_all(&dir.0).unwrap();
        fs::set_permissions(&dir.0, fs::Permissions::from_mode(0o775)).unwrap();
        let first = write_session_export(
            &dir.0,
            "20260807-101112",
            SessionExportFormat::Markdown,
            b"first",
        )
        .unwrap();
        let second = write_session_export(
            &dir.0,
            "20260807-101112",
            SessionExportFormat::Markdown,
            b"second",
        )
        .unwrap();

        assert_eq!(first.file_name().unwrap(), "session-20260807-101112.md");
        assert_eq!(second.file_name().unwrap(), "session-20260807-101112-1.md");
        assert_eq!(fs::read(&first).unwrap(), b"first");
        assert_eq!(fs::read(&second).unwrap(), b"second");
        assert!(fs::read_dir(&dir.0).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".frost-export.tmp.")));
        assert_eq!(
            fs::metadata(&first).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(&dir.0).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[test]
    fn concurrent_same_second_exports_publish_distinct_complete_files() {
        use std::collections::HashSet;
        use std::sync::{Arc, Barrier};

        let dir = TestDir::new("concurrent");
        fs::create_dir_all(&dir.0).unwrap();
        fs::set_permissions(&dir.0, fs::Permissions::from_mode(0o700)).unwrap();
        let barrier = Arc::new(Barrier::new(2));
        let workers: Vec<_> = [b"left".as_slice(), b"right".as_slice()]
            .into_iter()
            .map(|contents| {
                let directory = dir.0.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    write_session_export(
                        &directory,
                        "20260807-101112",
                        SessionExportFormat::Json,
                        contents,
                    )
                    .unwrap()
                })
            })
            .collect();
        let paths: Vec<_> = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect();

        assert_ne!(paths[0], paths[1]);
        let contents: HashSet<Vec<u8>> = paths.iter().map(|path| fs::read(path).unwrap()).collect();
        assert_eq!(
            contents,
            HashSet::from([b"left".to_vec(), b"right".to_vec()])
        );
    }

    #[test]
    fn export_file_names_are_family_shaped() {
        assert_eq!(
            export_file_name("20260807-101112", "json", 0),
            "session-20260807-101112.json"
        );
        assert_eq!(
            export_file_name("20260807-101112", "json", 3),
            "session-20260807-101112-3.json"
        );
    }

    #[test]
    fn an_existing_symlink_is_a_collision_and_its_target_is_untouched() {
        use std::os::unix::fs::symlink;

        let dir = TestDir::new("symlink");
        fs::create_dir_all(&dir.0).unwrap();
        fs::set_permissions(&dir.0, fs::Permissions::from_mode(0o700)).unwrap();
        let victim = dir.0.join("victim");
        fs::write(&victim, b"keep").unwrap();
        let first_name = dir.0.join("session-20260807-101112.json");
        symlink(&victim, &first_name).unwrap();

        let exported =
            write_session_export(&dir.0, "20260807-101112", SessionExportFormat::Json, b"new")
                .unwrap();

        assert_eq!(
            exported.file_name().unwrap(),
            "session-20260807-101112-1.json"
        );
        assert_eq!(fs::read(&victim).unwrap(), b"keep");
        assert!(fs::symlink_metadata(&first_name)
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[test]
    fn export_directory_symlinks_are_rejected() {
        use std::os::unix::fs::symlink;

        let root = TestDir::new("directory-symlink");
        fs::create_dir_all(&root.0).unwrap();
        fs::set_permissions(&root.0, fs::Permissions::from_mode(0o700)).unwrap();
        let victim = root.0.join("victim");
        fs::create_dir(&victim).unwrap();
        let linked = root.0.join("exports");
        symlink(&victim, &linked).unwrap();

        assert!(write_session_export(
            &linked,
            "20260807-101112",
            SessionExportFormat::Markdown,
            b"private",
        )
        .is_err());
        assert_eq!(fs::read_dir(&victim).unwrap().count(), 0);
    }
}
