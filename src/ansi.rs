//! Bounded ANSI-to-plain-text reconstruction for retained terminal output.
//!
//! This is intentionally independent from the live terminal grid: block
//! snapshots must survive resize and scrollback eviction before their OSC 133
//! prompt boundary arrives. The cursor model mirrors the Anvil/Forge plain
//! shadow closely enough to collapse carriage-return progress and repainting
//! without ever allocating from untrusted cursor coordinates without a cap.

use std::collections::BTreeMap;
use unicode_normalization::UnicodeNormalization;

const MAX_GRID_ROWS: usize = 100_000;
const MAX_GRID_COLS: usize = 10_000;

fn parse_param(params: &[u8], index: usize, default: usize) -> usize {
    let Some(field) = params.split(|&byte| byte == b';').nth(index) else {
        return default;
    };
    if field.is_empty() {
        return default;
    }
    let mut value = 0usize;
    for &byte in field {
        if !byte.is_ascii_digit() {
            return default;
        }
        value = value
            .saturating_mul(10)
            .saturating_add(usize::from(byte - b'0'));
    }
    if value == 0 {
        default
    } else {
        value
    }
}

fn skip_control_string(bytes: &[u8], mut index: usize, bell_terminates: bool) -> usize {
    index += 2;
    while index < bytes.len() {
        if bell_terminates && bytes[index] == b'\x07' {
            return index + 1;
        }
        if bytes[index] == b'\x1b' && index + 1 < bytes.len() && bytes[index + 1] == b'\\' {
            return index + 2;
        }
        index += 1;
    }
    index
}

fn skip_escape(input: &str, index: usize) -> usize {
    let bytes = input.as_bytes();
    if index + 1 >= bytes.len() {
        return index + 1;
    }
    match bytes[index + 1] {
        b']' => skip_control_string(bytes, index, true),
        b'P' | b'X' | b'^' | b'_' => skip_control_string(bytes, index, false),
        b'[' => {
            let mut next = index + 2;
            while next < bytes.len() && !(0x40..=0x7e).contains(&bytes[next]) {
                next += 1;
            }
            (next + usize::from(next < bytes.len())).min(bytes.len())
        }
        byte if (0x20..=0x2f).contains(&byte) => {
            let mut next = index + 2;
            while next < bytes.len() && (0x20..=0x2f).contains(&bytes[next]) {
                next += 1;
            }
            if next < bytes.len() && (0x30..=0x7e).contains(&bytes[next]) {
                next += 1;
            }
            next
        }
        // The live parser consumes ESC plus the next byte for an unknown
        // escape. When that byte starts UTF-8, also skip its continuation
        // bytes: landing mid-scalar would panic, while printing the scalar
        // would diverge from the live screen.
        byte if byte.is_ascii() => (index + 2).min(bytes.len()),
        _ => {
            let scalar_len = input[index + 1..].chars().next().map_or(1, char::len_utf8);
            (index + 1 + scalar_len).min(bytes.len())
        }
    }
}

fn spend_work(remaining: &mut usize, amount: usize, truncated: &mut bool) -> bool {
    if amount > *remaining {
        *remaining = 0;
        *truncated = true;
        false
    } else {
        *remaining -= amount;
        true
    }
}

fn clear_rows_from(
    grid: &mut BTreeMap<usize, Vec<char>>,
    first: usize,
    allocated_cells: &mut usize,
) {
    let removed = grid.split_off(&first);
    *allocated_cells =
        allocated_cells.saturating_sub(removed.values().map(Vec::len).sum::<usize>());
}

fn clear_rows_before(
    grid: &mut BTreeMap<usize, Vec<char>>,
    first: usize,
    allocated_cells: &mut usize,
) {
    let retained = grid.split_off(&first);
    let removed = std::mem::replace(grid, retained);
    *allocated_cells =
        allocated_cells.saturating_sub(removed.values().map(Vec::len).sum::<usize>());
}

