#!/usr/bin/env python3
"""Synthetic raster and owned Linux PTY checks for the held-drag driver."""
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import struct
import subprocess
import sys
import tempfile
import termios
import time
import unittest
from unittest.mock import patch

from PIL import Image, ImageDraw

import native_held_drag as driver
from native_held_drag import (
    APPEND_ROWS, DSR, DSR_REPLY, PRESS_PROBE, PROBE, ROW_COUNT, clipboard_matches,
    fixture_line, highlight, pressed_anchor, probe_geometry,
    verify_chronology,
)
from native_row_witness import Frame, Grid, InvalidWitness, marker_colors

ROWS = tuple(range(269, 277))
ROW = ROWS[4]
XY = (12, 24, 36, 48, 60)
YS = tuple(12 + n * 16 for n in range(len(ROWS)))
GRID = Grid(('native-fixture',), XY, YS)
PROBES = tuple(12 + col * 12 for col in (12, 15, 19))
SELECTED = (95, 109, 130)
ANCHOR_X = 12 + 6 * 12
PRESS_SELECTED = (84, 112, 89)


def raster(started, finished, selected=(), offsets=ROWS, probes=PROBES, pressed=False):
    img = Image.new('RGB', (260, 140), (0, 0, 0))
    draw = ImageDraw.Draw(img)
    for row, y in zip(offsets, YS):
        for x, color in zip(XY, marker_colors(row)):
            draw.rectangle((x - 1, y - 1, x + 1, y + 1), fill=color)
    y = YS[4]
    draw.rectangle((ANCHOR_X - 2, y - 2, ANCHOR_X + 2, y + 2),
                   fill=PRESS_SELECTED if pressed else PRESS_PROBE)
    for x in probes:
        draw.rectangle((x - 2, y - 2, x + 2, y + 2), fill=SELECTED if x in selected else PROBE)
    return Frame(GRID, img, started, finished)


class HeldDragWitnessTest(unittest.TestCase):
    def test_geometry_and_temporal_selection(self):
        baseline = raster(1, 2)
        y, probes, start_x, early_x, end_x = probe_geometry(baseline, ROW)
        self.assertEqual((y, probes), (YS[4], PROBES))
        self.assertEqual((start_x, early_x, end_x), (ANCHOR_X, 180, 216))
        with self.assertRaises(InvalidWitness):
            probe_geometry(raster(1, 2, probes=(PROBES[0], PROBES[1] - 3, PROBES[2])), ROW)
        pressed = raster(4, 5, pressed=True)
        self.assertEqual(pressed_anchor(pressed, ROWS, y, ANCHOR_X, probes), PRESS_SELECTED)
        for invalid in (baseline, raster(4, 5, selected=PROBES[:1], pressed=True)):
            with self.assertRaises(InvalidWitness):
                pressed_anchor(invalid, ROWS, y, ANCHOR_X, probes)
        held = raster(7, 8, selected=PROBES[:1], pressed=True)
        selected = highlight(held, ROWS, y, probes, False)
        self.assertEqual(selected, SELECTED)
        extended = raster(13, 14, selected=PROBES[:2], pressed=True)
        self.assertEqual(highlight(extended, ROWS, y, probes, True, selected), selected)
        verify_chronology(baseline, 3, pressed, 6, held, 9, 10, 11, 12, extended, 15)
        for seq in ((9, 10, 13, 12, 15), (9, 10, 11, 12, 13), (9, 9, 9, 12, 15),
                    (7, 10, 11, 12, 15)):
            admitted, flush, ack, extend, up = seq
            with self.assertRaises(InvalidWitness):
                verify_chronology(baseline, 3, pressed, 6, held, admitted, flush, ack, extend,
                                  extended, up)
        with self.assertRaises(InvalidWitness):
            verify_chronology(baseline, 3, pressed, 4, held, 9, 10, 11, 12, extended, 15)
        for invalid in (held, raster(12, 13, selected=PROBES),
                        raster(12, 13, selected=PROBES[:2], offsets=tuple(n + 1 for n in ROWS))):
            with self.assertRaises(InvalidWitness):
                highlight(invalid, ROWS, y, probes, True, selected)

    def test_probes_select_exact_row_specific_visible_text(self):
        for row in (1, 273, 312):
            visible = re.sub(r'\x1b\[[;0-9]+m', '', fixture_line(row))
            self.assertEqual(visible[6:18], f' R{row:04d} AB CD')
            self.assertEqual([visible[i] for i in (6, 12, 15, 19)], [' ', ' ', ' ', ' '])


