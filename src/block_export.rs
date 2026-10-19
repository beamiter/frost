//! Whole-session command-block export.
//!
//! The UI snapshots one terminal's bounded OSC 133 records, then hands the
//! owned snapshot to a worker. Serialization and durable file I/O therefore
//! never hold terminal state or block the renderer.

use crate::block_mode;
use crate::persistence;
use crate::terminal::{TerminalState, ZoneOutputExport};
use serde::Serialize;
use std::collections::HashSet;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Maximum command/output text cloned out of one terminal for an export.
/// Normal captured output is already held to an 8 MiB aggregate budget; this
/// also bounds live-row fallbacks whose snapshots were evicted.
const MAX_SESSION_EXPORT_SOURCE_BYTES: usize = 16 * 1024 * 1024;
/// JSON escaping can expand source text; keep the final file bounded too.
pub const MAX_SESSION_EXPORT_BYTES: usize = 64 * 1024 * 1024;
pub const SESSION_EXPORT_SCHEMA: &str = "frost.block-session";
pub const SESSION_EXPORT_SCHEMA_VERSION: u32 = 1;
pub const MAX_SELECTED_BLOCKS: usize = 128;

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
    /// Whether the OSC 133 `C` command-start mark was observed. Kept even
    /// after row eviction so lifecycle health remains stable in exports.
    pub start_mark_seen: bool,
    #[serde(serialize_with = "serialize_completion_provenance")]
    pub completion_provenance: block_mode::CompletionProvenance,
    #[serde(serialize_with = "serialize_lifecycle_health")]
    pub lifecycle_health: block_mode::BlockLifecycleHealth,
    /// `false` for a lifecycle recovered at the next prompt after OSC 133 `D`
    /// was lost. Retained for compatibility with the v1 boolean field; new
    /// consumers should use `completion_provenance`.
    pub completion_observed: bool,
}

fn serialize_completion_provenance<S>(
    value: &block_mode::CompletionProvenance,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_str(block_mode::completion_provenance_schema_name(*value))
}

fn serialize_lifecycle_health<S>(
    value: &block_mode::BlockLifecycleHealth,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_str(block_mode::lifecycle_health_schema_name(*value))
}

/// One immutable pane snapshot handed to the export worker. Keeping identity
/// and capture time beside the blocks makes JSON exports self-describing even
/// after their originating Frost process has exited.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionExportSnapshot {
    pub pane_session_id: usize,
    pub captured_at_ms: u64,
    pub captured_tz_offset_secs: i32,
    pub blocks: Vec<SessionExportBlock>,
    pub selection_only: bool,
}

#[derive(Serialize)]
struct SessionExportRetention {
    retained_blocks: usize,
    command_truncated: usize,
    output_truncated: usize,
    output_unavailable: usize,
    completion_unobserved: usize,
    lifecycle_degraded: usize,
    lifecycle_recovered: usize,
    lifecycle_incomplete: usize,
}

#[derive(Serialize)]
struct JsonSessionExport<'a> {
    schema: &'static str,
    version: u32,
    pane_session_id: usize,
    captured_at_ms: u64,
    captured_tz_offset_secs: i32,
    block_order: &'static str,
    scope: &'static str,
    retention: SessionExportRetention,
    blocks: &'a [SessionExportBlock],
}

impl SessionExportSnapshot {
    fn retention(&self) -> SessionExportRetention {
        let is_command_block = |block: &&SessionExportBlock| {
            block.command_truncated
                || block
                    .command
                    .as_deref()
                    .is_some_and(|command| !command.trim().is_empty())
        };
        SessionExportRetention {
            retained_blocks: self.blocks.len(),
            command_truncated: self
                .blocks
                .iter()
                .filter(|block| block.command_truncated)
                .count(),
            output_truncated: self
                .blocks
                .iter()
                .filter(|block| block.output_truncated)
                .count(),
            output_unavailable: self
                .blocks
                .iter()
                .filter(|block| block.output_unavailable)
                .count(),
            completion_unobserved: self
                .blocks
                .iter()
                .filter(|block| !block.completion_observed)
                .count(),
            lifecycle_degraded: self
                .blocks
                .iter()
                .filter(is_command_block)
                .filter(|block| {
                    block.lifecycle_health == block_mode::BlockLifecycleHealth::Degraded
                })
                .count(),
            lifecycle_recovered: self
                .blocks
                .iter()
                .filter(is_command_block)
                .filter(|block| {
                    block.lifecycle_health == block_mode::BlockLifecycleHealth::Recovered
                })
                .count(),
            lifecycle_incomplete: self
                .blocks
                .iter()
                .filter(is_command_block)
                .filter(|block| {
                    block.lifecycle_health == block_mode::BlockLifecycleHealth::Incomplete
                })
                .count(),
        }
    }
}