/// Truncate one row while also releasing the removed cells' backing storage.
///
/// `Vec::truncate` and `Vec::clear` retain capacity. That is normally useful,
/// but here cursor coordinates are untrusted: repeatedly allocating a wide
/// row and erasing it could otherwise keep far more heap than the logical
/// `allocated_cells` budget accounts for. Boxing the retained prefix before
/// converting it back gives the replacement vector no stale spare capacity.
fn release_row_suffix(
    cells: &mut Vec<char>,
    first_removed: usize,
    allocated_cells: &mut usize,
    work_remaining: &mut usize,
    truncated: &mut bool,
) {
    if first_removed >= cells.len() {
        return;
    }
    let old_len = cells.len();
    // Erasing the continuation also erases its wide lead, just as clear_cell
    // does in the live grid. Keep its column as a blank in the retained prefix.
    if cells[first_removed] == '\0' && first_removed > 0 {
        cells[first_removed - 1] = ' ';
    }
    if first_removed == 0 {
        *cells = Vec::new();
    } else if spend_work(work_remaining, first_removed, truncated) {
        *cells = cells[..first_removed]
            .to_vec()
            .into_boxed_slice()
            .into_vec();
    } else {
        // Preserving the prefix would exceed the bounded reconstruction work
        // budget. Drop the row instead; callers see the truncation flag and
        // the heap still returns to its accounted bound.
        *cells = Vec::new();
        *allocated_cells = allocated_cells.saturating_sub(old_len);
        return;
    }
    *allocated_cells = allocated_cells.saturating_sub(old_len - first_removed);
}

/// Erase through the cursor, including the continuation of a wide lead.
fn erase_row_prefix(
    cells: &mut [char],
    end: usize,
    work_remaining: &mut usize,
    truncated: &mut bool,
) {
    let mut end = end.min(cells.len());
    if end > 0 && cells.get(end) == Some(&'\0') {
        end += 1;
    }
    if spend_work(work_remaining, end, truncated) {
        cells[..end].fill(' ');
    }
}

fn combine_with_previous(
    grid: &mut BTreeMap<usize, Vec<char>>,
    row: usize,
    col: usize,
    mark: char,
) {
    let Some(cells) = grid.get_mut(&row) else {
        return;
    };
    if col == 0 || col > cells.len() {
        return;
    }
    let mut base_col = col - 1;
    if cells.get(base_col) == Some(&'\0') && base_col > 0 {
        base_col -= 1;
    }
    let Some(base) = cells.get_mut(base_col) else {
        return;
    };
    if *base == '\0' {
        return;
    }
    let mut combined = String::with_capacity(8);
    combined.push(*base);
    combined.push(mark);
    let normalized: String = combined.nfc().collect();
    let mut chars = normalized.chars();
    if let (Some(character), None) = (chars.next(), chars.next()) {
        *base = character;
    }
}

