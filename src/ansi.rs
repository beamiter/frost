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
                    let params_start = index + 2;
                    let mut final_index = params_start;
                    while final_index < bytes.len() && !(0x40..=0x7e).contains(&bytes[final_index])
                    {
                        final_index += 1;
                    }
                    if final_index >= bytes.len() {
                        break;
                    }
                    let params = &bytes[params_start..final_index];
                    let command = bytes[final_index];
                    index = final_index + 1;
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
                        b'J' => match params {
                            b"" | b"0" => {
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
                            b"1" => {
                                clear_rows_before(&mut grid, row, &mut allocated_cells);
                                if let Some(cells) = grid.get_mut(&row) {
                                    // ED1 includes the cursor cell.
                                    let end = col.saturating_add(1).min(cells.len());
                                    if spend_work(&mut work_remaining, end, &mut truncated) {
                                        cells[..end].fill(' ');
                                    }
                                }
                            }
                            b"2" | b"3" => {
                                grid.clear();
                                allocated_cells = 0;
                                row_extent = 1;
                                row = 0;
                                col = 0;
                            }
                            _ => {}
                        },
                        b'K' => {
                            row_extent = row_extent.max(row + 1);
                            match params {
                                b"" | b"0" => {
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
                                b"1" => {
                                    if let Some(cells) = grid.get_mut(&row) {
                                        // EL1 includes the cursor cell.
                                        let end = col.saturating_add(1).min(cells.len());
                                        if spend_work(&mut work_remaining, end, &mut truncated) {
                                            cells[..end].fill(' ');
                                        }
                                    }
                                }
                                b"2" => {
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
    use super::{release_row_suffix, terminal_plain_text};

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
}