impl SessionExportBlock {
    fn markdown(&self) -> String {
        block_mode::markdown_export_with_lifecycle(
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
            self.start_mark_seen,
            self.completion_provenance,
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

fn snapshot_zone(
    terminal: &TerminalState,
    zone: &crate::terminal::CommandZone,
    now_ms: u64,
    source_bytes: &mut usize,
    retained_only: bool,
) -> io::Result<SessionExportBlock> {
    let (output, output_truncated, output_unavailable) = match if retained_only {
        Some(match &zone.captured_output {
            Some((text, truncated)) => ZoneOutputExport::Available {
                text: text.clone(),
                truncated: *truncated,
            },
            None if zone.captured_output_evicted => ZoneOutputExport::Unavailable,
            None => ZoneOutputExport::Empty,
        })
    } else {
        terminal.zone_output_export_capped(zone.id)
    } {
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
    *source_bytes = source_bytes
        .checked_add(record_bytes)
        .ok_or_else(too_large)?;
    if *source_bytes > MAX_SESSION_EXPORT_SOURCE_BYTES {
        return Err(too_large());
    }
    let offset_at = zone.finished_at_ms.unwrap_or(now_ms);
    Ok(SessionExportBlock {
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
        start_mark_seen: zone.start_mark_seen,
        completion_provenance: zone.completion_provenance,
        lifecycle_health: block_mode::assess_lifecycle(
            zone.start_mark_seen,
            zone.completion_provenance,
        ),
        completion_observed: zone.completion_provenance
            == block_mode::CompletionProvenance::ShellReported,
    })
}

fn snapshot_with_blocks(
    pane_session_id: usize,
    now_ms: u64,
    blocks: Vec<SessionExportBlock>,
) -> SessionExportSnapshot {
    SessionExportSnapshot {
        pane_session_id,
        captured_at_ms: now_ms,
        captured_tz_offset_secs: block_mode::local_offset_secs((now_ms / 1000) as i64),
        blocks,
        selection_only: false,
    }
}

/// Clone the active terminal's retained finalized blocks oldest-first. Each zone's
/// captured snapshot wins over live row extraction through the terminal's
/// existing accessor, so records already outside scrollback still export.
pub fn snapshot_session(
    terminal: &TerminalState,
    pane_session_id: usize,
) -> io::Result<SessionExportSnapshot> {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let mut source_bytes = 0usize;
    let mut blocks = Vec::with_capacity(terminal.command_zones.len());

    for zone in &terminal.command_zones {
        blocks.push(snapshot_zone(
            terminal,
            zone,
            now_ms,
            &mut source_bytes,
            false,
        )?);
    }
    Ok(snapshot_with_blocks(pane_session_id, now_ms, blocks))
}

/// Snapshot one exact stable zone for a right-click export. Unrelated retained
/// blocks are never cloned or allowed to make this single-block operation fail.
pub fn snapshot_block(
    terminal: &TerminalState,
    pane_session_id: usize,
    zone_id: u64,
) -> io::Result<SessionExportSnapshot> {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let zone = terminal.zone_by_id(zone_id).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "right-clicked command block is no longer retained",
        )
    })?;
    let mut source_bytes = 0usize;
    let block = snapshot_zone(terminal, zone, now_ms, &mut source_bytes, false)?;
    Ok(snapshot_with_blocks(pane_session_id, now_ms, vec![block]))
}

/// Snapshot exactly the requested stable identities in terminal order. Missing
/// or duplicate identities reject the entire operation before output is cloned.
/// Only finalization captures are used; an evicted capture stays unavailable
/// even if unrelated text is now visible at its old row coordinates.
pub fn snapshot_selected(
    terminal: &TerminalState,
    pane_session_id: usize,
    ids: &[u64],
) -> io::Result<SessionExportSnapshot> {
    if ids.is_empty() || ids.len() > MAX_SELECTED_BLOCKS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("select between 1 and {MAX_SELECTED_BLOCKS} retained blocks"),
        ));
    }
    let wanted: HashSet<_> = ids.iter().copied().collect();
    if wanted.len() != ids.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "selected block identities are duplicated",
        ));
    }
    let zones: Vec<_> = terminal
        .command_zones
        .iter()
        .filter(|zone| wanted.contains(&zone.id))
        .collect();
    if zones.len() != wanted.len()
        || zones.iter().map(|zone| zone.id).collect::<HashSet<_>>() != wanted
    {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "a selected block is no longer retained; review the complete selection again",
        ));
    }
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let mut source_bytes = 0;
    let blocks = zones
        .into_iter()
        .map(|zone| snapshot_zone(terminal, zone, now_ms, &mut source_bytes, true))
        .collect::<io::Result<Vec<_>>>()?;
    let mut snapshot = snapshot_with_blocks(pane_session_id, now_ms, blocks);
    snapshot.selection_only = true;
    Ok(snapshot)
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
    snapshot: &SessionExportSnapshot,
    format: SessionExportFormat,
) -> io::Result<Vec<u8>> {
    for block in &snapshot.blocks {
        if block
            .command
            .as_deref()
            .is_some_and(crate::block_mode::command_text_is_unsafe)
            || crate::block_mode::clipboard_multiline_is_unsafe(&block.output)
            || block
                .cwd
                .as_deref()
                .is_some_and(crate::block_mode::clipboard_multiline_is_unsafe)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "session export contains control or visual-spoofing characters",
            ));
        }
    }
    let mut writer = BoundedBuffer::new();
    match format {
        SessionExportFormat::Json => {
            let document = JsonSessionExport {
                schema: SESSION_EXPORT_SCHEMA,
                version: SESSION_EXPORT_SCHEMA_VERSION,
                pane_session_id: snapshot.pane_session_id,
                captured_at_ms: snapshot.captured_at_ms,
                captured_tz_offset_secs: snapshot.captured_tz_offset_secs,
                block_order: "oldest_first",
                scope: if snapshot.selection_only {
                    "selected_blocks"
                } else {
                    "retained_session"
                },
                retention: snapshot.retention(),
                blocks: &snapshot.blocks,
            };
            serde_json::to_writer_pretty(&mut writer, &document)
                .map_err(|error| io::Error::other(error.to_string()))?;
            writer.write_all(b"\n")?;
        }
        SessionExportFormat::Markdown => {
            writeln!(
                writer,
                "# Terminal {} Export\n",
                if snapshot.selection_only {
                    "Selection"
                } else {
                    "Session"
                }
            )?;
            writeln!(writer, "Pane session: {}", snapshot.pane_session_id)?;
            writeln!(
                writer,
                "Captured: {}",
                block_mode::timestamp_at_offset(
                    snapshot.captured_at_ms,
                    snapshot.captured_tz_offset_secs
                )
            )?;
            writeln!(
                writer,
                "{} blocks: {}\n",
                if snapshot.selection_only {
                    "Selected"
                } else {
                    "Total retained"
                },
                snapshot.blocks.len()
            )?;
            writeln!(writer, "---\n")?;
            for (index, block) in snapshot.blocks.iter().enumerate() {
                writeln!(writer, "## Block #{}\n", index + 1)?;
                if snapshot.selection_only {
                    writeln!(writer, "Terminal block ID: {}\n", block.id)?;
                }
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
    write_export_with_scope(directory, stamp, format, contents, false)
}

fn write_export_with_scope(
    directory: &Path,
    stamp: &str,
    format: SessionExportFormat,
    contents: &[u8],
    selection_only: bool,
) -> io::Result<PathBuf> {
    persistence::ensure_private_directory(directory)?;
    for attempt in 0..100u32 {
        let name = export_file_name(stamp, format.extension(), attempt);
        let name = if selection_only {
            name.replacen("session-", "selection-", 1)
        } else {
            name
        };
        let path = directory.join(name);
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
    snapshot: &SessionExportSnapshot,
    format: SessionExportFormat,
) -> io::Result<PathBuf> {
    let contents = serialize_session(snapshot, format)?;
    let directory = dirs::data_dir()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no user data directory"))?
        .join("frost")
        .join("exports");
    if snapshot.selection_only {
        write_export_with_scope(&directory, &export_stamp(), format, &contents, true)
    } else {
        write_session_export(&directory, &export_stamp(), format, &contents)
    }
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
            start_mark_seen: true,
            completion_provenance: block_mode::CompletionProvenance::ShellReported,
            lifecycle_health: block_mode::BlockLifecycleHealth::Healthy,
            completion_observed: true,
        }
    }

    fn snapshot(blocks: Vec<SessionExportBlock>) -> SessionExportSnapshot {
        SessionExportSnapshot {
            pane_session_id: 42,
            captured_at_ms: 0,
            captured_tz_offset_secs: 0,
            blocks,
            selection_only: false,
        }
    }

    #[test]
    fn snapshot_session_bridges_real_osc_133_block_lifecycles() {
        let mut terminal = TerminalState::new(80, 12);

        // An ordinary observed completion keeps the shell-provided command,
        // status, duration, and cwd alongside the captured output snapshot.
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07printf done\r\n");
        terminal.process_input(
            b"\x1b]133;C;cmdline_url=printf%20done\x07done\r\n\x1b]133;D;exit_code=7;duration_ms=1250;cwd_url=%2Ftmp%2Fjob\x07",
        );

        // Visible asynchronous output at an untouched prompt becomes a
        // commandless Background block when the next prompt begins.
        terminal.process_input(
            b"\x1b]133;A;cwd_url=%2Ftmp%2Fbackground\x07$ \x1b]133;B\x07background notice",
        );
        terminal.process_input(b"\r\n\x1b]133;A\x07$ ");

        // A command that reached C but lost D is retained at the following A.
        // The shell's truncation evidence must survive that recovery path.
        terminal.process_input(b"\x1b]133;B\x07long-running-command\r\n");
        terminal.process_input(
            b"\x1b]133;C;cmdline_url=long-running-command;cmd_truncated=true;cwd_url=%2Fsrv%2Fwork\x07partial output\r\n",
        );
        terminal.process_input(b"\x1b]133;A\x07$ ");

        let snapshot = snapshot_session(&terminal, 17).expect("snapshot real block lifecycles");
        assert_eq!(snapshot.pane_session_id, 17);
        assert_eq!(snapshot.blocks.len(), 3);

        let completed = &snapshot.blocks[0];
        assert_eq!(completed.id, 0);
        assert_eq!(completed.command.as_deref(), Some("printf done"));
        assert_eq!(completed.output, "done");
        assert!(!completed.output_truncated);
        assert!(!completed.output_unavailable);
        assert_eq!(completed.exit_code, Some(7));
        assert_eq!(completed.duration_ms, Some(1_250));
        assert!(completed.finished_at_ms.is_some());
        assert_eq!(completed.cwd.as_deref(), Some("/tmp/job"));
        assert!(!completed.command_truncated);
        assert!(completed.completion_observed);

        let background = &snapshot.blocks[1];
        assert_eq!(background.id, 1);
        assert_eq!(background.command, None);
        assert_eq!(background.output, "background notice\n");
        assert!(!background.output_truncated);
        assert!(!background.output_unavailable);
        assert_eq!(background.exit_code, None);
        assert_eq!(background.duration_ms, None);
        assert!(background.finished_at_ms.is_some());
        assert_eq!(background.cwd.as_deref(), Some("/tmp/background"));
        assert!(!background.command_truncated);
        assert!(!background.start_mark_seen);
        assert_eq!(
            background.completion_provenance,
            block_mode::CompletionProvenance::BoundaryInferred
        );
        assert!(!background.completion_observed);

        let recovered = &snapshot.blocks[2];
        assert_eq!(recovered.id, 2);
        assert_eq!(recovered.command.as_deref(), Some("long-running-command"));
        assert_eq!(recovered.output, "partial output");
        assert!(!recovered.output_truncated);
        assert!(!recovered.output_unavailable);
        assert_eq!(recovered.exit_code, None);
        assert_eq!(recovered.duration_ms, None);
        assert_eq!(recovered.finished_at_ms, None);
        assert_eq!(recovered.cwd.as_deref(), Some("/srv/work"));
        assert!(recovered.command_truncated);
        assert!(recovered.start_mark_seen);
        assert_eq!(
            recovered.lifecycle_health,
            block_mode::BlockLifecycleHealth::Degraded
        );
        assert!(!recovered.completion_observed);

        let json: serde_json::Value = serde_json::from_slice(
            &serialize_session(&snapshot, SessionExportFormat::Json).unwrap(),
        )
        .unwrap();
        assert_eq!(json["retention"]["lifecycle_degraded"], 1);
        assert_eq!(json["retention"]["lifecycle_recovered"], 0);
        assert_eq!(json["retention"]["lifecycle_incomplete"], 0);
    }

    #[test]
    fn right_click_snapshot_exports_only_the_exact_block() {
        let mut terminal = TerminalState::new(80, 12);
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07alpha-command\r\n");
        terminal.process_input(
            b"\x1b]133;C;cmdline_url=alpha-command\x07alpha-output\r\n\x1b]133;D;exit_code=0\x07",
        );
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07beta-command\r\n");
        terminal.process_input(
            b"\x1b]133;C;cmdline_url=beta-command\x07beta-output\r\n\x1b]133;D;exit_code=1\x07",
        );

        let exact = snapshot_block(&terminal, 9, 0).expect("snapshot clicked alpha block");
        assert_eq!(exact.blocks.len(), 1);
        assert_eq!(exact.blocks[0].id, 0);
        for format in [SessionExportFormat::Markdown, SessionExportFormat::Json] {
            let document = String::from_utf8(serialize_session(&exact, format).unwrap()).unwrap();
            assert!(document.contains("alpha-command"));
            assert!(document.contains("alpha-output"));
            assert!(!document.contains("beta-command"));
            assert!(!document.contains("beta-output"));
        }
    }

    #[test]
    fn selected_snapshot_is_sparse_ordered_and_explicit_in_both_formats() {
        let mut terminal = TerminalState::new(80, 12);
        for (command, output) in [
            ("alpha", "first"),
            ("unrelated", "private-middle"),
            ("omega", "last"),
        ] {
            terminal.process_input(format!(
                "\u{1b}]133;A\u{7}$ \u{1b}]133;B\u{7}{command}\r\n\u{1b}]133;C;cmdline_url={command}\u{7}{output}\r\n\u{1b}]133;D;exit_code=0\u{7}"
            ).as_bytes());
        }
        let selected = snapshot_selected(&terminal, 42, &[2, 0]).unwrap();
        assert!(selected.selection_only);
        assert_eq!(
            selected
                .blocks
                .iter()
                .map(|block| block.id)
                .collect::<Vec<_>>(),
            [0, 2]
        );
        for format in [SessionExportFormat::Markdown, SessionExportFormat::Json] {
            let document =
                String::from_utf8(serialize_session(&selected, format).unwrap()).unwrap();
            assert!(document.contains("alpha") && document.contains("omega"));
            assert!(!document.contains("unrelated") && !document.contains("private-middle"));
            if format == SessionExportFormat::Markdown {
                assert!(document.starts_with("# Terminal Selection Export"));
                assert!(document.contains("Terminal block ID: 2"));
            } else {
                let json: serde_json::Value = serde_json::from_str(&document).unwrap();
                assert_eq!(json["scope"], "selected_blocks");
                assert_eq!(json["blocks"][1]["id"], 2);
            }
        }
    }

    #[test]
    fn selected_snapshot_refuses_missing_duplicate_empty_and_over_budget_sets() {
        let mut terminal = TerminalState::new(80, 12);
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07echo\r\n\x1b]133;C;cmdline_url=echo\x07done\r\n\x1b]133;D;exit_code=0\x07");
        assert!(snapshot_selected(&terminal, 42, &[]).is_err());
        assert!(snapshot_selected(&terminal, 42, &[0, 0]).is_err());
        assert!(snapshot_selected(&terminal, 42, &[0, 999]).is_err());
        assert!(snapshot_selected(&terminal, 42, &vec![0; MAX_SELECTED_BLOCKS + 1]).is_err());
        assert_eq!(
            snapshot_selected(&terminal, 42, &[0]).unwrap().blocks.len(),
            1
        );
    }

    #[test]
    fn selected_export_preserves_eviction_even_while_old_rows_are_available() {
        let mut terminal = TerminalState::new(80, 12);
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07echo\r\n\x1b]133;C;cmdline_url=echo\x07retained rows\r\n\x1b]133;D;exit_code=0\x07");
        terminal.command_zones[0].captured_output = None;
        terminal.command_zones[0].captured_output_evicted = true;
        let selected = snapshot_selected(&terminal, 42, &[0]).unwrap();
        assert!(selected.blocks[0].output_unavailable);
        assert!(selected.blocks[0].output.is_empty());
        let json: serde_json::Value = serde_json::from_slice(
            &serialize_session(&selected, SessionExportFormat::Json).unwrap(),
        )
        .unwrap();
        assert_eq!(json["retention"]["output_unavailable"], 1);
    }

    #[test]
    fn session_documents_keep_order_and_shared_block_metadata() {
        let snapshot = snapshot(vec![block(7, "cargo test", "ok"), block(9, "pwd", "/tmp")]);
        let markdown =
            String::from_utf8(serialize_session(&snapshot, SessionExportFormat::Markdown).unwrap())
                .unwrap();
        assert!(markdown.starts_with(
            "# Terminal Session Export\n\nPane session: 42\nCaptured: 1970-01-01 00:00:00 +00:00\nTotal retained blocks: 2\n"
        ));
        assert!(markdown.find("cargo test").unwrap() < markdown.find("pwd").unwrap());
        assert!(markdown.contains("- Finished: 1970-01-01 00:00:00 +00:00"));

        let json: serde_json::Value = serde_json::from_slice(
            &serialize_session(&snapshot, SessionExportFormat::Json).unwrap(),
        )
        .unwrap();
        assert_eq!(json["schema"], SESSION_EXPORT_SCHEMA);
        assert_eq!(json["version"], SESSION_EXPORT_SCHEMA_VERSION);
        assert_eq!(json["pane_session_id"], 42);
        assert_eq!(json["captured_at_ms"], 0);
        assert_eq!(json["captured_tz_offset_secs"], 0);
        assert_eq!(json["block_order"], "oldest_first");
        assert_eq!(json["retention"]["retained_blocks"], 2);
        assert_eq!(json["blocks"][0]["id"], 7);
        assert_eq!(json["blocks"][1]["command"], "pwd");
        assert_eq!(json["blocks"][0]["output"], "ok");
        assert_eq!(json["blocks"][0]["output_unavailable"], false);
        assert_eq!(json["blocks"][0]["completion_observed"], true);
        assert_eq!(json["blocks"][0]["completion_provenance"], "shell_reported");
        assert_eq!(json["blocks"][0]["lifecycle_health"], "healthy");
    }

    #[test]
    fn serialize_session_refuses_spoofed_command_or_output() {
        for format in [SessionExportFormat::Markdown, SessionExportFormat::Json] {
            assert!(serialize_session(
                &snapshot(vec![block(1, "git \u{202e}status", "ok")]),
                format
            )
            .is_err());
            assert!(serialize_session(
                &snapshot(vec![block(1, "git status", "ok\u{fffd}")]),
                format
            )
            .is_err());
        }
    }

    #[test]
    fn exports_disclose_truncated_commands_and_unavailable_output() {
        let mut retained = block(7, "partial-command", "");
        retained.command_truncated = true;
        retained.output_unavailable = true;
        retained.completion_observed = false;
        retained.completion_provenance = block_mode::CompletionProvenance::BoundaryInferred;
        retained.lifecycle_health = block_mode::BlockLifecycleHealth::Degraded;
        let mut clipped = block(8, "large-output", "retained prefix");
        clipped.output_truncated = true;

        let snapshot = snapshot(vec![retained.clone(), clipped]);
        let markdown =
            String::from_utf8(serialize_session(&snapshot, SessionExportFormat::Markdown).unwrap())
                .unwrap();
        assert!(markdown.contains("- Note: command truncated or unavailable"));
        assert!(markdown.contains("- Note: output unavailable"));
        assert!(markdown.contains("- Completion: inferred at next shell boundary"));

        let json: serde_json::Value = serde_json::from_slice(
            &serialize_session(&snapshot, SessionExportFormat::Json).unwrap(),
        )
        .unwrap();
        assert_eq!(json["retention"]["command_truncated"], 1);
        assert_eq!(json["retention"]["output_truncated"], 1);
        assert_eq!(json["retention"]["output_unavailable"], 1);
        assert_eq!(json["retention"]["completion_unobserved"], 1);
        assert_eq!(json["retention"]["lifecycle_degraded"], 1);
        assert_eq!(json["blocks"][0]["command_truncated"], true);
        assert_eq!(json["blocks"][0]["output_unavailable"], true);
        assert_eq!(json["blocks"][0]["completion_observed"], false);
        assert_eq!(
            json["blocks"][0]["completion_provenance"],
            "boundary_inferred"
        );
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