/// Apply terminal cursor/erase controls and return the final plain screen.
///
/// The returned flag is true when either cursor coordinates, reconstructed
/// cells, or UTF-8 output exceeded `max_bytes`; callers propagate it as the
/// block snapshot's truncation bit. The output itself never exceeds the cap.
pub(crate) fn terminal_plain_text(input: &str, max_bytes: usize) -> (String, bool) {
    let bytes = input.as_bytes();
    let row_limit = MAX_GRID_ROWS.min(max_bytes.saturating_add(1).max(1));
    let col_limit = MAX_GRID_COLS.min(max_bytes.max(1));
    // Sparse rows prevent cursor movement across blank space from allocating
    // or repeatedly scanning thousands of empty vectors.
    let mut grid: BTreeMap<usize, Vec<char>> = BTreeMap::new();
    let mut row_extent = 1usize;
    let mut allocated_cells = 0usize;
    let mut row = 0usize;
    let mut col = 0usize;
    let mut saved_cursor = (0usize, 0usize);
    let mut index = 0usize;
    let mut truncated = false;
    // Raw idle output is capped at eight times the final 1 MiB snapshot.
    // Apply that same ratio to amplified cell work so short cursor/erase
    // loops cannot force unbounded gap filling or prefix copying.
    let mut work_remaining = max_bytes.saturating_mul(8);

    let write_char = |grid: &mut BTreeMap<usize, Vec<char>>,
                      allocated_cells: &mut usize,
                      work_remaining: &mut usize,
                      row_extent: &mut usize,
                      row: usize,
                      col: usize,
                      ch: char,
                      width: usize,
                      truncated: &mut bool| {
        if row >= row_limit || col.saturating_add(width) > col_limit {
            *truncated = true;
            return;
        }
        *row_extent = (*row_extent).max(row + 1);
        let cells = grid.entry(row).or_default();
        let target_len = col + width;
        let needed = target_len.saturating_sub(cells.len());
        if allocated_cells.saturating_add(needed) > max_bytes {
            *truncated = true;
            return;
        }
        if !spend_work(work_remaining, needed, truncated) {
            return;
        }
        if needed > 0 {
            cells.resize(target_len, ' ');
        }
        *allocated_cells += needed;

        // Preserve a minimal wide-cell pair invariant. NUL is an internal
        // continuation sentinel and is never emitted into the plain result.
        let overlaps_next_wide =
            width == 2 && col + 2 < cells.len() && cells[col + 1] != '\0' && cells[col + 2] == '\0';
        if cells[col] == '\0' && col > 0 {
            cells[col - 1] = ' ';
        }
        if col + 1 < cells.len() && cells[col + 1] == '\0' {
            cells[col + 1] = ' ';
        }
        cells[col] = ch;
        if width == 2 {
            cells[col + 1] = '\0';
        }
        if overlaps_next_wide {
            cells[col + 2] = ' ';
        }
    };

    while index < bytes.len() {
        if bytes[index] == b'\x1b' && index + 1 < bytes.len() {
            match bytes[index + 1] {
                b'[' => {
                    // Match the live parser's bounded parameter collection:
                    // embedded C0 controls execute without becoming parameters.
                    let mut params = [0u8; 128];
                    let mut params_len = 0;
                    let mut command = None;
                    let mut cancelled = false;
                    index += 2;
                    while index < bytes.len() {
                        match bytes[index] {
                            byte @ 0x30..=0x3f => {
                                if params_len < params.len() {
                                    params[params_len] = byte;
                                    params_len += 1;
                                }
                            }
                            0x20..=0x2f => {}
                            byte @ 0x40..=0x7e => {
                                command = Some(byte);
                                index += 1;
                                break;
                            }
                            b'\x08' => col = col.saturating_sub(1),
                            b'\t' => {
                                let target = ((col / 8) + 1).saturating_mul(8);
                                if target >= col_limit {
                                    truncated = true;
                                }
                                col = target.min(col_limit - 1);
                            }
                            b'\n' | b'\x0b' | b'\x0c' => {
                                if row + 1 >= row_limit {
                                    truncated = true;
                                }
                                row = (row + 1).min(row_limit - 1);
                                row_extent = row_extent.max(row + 1);
                            }
                            b'\r' => col = 0,
                            b'\x18' | b'\x1a' => {
                                cancelled = true;
                                index += 1;
                                break;
                            }
                            b'\x1b' => {
                                cancelled = true;
                                break;
                            }
                            0x00..=0x1f => {}
                            _ => break,
                        }
                        index += 1;
                    }
                    if cancelled {
                        continue;
                    }
                    let Some(command) = command else {
                        break;
                    };
                    let params = &params[..params_len];
                    if matches!(params.first(), Some(0x3c..=0x3f)) {
                        continue;
                    }
                    match command {
                        b'H' | b'f' => {
                            let target_row = parse_param(params, 0, 1).saturating_sub(1);
                            let target_col = parse_param(params, 1, 1).saturating_sub(1);
                            if target_row >= row_limit || target_col >= col_limit {
                                truncated = true;
                            }
                            row = target_row.min(row_limit - 1);
                            col = target_col.min(col_limit - 1);
                        }
                        b'A' => row = row.saturating_sub(parse_param(params, 0, 1)),
                        b'B' | b'e' => {
                            let target = row.saturating_add(parse_param(params, 0, 1));
                            if target >= row_limit {
                                truncated = true;
                            }
                            row = target.min(row_limit - 1);
                        }
                        b'E' => {
                            let target = row.saturating_add(parse_param(params, 0, 1));
                            if target >= row_limit {
                                truncated = true;
                            }
                            row = target.min(row_limit - 1);
                            col = 0;
                        }
                        b'F' => {
                            row = row.saturating_sub(parse_param(params, 0, 1));
                            col = 0;
                        }
                        b'd' => {
                            let target = parse_param(params, 0, 1).saturating_sub(1);
                            if target >= row_limit {
                                truncated = true;
                            }
                            row = target.min(row_limit - 1);
                        }
                        b'C' | b'a' => {
                            let target = col.saturating_add(parse_param(params, 0, 1));
                            if target >= col_limit {
                                truncated = true;
                            }
                            col = target.min(col_limit - 1);
                        }
                        b'D' => col = col.saturating_sub(parse_param(params, 0, 1)),
                        b'G' | b'`' => {
                            let target = parse_param(params, 0, 1).saturating_sub(1);
                            if target >= col_limit {
                                truncated = true;
                            }
                            col = target.min(col_limit - 1);
                        }
                        b's' => saved_cursor = (row, col),
                        b'u' => (row, col) = saved_cursor,
                        b'J' => match parse_param(params, 0, 0) {
                            0 => {
                                if let Some(cells) = grid.get_mut(&row) {
                                    release_row_suffix(
                                        cells,
                                        col,
                                        &mut allocated_cells,
                                        &mut work_remaining,
                                        &mut truncated,
                                    );
                                }
                                if grid.get(&row).is_some_and(Vec::is_empty) {
                                    grid.remove(&row);
                                }
                                clear_rows_from(&mut grid, row + 1, &mut allocated_cells);
                                row_extent = row_extent.min(row + 1).max(1);
                            }
                            1 => {
                                clear_rows_before(&mut grid, row, &mut allocated_cells);
                                if let Some(cells) = grid.get_mut(&row) {
                                    // ED1 includes the cursor cell.
                                    erase_row_prefix(
                                        cells,
                                        col.saturating_add(1),
                                        &mut work_remaining,
                                        &mut truncated,
                                    );
                                }
                            }
                            2 => {
                                // ED2 erases the screen without moving the cursor.
                                grid.clear();
                                allocated_cells = 0;
                                row_extent = 1;
                            }
                            // ED3 erases saved lines only. This isolated model
                            // has no scrollback partition, so preserve its grid.
                            3 => {}
                            _ => {}
                        },
                        b'K' => {
                            row_extent = row_extent.max(row + 1);
                            match parse_param(params, 0, 0) {
                                0 => {
                                    if let Some(cells) = grid.get_mut(&row) {
                                        release_row_suffix(
                                            cells,
                                            col,
                                            &mut allocated_cells,
                                            &mut work_remaining,
                                            &mut truncated,
                                        );
                                    }
                                    if grid.get(&row).is_some_and(Vec::is_empty) {
                                        grid.remove(&row);
                                    }
                                }
                                1 => {
                                    if let Some(cells) = grid.get_mut(&row) {
                                        // EL1 includes the cursor cell.
                                        erase_row_prefix(
                                            cells,
                                            col.saturating_add(1),
                                            &mut work_remaining,
                                            &mut truncated,
                                        );
                                    }
                                }
                                2 => {
                                    if let Some(cells) = grid.remove(&row) {
                                        allocated_cells =
                                            allocated_cells.saturating_sub(cells.len());
                                    }
                                }
                                _ => {}
                            }
                        }
                        _ => {}
                    }
                }
                b']' => index = skip_control_string(bytes, index, true),
                b'P' | b'X' | b'^' | b'_' => {
                    index = skip_control_string(bytes, index, false);
                }
                b'7' => {
                    saved_cursor = (row, col);
                    index += 2;
                }
                b'8' => {
                    (row, col) = saved_cursor;
                    index += 2;
                }
                b'D' => {
                    if row + 1 >= row_limit {
                        truncated = true;
                    }
                    row = (row + 1).min(row_limit - 1);
                    index += 2;
                }
                b'E' => {
                    if row + 1 >= row_limit {
                        truncated = true;
                    }
                    row = (row + 1).min(row_limit - 1);
                    col = 0;
                    index += 2;
                }
                b'M' => {
                    row = row.saturating_sub(1);
                    index += 2;
                }
                _ => index = skip_escape(input, index),
            }
            continue;
        }

        match bytes[index] {
            b'\n' => {
                if row + 1 >= row_limit {
                    truncated = true;
                }
                row = (row + 1).min(row_limit - 1);
                col = 0;
                row_extent = row_extent.max(row + 1);
                index += 1;
            }
            b'\x0b' | b'\x0c' => {
                if row + 1 >= row_limit {
                    truncated = true;
                }
                row = (row + 1).min(row_limit - 1);
                row_extent = row_extent.max(row + 1);
                index += 1;
            }
            b'\r' => {
                col = 0;
                index += 1;
            }
            b'\x08' => {
                col = col.saturating_sub(1);
                index += 1;
            }
            b'\t' => {
                let target = ((col / 8) + 1).saturating_mul(8);
                if target >= col_limit {
                    truncated = true;
                }
                col = target.min(col_limit - 1);
                index += 1;
            }
            byte if byte < 0x20 || byte == 0x7f => index += 1,
            _ => {
                let ch = input[index..].chars().next().unwrap_or('\u{fffd}');
                index += ch.len_utf8();
                if !ch.is_control() {
                    let width = crate::char_width::cached_char_width(ch);
                    if width == 0 {
                        combine_with_previous(&mut grid, row, col, ch);
                        continue;
                    }
                    write_char(
                        &mut grid,
                        &mut allocated_cells,
                        &mut work_remaining,
                        &mut row_extent,
                        row,
                        col,
                        ch,
                        width,
                        &mut truncated,
                    );
                    col = col.saturating_add(width);
                }
            }
        }
    }

    let mut output = String::with_capacity(max_bytes.min(input.len()));
    'rows: for line_index in 0..row_extent {
        if line_index > 0 {
            if output.len() == max_bytes {
                truncated = true;
                break;
            }
            output.push('\n');
        }
        if let Some(cells) = grid.get(&line_index) {
            for &ch in cells {
                if ch == '\0' {
                    continue;
                }
                if output.len().saturating_add(ch.len_utf8()) > max_bytes {
                    truncated = true;
                    break 'rows;
                }
                output.push(ch);
            }
        }
    }
    (output, truncated)
}

