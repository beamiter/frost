#!/usr/bin/env python3
"""Pixel calibration and owned PTY producer; no installed UI runs here."""
import errno
import fcntl
import json
import os
from pathlib import Path
import pty
import struct
import subprocess
import sys
import tempfile
import termios
import time
import unittest

from PIL import Image, ImageDraw

from native_page_up import ROW_COUNT, calibrate_grid, first_complete_return_sample
from native_row_witness import Frame, InvalidWitness, marker_colors, row_ids


def image_with_markers(ids, x, y, cell_width, cell_height):
    image = Image.new('RGB', (400, 200), (0, 0, 0))
    draw = ImageDraw.Draw(image)
    for i, row in enumerate(ids):
        for j, color in enumerate(marker_colors(row)):
            left, top = x + j * cell_width, y + i * cell_height
            draw.rectangle((left, top, left + cell_width - 1, top + cell_height - 1),
                           fill=color)
    return image, tuple(ids)


class PageCalibrationTest(unittest.TestCase):
    def test_accepts_one_blended_seam_but_rejects_ambiguous_edges(self):
        # Native X11 raster alternates 7–8 solid pixels and one mixed
        # boundary pixel. The five RGB interiors must remain exact.
        rows = tuple(range(269, 277))
        seams = ((85, 29, 158), (35, 53, 179), (153, 132, 179), (61, 165, 126))
        for width, fill, accepted in (
            (1, seams[0], True),
            (2, seams[0], False),
            (1, marker_colors(269)[-1], False),
        ):
            with self.subTest(seam_width=width, seam_color=fill):
                image = Image.new('RGB', (400, 200), (0, 0, 0))
                draw = ImageDraw.Draw(image)
                for i, row in enumerate(rows):
                    left, top = 30, 20 + i * 16
                    for j, color in enumerate(marker_colors(row)):
                        length = 8 - j % 2
                        draw.rectangle((left, top, left + length - 1, top + 15), fill=color)
                        left += length
                        if j < 4:
                            seam = seams[j] if accepted else fill
                            draw.rectangle((left, top, left + width - 1, top + 15), fill=seam)
                            left += width
                if not accepted:
                    with self.assertRaises(InvalidWitness):
                        calibrate_grid(image, ('same-window',), rows)
                    continue
                grid = calibrate_grid(image, ('same-window',), rows)
                self.assertEqual(row_ids(Frame(grid, image, 100, 101)), rows)
                with self.assertRaises(InvalidWitness):
                    calibrate_grid(image, ('same-window',), rows[:-1])
                # Changed grid/marker centers on one row cannot pass.
                image.paste(image.crop((29, 36, 90, 52)), (28, 36))
                with self.assertRaises(InvalidWitness):
                    calibrate_grid(image, ('same-window',), rows)

    def test_calibrates_two_geometries_with_repeated_hex_digits(self):
        for box in ((13, 26, 6, 8), (31, 49, 9, 13)):
            img, expected = image_with_markers(range(252, 260), *box)
            grid = calibrate_grid(img, ('same-window',), expected)
            self.assertEqual(row_ids(Frame(grid, img, 100, 101)), expected)
            self.assertEqual(grid.x[0], box[0] + (box[2] - 1) // 2)
            self.assertEqual(grid.y[0], box[1] + (box[3] - 1) // 2)

    def test_wrong_or_duplicate_grid_does_not_calibrate(self):
        img, seq = image_with_markers(range(253, 260), 13, 26, 6, 8)
        with self.assertRaises(InvalidWitness):
            calibrate_grid(img, ('same-window',), tuple(range(252, 260)))
        copy = img.crop((10, 0, 60, img.height))
        img.paste(copy, (70, 0))
        with self.assertRaisesRegex(InvalidWitness, 'Multiple synthetic'):
            calibrate_grid(img, ('same-window',), seq)


class TabReturnFirstCompleteTest(unittest.TestCase):
    def test_fully_marked_noncontiguous_return_fails_without_retry(self):
        exact, parked = image_with_markers(range(239, 243), 13, 26, 6, 8)
        grid = calibrate_grid(exact, ('same-window',), parked)
        for rows in ((239, 240, 271, 272), (239, 240, 240, 242)):
            with self.subTest(rows=rows):
                mixed, _ = image_with_markers(rows, 13, 26, 6, 8)
                frame = Frame(grid, mixed, 120, 130)
                with self.assertRaisesRegex(InvalidWitness, 'not a contiguous'):
                    row_ids(frame)  # Ordinary PageUp still needs a contiguous span.
                with self.assertRaisesRegex(InvalidWitness, 'not retained row'):
                    first_complete_return_sample(frame, grid, parked, (100, 110),
                                                 target_before=True, target_after=True)

    def test_exact_parked_rows_are_required_at_the_first_complete_sample(self):
        parked_image, parked = image_with_markers(range(239, 247), 13, 26, 6, 8)
        tail_image, _ = image_with_markers(range(269, 277), 13, 26, 6, 8)
        grid = calibrate_grid(parked_image, ('same-window',), parked)
        target = dict(target_before=True, target_after=True)
        first = Frame(grid, parked_image, 120, 130)
        self.assertEqual(first_complete_return_sample(first, grid, parked, (100, 110), **target),
                         parked)
        # Do not retry a complete but incorrect tail frame until the right one appears.
        with self.assertRaisesRegex(InvalidWitness, 'starts at 269'):
            first_complete_return_sample(Frame(grid, tail_image, 120, 130), grid, parked,
                                         (100, 110), **target)

    def test_only_title_bracketed_well_ordered_complete_samples_count(self):
        image, rows = image_with_markers(range(239, 247), 13, 26, 6, 8)
        grid = calibrate_grid(image, ('same-window',), rows)
        first = Frame(grid, image, 120, 130)
        for before, after in ((False, True), (True, False)):
            self.assertIsNone(first_complete_return_sample(
                first, grid, rows, (100, 110), target_before=before, target_after=after))
        image.putpixel((grid.x[0], grid.y[0]), (0, 0, 0))
        self.assertIsNone(first_complete_return_sample(
            first, grid, rows, (100, 110), target_before=True, target_after=True))
        with self.assertRaisesRegex(InvalidWitness, 'did not follow'):
            first_complete_return_sample(first, grid, rows, (100, 125),
                                         target_before=True, target_after=True)
        wrong_target = calibrate_grid(
            image_with_markers(rows, 13, 26, 6, 8)[0], ('other-window',), rows)
        with self.assertRaisesRegex(InvalidWitness, 'grid identity'):
            first_complete_return_sample(first, wrong_target, rows, (100, 110),
                                         target_before=True, target_after=True)


class OrdinalProducerTest(unittest.TestCase):
    def test_owned_pty_has_bounded_gates_and_detects_forwarded_pageup(self):
        for keys in (b'', b'\x1b[5~'):
            with self.subTest(forwarded=len(keys)), tempfile.TemporaryDirectory() as d:
                root = Path(d)
                master, slave = pty.openpty()
                data = bytearray()
                child = None
                try:
                    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 28, 80, 0, 0))
                    child = subprocess.Popen([sys.executable, str(
                        Path(__file__).with_name('native_page_up.py')), str(root)],
                        stdin=slave, stdout=slave, stderr=subprocess.PIPE)
                    os.set_blocking(master, False)

                    def wait_receipt(filename):
                        deadline = time.monotonic() + 4
                        while time.monotonic() < deadline:
                            try:
                                data.extend(os.read(master, 8192))
                            except OSError as ex:
                                if ex.errno not in (errno.EAGAIN, errno.EIO):
                                    raise
                            path = root / filename
                            if path.exists():
                                return json.loads(path.read_text())
                            if child.poll() is not None:
                                self.fail('Synthetic producer unexpectedly exited')
                            time.sleep(.005)
                        self.fail('Missing producer receipt: ' + filename)

                    ready = wait_receipt('ordinal-ready.json')
                    self.assertFalse((root / 'ordinal-flush.json').exists())
                    (root / 'emit-ordinal-rows').touch()
                    flushed = wait_receipt('ordinal-flush.json')
                    self.assertEqual(flushed['last'], ROW_COUNT)
                    self.assertEqual((flushed['rows'], flushed['cols']), (28, 80))
                    self.assertEqual(ready['pid'], flushed['pid'])
                    if keys:
                        os.write(master, keys)
                    (root / 'check-ordinal-input').touch()
                    received = wait_receipt('ordinal-input.json')
                    self.assertIn(b'Frost ordinal 0300', data)
                    self.assertNotIn(b'\x1b[?1049h', data)
                    self.assertEqual(received['input_bytes'], len(keys))
                    self.assertEqual(received['tty'], flushed['tty'])
                    (root / 'producer-finish').touch()
                    self.assertEqual(child.wait(timeout=3), 0)
                finally:
                    if child is not None:
                        if child.poll() is None:
                            child.kill()
                            child.wait(timeout=1)
                        child.stderr.close()
                    os.close(master)
                    os.close(slave)


if __name__ == '__main__':
    unittest.main()