class DriverBoundaryTest(unittest.TestCase):
    def test_release_and_copy_follow_two_native_witnesses(self):
        # Substitute only display/terminal boundaries to test driver ordering.
        for loss in (None, 'pressed', 'extended'):
            with self.subTest(loss=loss), tempfile.TemporaryDirectory() as td:
                root = Path(td)
                actions = []
                phase = 'baseline'
                copied = False
                dims = {'pid': 4, 'tty': '/dev/pts/fixture', 'rows': 8, 'cols': 80}
                app = subprocess.CompletedProcess([], 0)
                app.pid = 12
                app.poll = lambda: None
                captured = tuple(range(ROW_COUNT - 7, ROW_COUNT + 1))
                chosen = captured[4]
                exact = f' R{chosen:04d} AB CD'

                def flush_record(name, **fields):
                    (root / name).write_text(json.dumps({
                        **dims, 'monotonic_ns': time.monotonic_ns(), **fields,
                    }))

                class FakeCapture:
                    def __init__(self, *_):
                        pass
                    def capture(self):
                        img = Image.new('RGB', (250, 160))
                        paint = ImageDraw.Draw(img)
                        for i, n in enumerate(captured):
                            y = 14 + i * 14
                            for col, color in enumerate(marker_colors(n)):
                                x = 13 + col * 10
                                paint.rectangle((x, y, x + 9, y + 13), fill=color)
                            if n == chosen:
                                anchor_color = (PRESS_SELECTED if phase in ('down', 'held', 'extended', 'up')
                                                and loss != 'pressed' else PRESS_PROBE)
                                x = 13 + 6 * 10
                                paint.rectangle((x, y, x + 9, y + 13), fill=anchor_color)
                                for col in (12, 15, 19):
                                    x = 13 + col * 10
                                    selected = (col == 12 and phase in ('held', 'extended', 'up') or
                                                col == 15 and phase in ('extended', 'up')
                                                and loss != 'extended')
                                    paint.rectangle((x, y, x + 9, y + 13),
                                                    fill=SELECTED if selected else PROBE)
                        return img
                    def close(self):
                        actions.append('close')

                def run(*args, check=True):
                    nonlocal phase, copied
                    command = list(args)
                    output = ''
                    if command[:2] == ['xdotool', 'getactivewindow']:
                        output = 'window'
                    elif command[:2] == ['xdotool', 'getwindowname']:
                        output = driver.TITLE
                    elif command[:2] == ['xdotool', 'getwindowgeometry']:
                        output = 'owned-grid'
                    elif command[:2] == ['xdotool', 'mousedown']:
                        actions.append('down')
                        phase = 'down'
                    elif command[:2] == ['xdotool', 'mousemove']:
                        if phase == 'baseline':
                            actions.append('place')
                        elif phase == 'down':
                            self.assertTrue((root / 'held-pressed.png').exists())
                            actions.append('first-move')
                            phase = 'held'
                        elif phase == 'held':
                            self.assertTrue((root / 'held-ack.json').exists())
                            actions.append('extend')
                            phase = 'extended'
                    elif command[:2] == ['xdotool', 'mouseup']:
                        actions.append('up')
                        phase = 'up'
                    elif command[:2] == ['xdotool', 'key']:
                        self.assertEqual(phase, 'up')
                        actions.append('copy')
                        copied = True
                    else:
                        self.fail(f'Unexpected driver command {command}')
                    return subprocess.CompletedProcess(args, 0, output)

                def wait(predicate, description):
                    if description == 'held producer initial flush':
                        self.assertTrue((root / 'emit-held-rows').exists())
                        flush_record('held-initial.json', last=ROW_COUNT)
                    if description == 'held producer append flush':
                        self.assertTrue((root / 'held-intermediate.png').exists())
                        self.assertTrue((root / 'append-held-rows').exists())
                        actions.append('flush')
                        flush_record('held-append.json', first=ROW_COUNT + 1,
                                     last=ROW_COUNT + APPEND_ROWS)
                    if description == 'terminal parser receipt during held drag':
                        actions.append('ack')
                        flush_record('held-ack.json', parsed_append=True)
                    for _ in range(4):
                        result = predicate()
                        if result:
                            return result
                    raise TimeoutError(description)

                def clipboard(selection, expected, env):
                    self.assertEqual(expected, exact)
                    self.assertEqual(env['DISPLAY'], ':fixture')
                    return phase == 'up' and (selection == 'primary' or copied)

                with (patch.object(driver, 'X11StripCapture', FakeCapture),
                      patch.object(driver, 'clipboard_matches', clipboard)):
                    if loss:
                        error = 'native press anchor' if loss == 'pressed' else 'extended held'
                        with self.assertRaisesRegex(TimeoutError, error):
                            driver.exercise(root, {'DISPLAY': ':fixture'}, app, 'window', run, wait)
                        if loss == 'pressed':
                            self.assertEqual(actions, ['place', 'down', 'up', 'close'])
                            self.assertFalse((root / 'append-held-rows').exists())
                        else:
                            self.assertEqual(actions, ['place', 'down', 'first-move', 'flush', 'ack',
                                                       'extend', 'up', 'close'])
                        self.assertFalse((root / 'held-finish').exists())
                    else:
                        result = driver.exercise(root, {'DISPLAY': ':fixture'}, app, 'window', run, wait)
                        self.assertEqual(actions, ['place', 'down', 'first-move', 'flush', 'ack',
                                                   'extend', 'up', 'copy', 'close'])
                        self.assertEqual(result['row'], chosen)
                        self.assertTrue(result['primary_exact'] and result['clipboard_exact'])