#[cfg(test)]
mod tests {
    use super::{erase_row_prefix, release_row_suffix, terminal_plain_text};

    #[test]
    fn reconstructs_cursor_overwrites_and_erase() {
        assert_eq!(terminal_plain_text("foo\rbar", 1024), ("bar".into(), false));
        assert_eq!(
            terminal_plain_text("abcdef\rXY", 1024),
            ("XYcdef".into(), false)
        );
        assert_eq!(
            terminal_plain_text("Loading...\r\x1b[KDone", 1024),
            ("Done".into(), false)
        );
        assert_eq!(
            terminal_plain_text("old\nframe\x1b[Hnew\x1b[J", 1024),
            ("new".into(), false)
        );
    }

    #[test]
    fn strips_osc_dcs_and_apc_payloads() {
        let input = "\x1b]133;D;7\x07\x1bPprivate\x1b\\\x1b_Gbase64\x1b\\visible";
        assert_eq!(terminal_plain_text(input, 1024), ("visible".into(), false));
        // BEL terminates OSC, but is ordinary payload inside DCS/APC/SOS/PM;
        // only ST ends those strings, matching the live terminal parser.
        assert_eq!(
            terminal_plain_text("x\x1bPsecret\x07LEAK\x1b\\", 1024),
            ("x".into(), false)
        );
    }

