//! Frozen pre-migration retained-output composition, using the original NFC body.
use super::*;
use unicode_normalization::UnicodeNormalization;

fn frozen_combine_with_previous(
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

#[test]
fn ansi_shared_composition_matches_frozen_output() {
    for base in [
        'e', 'A', '\u{00c5}', '\u{1100}', '\u{ac00}', ' ', '\0', '界', '\u{0344}', '\u{05c8}',
    ] {
        for cursor in 0..=5 {
            for has_row in [false, true] {
                let mut actual = BTreeMap::new();
                if has_row {
                    actual.insert(1, vec![base, '\0', base, base]);
                }
                let mut original = actual.clone();
                for mark in [
                    '\u{0301}', '\u{0308}', '\u{1161}', '\u{11a8}', '\u{fe0f}', '\u{200d}',
                    '\u{05c8}',
                ] {
                    combine_with_previous(&mut actual, 1, cursor, mark);
                    frozen_combine_with_previous(&mut original, 1, cursor, mark);
                    assert_eq!(
                        actual, original,
                        "base={base:?} col={cursor} row={has_row} mark={mark:?}"
                    );
                }
            }
        }
    }
}
