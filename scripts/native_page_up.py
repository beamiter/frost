"""Native PageUp test driver and owned, synthetic inline ANSI producer."""
import json
import os
from pathlib import Path
import signal
import sys
import time
import tty

from native_row_witness import (
    Frame, Grid, InvalidWitness, MARKER_END, MARKER_START, NIBBLE_COLORS,
    marker_ansi, row_ids, verify_page_up,
)
from native_x11_capture import X11StripCapture

ROW_COUNT = 300
TITLE = 'CLI_RESIZE_FIXTURE'


def calibrate_grid(image, target, expected):
    """Locate only an intact expected ordinal grid, with no hard-coded origins."""
    if image.mode != 'RGB':
        raise InvalidWitness('Expected RGB fixture pixels')
    raw = image.tobytes()
    width = image.width * 3
    start = bytes(MARKER_START)
    digits = tuple(tuple(bytes(color) for color in bank) for bank in NIBBLE_COLORS)
    end = bytes(MARKER_END)
    palette = {start, end, *(color for bank in digits for color in bank)}
    bands = []
    for y in range(image.height):
        line = raw[y * width:(y + 1) * width]
        x = line.find(start * 3)
        if x < 0 or x % 3:
            continue
        right = x
        samples, encoded = [], []
        for desired in ((start,), *digits, (end,)):
            color = line[right:right + 3]
            if samples and color not in palette:
                # Fractional cell edges can leave exactly one blended
                # pixel between solid marker runs in a native raster.
                right += 3
                color = line[right:right + 3]
            if color not in desired:
                break
            left = right
            while line[right:right + 3] == color:
                right += 3
            if right - left < 9:
                break
            samples.append((left // 3 + right // 3 - 1) // 2)
            encoded.append(color)
        if len(samples) != 5:
            continue
        if line.find(start * 3, right) >= 0:
            raise InvalidWitness('Multiple synthetic row markers in one scanline')
        row = sum(bank.index(color) << shift
                  for bank, color, shift in zip(digits, encoded[1:4], (8, 4, 0)))
        key = (tuple(samples), row)
        if bands and bands[-1][0] == key and bands[-1][2] + 1 == y:
            bands[-1] = (key, bands[-1][1], y)
        else:
            bands.append((key, y, y))
    if (len(bands) != len(expected)
            or tuple(key[1] for key, _, _ in bands) != expected
            or any(bottom - top < 2 for _, top, bottom in bands)
            or len({key[0] for key, _, _ in bands}) != 1):
        raise InvalidWitness('No complete ordered marker grid for the measured PTY')
    return Grid(target, bands[0][0][0], tuple((top + bottom) // 2 for _, top, bottom in bands))


def atomic_receipt(root, name, **extra):
    size = os.get_terminal_size(1)
    record = {'pid': os.getpid(), 'tty': os.ttyname(0), 'cols': size.columns,
              'rows': size.lines, 'monotonic_ns': time.monotonic_ns(), **extra}
    path = root / name
    temp = path.with_suffix('.tmp')
    temp.write_text(json.dumps(record) + '\n', encoding='utf-8')
    temp.replace(path)


def wait_file(root, name):
    deadline = time.monotonic() + 45
    while time.monotonic() < deadline:
        if (root / name).is_file():
            return
        time.sleep(.02)
    raise TimeoutError('Missing producer gate: ' + name)


def first_complete_return_sample(frame, expected_grid, expected_rows, key_interval,
                                 *, target_before, target_after):
    """A wrong complete frame is evidence against retention, not a retry."""
    if not target_before or not target_after:
        return None
    if frame.grid != expected_grid:
        raise InvalidWitness('Returned viewport changed grid identity')
    sent, returned = key_interval
    if not sent <= returned < frame.started < frame.finished:
        raise InvalidWitness('Tab return capture did not follow input injection')
    try:
        observed = row_ids(frame)
    except InvalidWitness:
        # Incomplete/ambiguous paint is not counted as a native frame receipt.
        return None
    if observed != expected_rows:
        raise InvalidWitness(f'First complete returned viewport starts at {observed[0]}, '
                             f'not retained row {expected_rows[0]}')
    return observed


def producer(root):
    """No child CLI, mouse reporting, alternate screen, or query injection."""
    generation = 0

    def resized(*_args):
        nonlocal generation
        generation += 1

    signal.signal(signal.SIGWINCH, resized)
    tty.setraw(0)
    sys.stdout.write('\x1b]0;' + TITLE + '\x07'
                     '\x1b]133;A\x07$ \x1b]133;B\x07codex\r\n'
                     '\x1b]133;C;cmdline_url=codex\x07')
    sys.stdout.flush()
    atomic_receipt(root, 'ordinal-ready.json', resize_generation=generation)
    wait_file(root, 'emit-ordinal-rows')
    # No trailing newline: a completely painted test viewport can contain a
    # marker in every PTY row without a bare cursor line at the bottom.
    sys.stdout.write('\r\n'.join(marker_ansi(row) + f'Frost ordinal {row:04d}'
                                for row in range(1, ROW_COUNT + 1)))
    sys.stdout.flush()
    atomic_receipt(root, 'ordinal-flush.json', resize_generation=generation,
                   first=1, last=ROW_COUNT,
                   alternate_screen=False, mouse_reporting=False)
    wait_file(root, 'check-ordinal-input')
    os.set_blocking(0, False)
    received = 0
    while received <= 4096:
        try:
            data = os.read(0, 4096)
        except BlockingIOError:
            break
        received += len(data)
        if not data:
            break
    atomic_receipt(root, 'ordinal-input.json', resize_generation=generation,
                   input_bytes=received)
    wait_file(root, 'producer-finish')


def exercise(root, env, app, window, run, wait, key, *, tab_return=False):
    root.joinpath('emit-ordinal-rows').touch(exist_ok=False)

    def read(name):
        path = root / name
        return json.loads(path.read_text(encoding='utf-8')) if path.exists() else None

    flush = wait(lambda: read('ordinal-flush.json'), 'ordinal producer flush')
    if (not 4 <= flush['rows'] <= 128 or flush['cols'] < 30
            or flush['first'] != 1 or flush['last'] != ROW_COUNT
            or flush['alternate_screen'] or flush['mouse_reporting']):
        raise InvalidWitness('Producer did not witness a supported scrollback-only PTY')
    expected = tuple(range(ROW_COUNT - flush['rows'] + 1, ROW_COUNT + 1))

    def identity():
        if (app.poll() is not None or run('xdotool', 'getactivewindow').stdout.strip() != window
                or TITLE not in run('xdotool', 'getwindowname', window).stdout):
            raise InvalidWitness('Synthetic CLI no longer owns the Frost input window')
        geometry = run('xdotool', 'getwindowgeometry', '--shell', window).stdout
        return (app.pid, window, geometry, flush['pid'], flush['tty'], flush['rows'], flush['cols'])

    capture = X11StripCapture(env['DISPLAY'], (0, 0, 1500, 900))

    def observe(grid=None):
        start = time.monotonic_ns()
        image = capture.capture()
        finished = time.monotonic_ns()
        if grid is None:
            grid = calibrate_grid(image, target, expected)
        return Frame(grid, image, start, finished)

    def witnessed(rows, grid=None):
        last = None

        def sample():
            nonlocal last
            try:
                frame = observe(grid)
                if row_ids(frame) != rows:
                    last = None
                    return None
            except InvalidWitness:
                last = None
                return None
            if last is not None and last.grid == frame.grid:
                return frame
            last = frame
            return None

        return wait(sample, 'two stable decoded native ordinal frames')

    def first_return(grid, rows, label):
        if (run('xdotool', 'getactivewindow').stdout.strip() != window or
                'SHELL_RESIZE_FIXTURE' not in run('xdotool', 'getwindowname', window).stdout):
            raise InvalidWitness('Tab return did not start on the disposable shell tab')

        def on_cli_tab():
            if app.poll() is not None or run('xdotool', 'getactivewindow').stdout.strip() != window:
                raise InvalidWitness('Tab return left the dedicated fixture window')
            return TITLE in run('xdotool', 'getwindowname', window).stdout

        sent = time.monotonic_ns()
        run('xdotool', 'key', '--clearmodifiers', 'ctrl+Prior')
        returned = time.monotonic_ns()
        unqualified = 0

        def sample():
            nonlocal unqualified
            if not on_cli_tab():
                return None
            if identity() != grid.target:
                raise InvalidWitness('Tab return changed window geometry or PTY identity')
            frame = observe(grid)
            on_target_after = on_cli_tab()
            if on_target_after and identity() != grid.target:
                raise InvalidWitness('Tab return capture straddled a changed target')
            try:
                observed = first_complete_return_sample(
                    frame, grid, rows, (sent, returned),
                    target_before=True, target_after=on_target_after)
            except InvalidWitness:
                frame.image.save(root / f'ordinal-return-{label}-wrong.png')
                raise
            if observed is None:
                unqualified += 1
                return None
            frame.image.save(root / f'ordinal-return-{label}.png')
            return frame

        frame = wait(sample, f'first complete same-grid {label} return')
        return frame, {
            'first': rows[0],
            'rows': len(rows),
            'unqualified_samples_after_title': unqualified,
            'key_interval_ns': [sent, returned],
            'capture_interval_ns': [frame.started, frame.finished],
        }

    try:
        target = identity()
        first = witnessed(expected)
        first.image.save(root / 'ordinal-before-tab.png')
        key('ctrl+shift+t', 'SHELL_RESIZE_FIXTURE')
        returns = {}
        if tab_return:
            before, returns['tail'] = first_return(first.grid, expected, 'tail')
        else:
            key('ctrl+Prior', TITLE)
        if identity() != target:
            raise InvalidWitness('Tab return changed window or grid identity')
        if not tab_return:
            before = witnessed(expected, first.grid)
        before.image.save(root / 'ordinal-before-page.png')
        if identity() != target:
            raise InvalidWitness('Synthetic CLI lost ownership before PageUp')

        sent = time.monotonic_ns()
        run('xdotool', 'key', '--clearmodifiers', 'Prior')
        returned = time.monotonic_ns()
        page = max(1, flush['rows'] - 2)
        after = witnessed(tuple(row - page for row in expected), first.grid)
        if identity() != target:
            raise InvalidWitness('PageUp changed the target window')
        after.image.save(root / 'ordinal-after-page.png')
        result = verify_page_up(before, after, [('Prior', sent, returned)],
                                pty_rows=flush['rows'], ownership='frost-scrollback',
                                fixture_flushes=(flush['monotonic_ns'],))
        if tab_return:
            key('ctrl+Next', 'SHELL_RESIZE_FIXTURE')
            _, returns['parked'] = first_return(
                first.grid, tuple(row - page for row in expected), 'parked')
            result['tab_return_first_complete'] = returns
        root.joinpath('check-ordinal-input').touch(exist_ok=False)
        receipt = wait(lambda: read('ordinal-input.json'), 'ordinal producer input readback')
        for field in ('pid', 'tty', 'rows', 'cols', 'resize_generation'):
            if receipt[field] != flush[field]:
                raise InvalidWitness('Page or same-grid tab return resized or replaced PTY')
        if receipt['input_bytes'] != 0:
            raise InvalidWitness('Input reached the synthetic CLI during owned scrollback')
        root.joinpath('producer-finish').touch(exist_ok=False)
        return {**result, 'producer': flush, 'input_receipt': receipt,
                'capture_intervals_ns': [[before.started, before.finished],
                                         [after.started, after.finished]],
                'bare_prior_interval_ns': [sent, returned]}
    except Exception:
        try:
            capture.capture().save(root / 'ordinal-failure.png')
        except (OSError, AssertionError):
            pass
        raise
    finally:
        capture.close()


if __name__ == '__main__':
    producer(Path(sys.argv[1]))