    #[test]
    fn unknown_escape_before_utf8_stays_on_a_character_boundary() {
        assert_eq!(terminal_plain_text("\x1bé", 1024), (String::new(), false));
    }

    #[test]
    fn erase_to_beginning_includes_the_cursor_cell() {
        assert!(terminal_plain_text("x\r\x1b[1K", 1024).0.trim().is_empty());
        assert!(terminal_plain_text("x\r\x1b[1J", 1024).0.trim().is_empty());
    }

    #[test]
    fn unicode_width_matches_live_cells_and_drops_invisible_spoofing() {
        assert_eq!(
            terminal_plain_text("ok\u{202e}", 1024),
            ("ok".into(), false)
        );
        assert_eq!(
            terminal_plain_text("\u{200d}\u{feff}", 1024),
            (String::new(), false)
        );
        assert_eq!(terminal_plain_text("e\u{301}", 1024), ("é".into(), false));
        assert_eq!(
            terminal_plain_text("界\r\x1b[2CX", 1024),
            ("界X".into(), false)
        );
    }

    #[test]
    fn erased_rows_release_untrusted_spare_capacity() {
        let mut cells = Vec::with_capacity(10_000);
        cells.resize(10_000, 'x');
        let mut allocated = cells.len();
        let mut work_remaining = usize::MAX;
        let mut truncated = false;

        release_row_suffix(
            &mut cells,
            2,
            &mut allocated,
            &mut work_remaining,
            &mut truncated,
        );
        assert_eq!(cells, ['x', 'x']);
        assert_eq!(cells.capacity(), 2);
        assert_eq!(allocated, 2);

        release_row_suffix(
            &mut cells,
            0,
            &mut allocated,
            &mut work_remaining,
            &mut truncated,
        );
        assert!(cells.is_empty());
        assert_eq!(cells.capacity(), 0);
        assert_eq!(allocated, 0);
        assert!(!truncated);
    }

