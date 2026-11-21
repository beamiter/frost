"""Opt-in installed X11 selection witness with an owned synthetic PTY."""
import json
import os
from pathlib import Path
import select
import subprocess
import sys
import time
import tty

from native_page_up import ROW_COUNT, TITLE, atomic_receipt, calibrate_grid, wait_file
from native_row_witness import Frame, InvalidWitness, _pixel, marker_ansi, row_ids
from native_x11_capture import X11StripCapture

APPEND_ROWS = 12
PROBE = (11, 40, 66)
PRESS_PROBE = (27, 69, 31)
PRESS_ANSI = '\x1b[48;2;27;69;31m \x1b[0m'
PROBE_ANSI = '\x1b[48;2;11;40;66m \x1b[0m'
DSR = b'\x1b[5n'
DSR_REPLY = b'\x1b[0n'


def fixture_line(row):
    return marker_ansi(row) + ' ' + PRESS_ANSI + f'R{row:04d}' + PROBE_ANSI + 'AB' + PROBE_ANSI + 'CD ' + PROBE_ANSI


def probe_geometry(frame, row):
    """Discover three independent blank-cell probes before any pointer input."""
    ids = row_ids(frame)
    if row not in ids:
        raise InvalidWitness('Missing baseline selection ordinal')
    y = frame.grid.y[ids.index(row)]
    buf = frame.image.crop((0, y, frame.image.width, y + 1)).tobytes()
    color = bytes(PROBE)
    centers = []
    right = frame.grid.x[-1] * 3
    while right < len(buf):
        left = buf.find(color * 3, right)
        if left < 0:
            break
        right = left + 3
        if left % 3:
            continue
        while buf[right:right + 3] == color:
            right += 3
        centers.append((left // 3 + right // 3 - 1) // 2)
    if len(centers) != 3:
        raise InvalidWitness('Need three exact-color baseline probes')
    inner, extension, control = centers
    pitch = (control - inner) / 7
    marker_pitch = (frame.grid.x[-1] - frame.grid.x[0]) / 4
    if (not 4 <= pitch <= 64 or abs(pitch - marker_pitch) > .5
            or abs(extension - inner - 3 * pitch) > 1
            or abs(inner - 8 * pitch - frame.grid.x[-1]) > 2):
        raise InvalidWitness('Selection probes do not match native cell spacing')
    for x in centers:
        if _pixel(frame.image, x, y) != PROBE:
            raise InvalidWitness('Baseline probe is not solid')
    # Columns: marker[0..4], blank[5], press[6], Rdddd[7..11],
    # probe[12], AB[13..14], probe[15], CD[16..17], blank[18], control[19].
    anchor = round(inner - 6 * pitch)
    if _pixel(frame.image, anchor, y) != PRESS_PROBE:
        raise InvalidWitness('Missing exact blank press anchor control')
    return (y, tuple(centers), anchor, round(inner + 2 * pitch), round(inner + 5 * pitch))


def pressed_anchor(frame, ids, y, anchor, probes):
    if row_ids(frame) != ids:
        raise InvalidWitness('Press displaced its numbered history viewport')
    color = _pixel(frame.image, anchor, y)
    if color == PRESS_PROBE or any(_pixel(frame.image, x, y) != PROBE for x in probes):
        raise InvalidWitness('Expected only the press anchor to be highlighted')
    return color


def highlight(frame, ids, y, probes, extended, selected=None):
    if row_ids(frame) != ids:
        raise InvalidWitness('Selection displaced its numbered history viewport')
    first, second, control = (_pixel(frame.image, x, y) for x in probes)
    if (first == PROBE or control != PROBE or
            second != (first if extended else PROBE) or
            (selected is not None and first != selected)):
        raise InvalidWitness('Wrong held highlight or changed outside control')
    return first


def verify_chronology(baseline, down, pressed, move, held, admitted, flushed, acked,
                      extend, extended, up):
    if not (baseline.finished < down < pressed.started < pressed.finished < move
            < held.started < held.finished < admitted
            <= flushed < acked < extend < extended.started < extended.finished < up):
        raise InvalidWitness('Held-drag input, append, and native capture were not ordered')


def producer(root):
    tty.setraw(0)
    sys.stdout.write('\x1b]0;' + TITLE + '\x07'
                     '\x1b]133;A\x07$ \x1b]133;B\x07codex\r\n'
                     '\x1b]133;C;cmdline_url=codex\x07')
    sys.stdout.flush()
    atomic_receipt(root, 'held-ready.json')
    wait_file(root, 'emit-held-rows')
    sys.stdout.write('\r\n'.join(fixture_line(row) for row in range(1, ROW_COUNT + 1)))
    sys.stdout.flush()
    atomic_receipt(root, 'held-initial.json', last=ROW_COUNT)
    wait_file(root, 'append-held-rows')
    sys.stdout.buffer.write(('\r\n' + '\r\n'.join(
        fixture_line(row) for row in range(ROW_COUNT + 1, ROW_COUNT + APPEND_ROWS + 1))).encode())
    sys.stdout.buffer.write(DSR)
    sys.stdout.flush()
    atomic_receipt(root, 'held-append.json', first=ROW_COUNT + 1, last=ROW_COUNT + APPEND_ROWS)

    # DSR arrives only after Frost processed preceding PTY output in order.
    # Never persist raw received bytes (only a recognized fixed receipt).
    reply = b''
    until = time.monotonic() + 8
    while len(reply) < len(DSR_REPLY):
        remaining = until - time.monotonic()
        if remaining <= 0 or not select.select([0], [], [], remaining)[0]:
            raise TimeoutError('No terminal status receipt after append')
        chunk = os.read(0, len(DSR_REPLY) + 1)
        reply += chunk
        if not chunk or not DSR_REPLY.startswith(reply):
            raise InvalidWitness('Unexpected PTY input before terminal status receipt')
    atomic_receipt(root, 'held-ack.json', parsed_append=True)
    wait_file(root, 'held-finish')


def clipboard_matches(selection, expected, env, *, timeout=1.0):
    """Read one private X11 selection, with byte and wall-clock limits."""
    display = env.get('DISPLAY', '')
    if (selection not in ('primary', 'clipboard') or not display.startswith(':')
            or not display[1:].isascii() or not display[1:].isdecimal()
            or not 0 < len(expected) <= 64 or not expected.isascii()
            or not 0 < timeout <= 1):
        raise InvalidWitness('Invalid private clipboard probe')
    wanted = expected.encode('ascii')
    limit = len(wanted) + 1  # One extra byte proves a non-exact selection.
    deadline = time.monotonic() + timeout
    process = subprocess.Popen(['xclip', '-out', '-selection', selection], env=env,
                               stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                               stderr=subprocess.DEVNULL)
    assert process.stdout is not None
    fd = process.stdout.fileno()
    data = bytearray()
    try:
        os.set_blocking(fd, False)
        while len(data) < limit:
            remaining = deadline - time.monotonic()
            if remaining <= 0 or not select.select([fd], [], [], remaining)[0]:
                return False
            try:
                chunk = os.read(fd, limit - len(data))
            except BlockingIOError:
                continue
            if not chunk:
                remaining = deadline - time.monotonic()
                return (data == wanted and remaining > 0
                        and process.wait(timeout=remaining) == 0)
            data.extend(chunk)
        return False
    except subprocess.TimeoutExpired:
        return False
    finally:
        process.stdout.close()
        if process.poll() is None:
            process.kill()
            process.wait(timeout=1)


def exercise(root, env, app, window, run, wait):
    def gate(name):
        root.joinpath(name).touch(exist_ok=False)

    def read(name):
        path = root / name
        return json.loads(path.read_text(encoding='utf-8')) if path.exists() else None

    gate('emit-held-rows')
    initial = wait(lambda: read('held-initial.json'), 'held producer initial flush')
    if not 4 <= initial['rows'] <= 128 or not 30 <= initial['cols'] or initial['last'] != ROW_COUNT:
        raise InvalidWitness('Unsupported held-drag PTY geometry')
    ids = tuple(range(ROW_COUNT + 1 - initial['rows'], ROW_COUNT + 1))
    row = ids[len(ids) // 2]

    def target():
        if (app.poll() is not None or run('xdotool', 'getactivewindow').stdout.strip() != window
                or TITLE not in run('xdotool', 'getwindowname', window).stdout):
            raise InvalidWitness('Held fixture no longer owns the active X11 window')
        return (app.pid, window, run('xdotool', 'getwindowgeometry', '--shell', window).stdout,
                initial['pid'], initial['tty'], initial['rows'], initial['cols'])

    owner = target()
    cap = X11StripCapture(env['DISPLAY'], (0, 0, 1500, 900))
    pressed = False

    def sample(grid=None):
        start = time.monotonic_ns()
        image = cap.capture()
        finish = time.monotonic_ns()
        return Frame(grid if grid is not None else calibrate_grid(image, owner, ids),
                     image, start, finish)

    def stable(grid, observe, desc):
        last = None
        def try_sample():
            nonlocal last
            try:
                f = sample(grid)
                value = observe(f)
            except InvalidWitness:
                last = None
                return None
            qual = (f.grid, value)
            duplicate = last == qual
            last = qual
            return (f, value) if duplicate else None
        return wait(try_sample, desc)

    try:
        baseline, (y, probes, start_x, early_x, final_x) = stable(
            None, lambda frame: probe_geometry(frame, row), 'unselected native baseline')
        baseline.image.save(root / 'held-baseline.png')
        expected = f' R{row:04d} AB CD'
        if any(clipboard_matches(selection, expected, env)
               for selection in ('primary', 'clipboard')):
            raise InvalidWitness('Clipboard already contains expected witness before input')
        if target() != owner:
            raise InvalidWitness('Held fixture geometry changed before input')
        run('xdotool', 'mousemove', start_x, y)
        run('xdotool', 'mousedown', '1')
        pressed = True
        down = time.monotonic_ns()
        # XTest request completion alone does not establish that Frost has
        # observed the starting cursor. Do not queue the drag endpoint until
        # the distinct press anchor is painted on two native samples.
        pressed_frame, press_rgb = stable(
            baseline.grid, lambda f: pressed_anchor(f, ids, y, start_x, probes),
            'native press anchor before first movement')
        pressed_frame.image.save(root / 'held-pressed.png')
        if target() != owner:
            raise InvalidWitness('Held fixture lost its input target after press')

        def selection(f, extended, expected_rgb=None):
            if _pixel(f.image, start_x, y) != press_rgb:
                raise InvalidWitness('Drag discarded its observed press anchor')
            return highlight(f, ids, y, probes, extended, expected_rgb)

        run('xdotool', 'mousemove', early_x, y)
        move = time.monotonic_ns()
        held, selection_rgb = stable(
            baseline.grid, lambda f: selection(f, False), 'first held highlight')
        held.image.save(root / 'held-intermediate.png')
        admitted = time.monotonic_ns()
        gate('append-held-rows')
        append = wait(lambda: read('held-append.json'), 'held producer append flush')
        ack = wait(lambda: read('held-ack.json'), 'terminal parser receipt during held drag')
        if ((append['first'], append['last']) != (ROW_COUNT + 1, ROW_COUNT + APPEND_ROWS)
                or not ack['parsed_append']
                or any(r[f] != initial[f] for r in (append, ack)
                       for f in ('pid', 'tty', 'rows', 'cols'))
                or target() != owner):
            raise InvalidWitness('Held append lost original producer, PTY or target')
        run('xdotool', 'mousemove', final_x, y)
        extend = time.monotonic_ns()
        extended, _ = stable(baseline.grid,
            lambda f: selection(f, True, selection_rgb), 'extended held highlight')
        extended.image.save(root / 'held-extended.png')
        if target() != owner:
            raise InvalidWitness('Held fixture geometry changed before release')
        up = time.monotonic_ns()
        run('xdotool', 'mouseup', '1')
        pressed = False
        verify_chronology(baseline, down, pressed_frame, move, held, admitted, append['monotonic_ns'],
                          ack['monotonic_ns'], extend, extended, up)
        wait(lambda: clipboard_matches('primary', expected, env), 'exact PRIMARY after native release')
        cap.capture().save(root / 'held-released.png')
        if target() != owner:
            raise InvalidWitness('Held fixture lost the active X11 window before copy')
        run('xdotool', 'key', '--clearmodifiers', 'ctrl+shift+c')
        wait(lambda: clipboard_matches('clipboard', expected, env), 'exact CLIPBOARD after native copy')
        gate('held-finish')
        result = {'row': row, 'initial': initial, 'append': append, 'parser_ack': ack,
                  'primary_exact': True, 'clipboard_exact': True,
                  'capture_intervals_ns': [[f.started, f.finished] for f in
                                           (baseline, pressed_frame, held, extended)],
                  'input_and_gate_ns': [down, move, admitted, extend, up]}
        return result
    except Exception:
        try:
            cap.capture().save(root / 'held-failure.png')
        except (OSError, AssertionError):
            pass
        raise
    finally:
        try:
            if pressed:
                run('xdotool', 'mouseup', '1', check=False)
        finally:
            cap.close()


if __name__ == '__main__':
    producer(Path(sys.argv[1]))
