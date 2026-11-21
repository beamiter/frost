#!/usr/bin/env python3
import unittest

from PIL import Image, ImageDraw

from native_row_witness import (
    Frame, Grid, InvalidWitness, marker_ansi, marker_colors, row_ids,
    verify_page_up,
)

GRID = Grid(('fixture-pid', 'window', 'owned-pty', 'geometry'), (6, 12, 18, 24, 30),
            tuple(6 + i * 6 for i in range(8)))
NORMAL = (12, 19, 24)


def frame(first, started, finished, *, geometry=GRID, row_indexes=None):
    image = Image.new('RGB', (70, 56), NORMAL)
    draw = ImageDraw.Draw(image)
    ids = row_indexes if row_indexes is not None else range(first, first + len(geometry.y))
    for row, y in zip(ids, geometry.y):
        for x, rgb in zip(geometry.x, marker_colors(row)):
            draw.rectangle((x - 1, y - 1, x + 1, y + 1), fill=rgb)
    return Frame(geometry, image, started, finished)


class NativeRowWitnessTests(unittest.TestCase):
    def test_marker_encoding_matches_pixel_decoder_at_hex_boundary(self):
        expected = marker_colors(256)
        self.assertEqual(marker_ansi(256).count('\x1b[48;2;'), len(expected))
        self.assertEqual(row_ids(frame(252, 10, 11)), tuple(range(252, 260)))

    def test_six_rows_for_8_row_pty(self):
        before, after = frame(119, 10, 11), frame(113, 15, 16)
        self.assertEqual(verify_page_up(before, after, [('Prior', 12, 13)], pty_rows=8, ownership='frost-scrollback'),
                         {'page_rows': 6, 'before_first': 119, 'after_first': 113})

    def test_short_page_lost_page_replay_mixed_and_wrong_grid_reject(self):
        before = frame(119, 10, 11)
        cases = [frame(115, 15, 16), frame(119, 15, 16), frame(1, 15, 16),
                 frame(113, 15, 16, row_indexes=[113, 114, 115, 116, 123, 124, 125, 126]),
                 frame(113, 15, 16, geometry=Grid(('wrong PID',), GRID.x, GRID.y))]
        for bad in cases:
            with self.subTest(first=bad.grid.target), self.assertRaises(InvalidWitness):
                verify_page_up(before, bad, [('Prior', 12, 13)], pty_rows=8, ownership='frost-scrollback')

    def test_frost_page_size_does_not_certify_fullscreen(self):
        before, after = frame(119, 10, 11), frame(113, 15, 16)
        with self.assertRaisesRegex(InvalidWitness, 'only to Frost-owned'):
            verify_page_up(before, after, [('Prior', 12, 13)], pty_rows=8,
                           ownership='codex-fullscreen')

    def test_malformed_marker_bracketing_and_unquiet_producer_reject(self):
        a, b = frame(119, 10, 11), frame(113, 15, 16)
        for keys, pty_rows, writes in [([('ctrl+Prior', 12, 13)], 8, ()),
                                        ([('Prior', 8, 9)], 8, ()),
                                        ([('Prior', 12, 13)] * 2, 8, ()),
                                        ([('Prior', 12, 13)], 7, ()),
                                        ([('Prior', 12, 13)], 8, (14,))]:
            with self.subTest(keys=keys), self.assertRaises(InvalidWitness):
                verify_page_up(a, b, keys, pty_rows=pty_rows,
                               ownership='frost-scrollback', fixture_flushes=writes)
        b.image.putpixel((6, 6), NORMAL)
        with self.assertRaisesRegex(InvalidWitness, 'Ambiguous paint'):
            verify_page_up(a, b, [('Prior', 12, 13)], pty_rows=8, ownership='frost-scrollback')


if __name__ == '__main__':
    unittest.main()