    #[test]
    fn sparse_rows_and_fuel_bound_erase_amplification() {
        let empty_rows = "\n".repeat(99_999);
        let erase_above = "\x1b[1J".repeat(10_000);
        let (_, row_truncated) = terminal_plain_text(&(empty_rows + &erase_above), 32);
        assert!(row_truncated);

        let repaint = "\x1b[1;32Hx\x1b[2K".repeat(1_000);
        let (text, repaint_truncated) = terminal_plain_text(&repaint, 32);
        assert!(repaint_truncated);
        assert!(text.len() <= 32);
    }

    #[test]
    fn output_and_untrusted_coordinates_are_bounded() {
        let (text, truncated) = terminal_plain_text("\x1b[999999;999999Hboom", 32);
        assert!(truncated);
        assert!(text.len() <= 32);
    }

    #[test]
    fn ed2_keeps_the_cursor_column() {
        assert_eq!(
            terminal_plain_text("old\x1b[2Jnew", 1024),
            ("   new".into(), false)
        );
    }
    #[test]
    fn ed2_keeps_the_cursor_row() {
        assert_eq!(
            terminal_plain_text("old\nxy\x1b[2JZ", 1024),
            ("\n  Z".into(), false)
        );
    }
    #[test]
    fn ed3_keeps_live_text_and_cursor() {
        assert_eq!(
            terminal_plain_text("keep\x1b[3J!", 1024),
            ("keep!".into(), false)
        );
    }
    #[test]
    fn numeric_erase_modes_accept_leading_zeros() {
        assert_eq!(
            terminal_plain_text("old\r\x1b[02JX", 1024),
            ("X".into(), false)
        );
        assert_eq!(
            terminal_plain_text("old\r\x1b[02KX", 1024),
            ("X".into(), false)
        );
    }
    #[test]
    fn erase_suffix_clears_a_split_wide_character() {
        for command in ['J', 'K'] {
            let input = format!("界tail\r\x1b[C\x1b[{command}X");
            assert_eq!(
                terminal_plain_text(&input, 1024),
                (" X".into(), false),
                "{command}"
            );
        }
    }
    #[test]
    fn erase_prefix_clears_a_split_wide_character() {
        for command in ['J', 'K'] {
            let input = format!("界X\r\x1b[1{command}");
            assert_eq!(
                terminal_plain_text(&input, 1024),
                ("  X".into(), false),
                "{command}"
            );
        }
    }
    #[test]
    fn csi_executes_embedded_carriage_return() {
        assert_eq!(
            terminal_plain_text("old\x1b[\r31mX", 1024),
            ("Xld".into(), false)
        );
    }
    #[test]
    fn csi_keeps_parameters_across_embedded_control() {
        assert_eq!(
            terminal_plain_text("old\x1b[\r2CX", 1024),
            ("olX".into(), false)
        );
    }
    #[test]
    fn vertical_tab_and_form_feed_advance_without_carriage_return() {
        for control in ['\x0b', '\x0c'] {
            assert_eq!(
                terminal_plain_text(&format!("x{control}y"), 1024),
                ("x\n y".into(), false)
            );
        }
    }