class BoundedClipboardProbeTest(unittest.TestCase):
    def test_exact_short_oversize_timeout_and_nonzero(self):
        # Fake only xclip itself. The real nonblocking subprocess pipe,
        # overflow bound, timeout, and cleanup path run over native OS fds.
        with tempfile.TemporaryDirectory() as d:
            launcher = Path(d) / 'xclip'
            launcher.write_text('#!' + sys.executable + '\n' + r"""
import os, sys, time
assert sys.argv[1:] == ['-out', '-selection', 'primary']
assert os.environ['DISPLAY'] == ':321'
mode = os.environ['FIXTURE_CASE']
if mode == 'exact':
    os.write(1, b'R0285 AB CD')
elif mode == 'overflow':
    while True:
        os.write(1, b'R0285 AB CD' * 10000)
elif mode == 'closed':
    os.write(1, b'R0285')
elif mode == 'timeout':
    os.write(1, b'R0285 AB CD')
    time.sleep(4)
elif mode == 'closed-timeout':
    os.write(1, b'R0285 AB CD')
    os.close(1)
    time.sleep(4)
elif mode == 'nonzero':
    os.write(1, b'R0285 AB CD')
    sys.exit(2)
""")
            launcher.chmod(0o700)
            for case in ('exact', 'overflow', 'closed', 'timeout', 'closed-timeout', 'nonzero'):
                with self.subTest(case=case):
                    env = {'PATH': d, 'DISPLAY': ':321', 'FIXTURE_CASE': case}
                    self.assertEqual(clipboard_matches('primary', 'R0285 AB CD', env, timeout=.2),
                                     case == 'exact')
            with self.assertRaises(InvalidWitness):
                clipboard_matches('primary', 'R0285 AB CD', {'DISPLAY': 'remote:0'})


class HeldProducerTest(unittest.TestCase):
    def test_controlled_append_recognizes_only_terminal_receipt(self):
        for reply in (DSR_REPLY, b'unexpected'):
            with self.subTest(expected=reply == DSR_REPLY), tempfile.TemporaryDirectory() as root_dir:
                root = Path(root_dir)
                master, slave = pty.openpty()
                received = bytearray()
                fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 32, 112, 0, 0))
                process = subprocess.Popen([
                    sys.executable, str(Path(__file__).with_name('native_held_drag.py')), str(root),
                ], stdin=slave, stdout=slave, stderr=subprocess.PIPE)
                os.set_blocking(master, False)

                def receipt(name):
                    until = time.monotonic() + 4
                    while time.monotonic() < until:
                        try:
                            received.extend(os.read(master, 8192))
                        except BlockingIOError:
                            pass
                        if (root / name).is_file():
                            return json.loads((root / name).read_text())
                        if process.poll() is not None:
                            self.fail('Fixture exited before staged receipt')
                        time.sleep(.005)
                    self.fail('No staged receipt: ' + name)
                try:
                    first = receipt('held-ready.json')
                    self.assertFalse((root / 'held-initial.json').exists())
                    (root / 'emit-held-rows').touch()
                    initial = receipt('held-initial.json')
                    self.assertEqual(initial['last'], ROW_COUNT)
                    self.assertEqual((initial['pid'], initial['tty']), (first['pid'], first['tty']))
                    self.assertNotIn(DSR, received)
                    (root / 'append-held-rows').touch()
                    append = receipt('held-append.json')
                    self.assertEqual((append['first'], append['last']), (ROW_COUNT + 1, ROW_COUNT + APPEND_ROWS))
                    self.assertTrue(received.endswith(DSR))
                    self.assertFalse((root / 'held-ack.json').exists())
                    os.write(master, reply)
                    if reply == DSR_REPLY:
                        ack = receipt('held-ack.json')
                        self.assertTrue(ack['parsed_append'])
                        self.assertGreater(ack['monotonic_ns'], append['monotonic_ns'])
                        (root / 'held-finish').touch()
                        self.assertEqual(process.wait(timeout=3), 0)
                    else:
                        self.assertNotEqual(process.wait(timeout=3), 0)
                        self.assertFalse((root / 'held-ack.json').exists())
                        self.assertIn(b'Unexpected PTY input', process.stderr.read())
                finally:
                    if process.poll() is None:
                        process.kill()
                        process.wait(timeout=1)
                    process.stderr.close()
                    os.close(master)
                    os.close(slave)


if __name__ == '__main__':
    unittest.main()