    #[test]
    fn screen_erase_preserves_saved_cursor_and_explicit_home_still_works() {
        assert_eq!(
            terminal_plain_text("abc\x1b7\x1b[2JX\x1b8Y", 1024),
            ("   Y".into(), false)
        );
        assert_eq!(
            terminal_plain_text("old\x1b[2J\x1b[Hnew", 1024),
            ("new".into(), false)
        );
        assert_eq!(
            terminal_plain_text("first\nlast\x1b7\x1b[3J!\x1b8?", 1024),
            ("first\nlast?".into(), false)
        );
    }

    #[test]
    fn erase_uses_the_first_numeric_parameter() {
        for command in ['J', 'K'] {
            for mode in ["", "0", "00", "0;99", ";"] {
                assert_eq!(
                    terminal_plain_text(&format!("old\r\x1b[{mode}{command}X"), 1024),
                    ("X".into(), false),
                    "mode={mode:?}, command={command}"
                );
            }
            for mode in ["1", "01", "1;99"] {
                assert_eq!(
                    terminal_plain_text(&format!("old\r\x1b[{mode}{command}"), 1024),
                    (" ld".into(), false),
                    "mode={mode:?}, command={command}"
                );
            }
            for mode in ["2", "002", "2;99"] {
                assert_eq!(
                    terminal_plain_text(&format!("old\r\x1b[{mode}{command}X"), 1024),
                    ("X".into(), false),
                    "mode={mode:?}, command={command}"
                );
            }
            assert_eq!(
                terminal_plain_text(&format!("old\r\x1b[99{command}X"), 1024),
                ("Xld".into(), false)
            );
        }
    }

    #[test]
    fn embedded_csi_controls_execute_once_and_cancellation_resumes() {
        for (control, expected) in [
            ('\x08', "olX"),
            ('\t', "old     X"),
            ('\n', "old\n   X"),
            ('\x0b', "old\n   X"),
            ('\x0c', "old\n   X"),
            ('\r', "Xld"),
            ('\x07', "oldX"),
        ] {
            assert_eq!(
                terminal_plain_text(&format!("old\x1b[3{control}1mX"), 1024),
                (expected.into(), false),
                "control={control:?}"
            );
        }
        for cancel in ['\x18', '\x1a'] {
            assert_eq!(
                terminal_plain_text(&format!("old\x1b[2{cancel}X"), 1024),
                ("oldX".into(), false)
            );
        }
        assert_eq!(
            terminal_plain_text("old\x1b[2\x1b[1GX", 1024),
            ("Xld".into(), false)
        );
        assert_eq!(
            terminal_plain_text("old\x1b[\r", 1024),
            ("old".into(), false)
        );
    }

    #[test]
    fn wide_erase_accounts_for_repaired_cells_and_retained_capacity() {
        let mut cells = vec!['界', '\0', 'x'];
        let mut work = 1;
        let mut truncated = false;
        erase_row_prefix(&mut cells, 1, &mut work, &mut truncated);
        assert!(truncated);
        assert_eq!(work, 0);
        assert_eq!(cells, ['界', '\0', 'x']);

        let mut work = 2;
        let mut truncated = false;
        erase_row_prefix(&mut cells, 1, &mut work, &mut truncated);
        assert!(!truncated);
        assert_eq!(work, 0);
        assert_eq!(cells, [' ', ' ', 'x']);

        let mut cells = vec!['界', '\0', 'x'];
        let mut allocated = 3;
        let mut work = 1;
        release_row_suffix(&mut cells, 1, &mut allocated, &mut work, &mut truncated);
        assert_eq!(cells, [' ']);
        assert_eq!(cells.capacity(), 1);
        assert_eq!(allocated, 1);
        assert_eq!(work, 0);
        assert!(!truncated);
    }

    #[test]
    fn all_scalar_boundaries_and_tiny_budgets_stay_safe() {
        let tokens = [
            "",
            "a",
            "界",
            "😀",
            "e\u{301}",
            "\u{202e}",
            "\u{009b}",
            "\x1b",
            "\x1b[",
            "\x1b[2J",
            "\x1b[3J",
            "\x1b[1K",
            "\x1b[\r31m",
            "\x1b[2\x18",
            "\x1b[2\x1b[1G",
            "\x1b[é",
            "\x1b%G",
            "\x1b%界",
            "\x1bé",
            "\x1b]hidden\x07",
            "\x1bPhidden\x1b\\",
            "\x1b_hidden\x1b\\",
            "\x1b[999999999999999999999999H",
            "\r",
            "\n",
            "\t",
            "\x0b",
            "\x0c",
            "\x1b7",
            "\x1b8",
        ];
        for first in tokens {
            for second in tokens {
                let input = format!("{first}{second}終");
                for end in input
                    .char_indices()
                    .map(|(index, _)| index)
                    .chain([input.len()])
                {
                    for cap in [0, 1, 2, 3, 4, 8, 32] {
                        let (text, _) = terminal_plain_text(&input[..end], cap);
                        assert!(text.len() <= cap, "input={input:?}, end={end}, cap={cap}");
                        assert!(!text.contains('\0'));
                        assert!(!text.contains('\x1b'));
                        assert!(text.is_char_boundary(text.len()));
                    }
                }
            }
        }
    }

    #[test]
    fn csi_parameter_overflow_matches_the_live_128_byte_prefix() {
        for command in ['J', 'K'] {
            for zero_count in [126, 127, 128, 129, 512] {
                let params = format!("{}2", "0".repeat(zero_count));
                let input = format!("old\r\x1b[C\x1b[{params}{command}X");
                // The live parser ignores parameter bytes after byte 128,
                // but still dispatches the final: 2 fits only below 128 zeros.
                let expected = if zero_count < 128 { " X" } else { "oX" };
                assert_eq!(
                    terminal_plain_text(&input, 1024),
                    (expected.into(), false),
                    "zeros={zero_count}, command={command}"
                );
            }
        }
        // Saturation must not hide C0 effects later in the same CSI.
        assert_eq!(
            terminal_plain_text(&format!("old\x1b[{}\r2JX", "0".repeat(128)), 1024),
            ("X".into(), false)
        );
    }

    #[test]
    fn saturated_csi_cancellation_and_non_erase_finals_do_not_erase() {
        let prefix = format!("old\r\x1b[C\x1b[{}2", "0".repeat(512));
        for suffix in ["\x18X", "\x1aX", "qX"] {
            assert_eq!(
                terminal_plain_text(&format!("{prefix}{suffix}"), 1024),
                ("oXd".into(), false),
                "suffix={suffix:?}"
            );
        }
        assert_eq!(
            terminal_plain_text(&format!("{prefix}\x1b[1GX"), 1024),
            ("Xld".into(), false)
        );
        assert_eq!(terminal_plain_text(&prefix, 1024), ("old".into(), false));
    }

    #[test]
    fn erase_with_intermediates_matches_the_live_final_dispatch() {
        // The live J/K arms do not guard intermediates, unlike s/u/r. Preserve
        // that contract even beyond the live parser's eight-byte buffer.
        for command in ['J', 'K'] {
            for intermediate in 0x20u8..=0x2f {
                for count in [1, 8, 9, 32] {
                    let intermediates = (intermediate as char).to_string().repeat(count);
                    let input = format!("old\r\x1b[C\x1b[2{intermediates}{command}X");
                    assert_eq!(
                        terminal_plain_text(&input, 1024),
                        (" X".into(), false),
                        "command={command}, intermediate={intermediate}, count={count}"
                    );
                    // An unsupported final remains unsupported even when its
                    // numeric parameter could otherwise look like an erase.
                    let input = format!("old\r\x1b[C\x1b[2{intermediates}yX");
                    assert_eq!(terminal_plain_text(&input, 1024), ("oXd".into(), false));
                }
            }
        }
    }
}
