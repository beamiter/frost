#!/usr/bin/env python3
"""Check alternate-screen output and replies while a real Shell tab is hidden."""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import select
import shlex
import sys
import termios
import time
import tty


PRIMARY_COLOR = (41, 97, 223)
ALTERNATE_COLOR = (29, 218, 137)


def publish(directory, name, data):
    target = directory / f'{name}.json'
    pending = target.with_suffix('.tmp')
    pending.write_text(json.dumps(data))
    pending.replace(target)


def produce(directory):
    with open('/dev/tty', 'r+b', buffering=0) as terminal:
        fd = terminal.fileno()
        previous = termios.tcgetattr(fd)

        def write(data):
            remaining = memoryview(data)
            while remaining:
                remaining = remaining[os.write(fd, remaining):]

        def expect(expected):
            deadline = time.monotonic() + 28
            received = bytearray()
            while len(received) < len(expected):
                remaining = deadline - time.monotonic()
                if remaining <= 0 or not select.select([fd], [], [], remaining)[0]:
                    raise RuntimeError(f'Timed out waiting for {expected!r}, received {bytes(received)!r}')
                chunk = os.read(fd, len(expected) + 1)
                received.extend(chunk)
                assert chunk and expected.startswith(received), (expected, received)
            return received.hex()

        def stripe(color):
            rgb = ';'.join(map(str, color))
            write(f'\x1b[48;2;{rgb}m          \x1b[0m'.encode('ascii'))

        try:
            tty.setraw(fd)
            write(b'\x1b[2J\x1b[H')
            stripe(PRIMARY_COLOR)
            write(b'\x1b[?1049h\x1b[2J\x1b[5;11H\x1b[6n')
            reply = expect(b'\x1b[5;11R')
            name = os.ttyname(sys.stdin.fileno())
            columns = os.get_terminal_size(fd).columns
            publish(directory, 'alternate-open', dict(cursor_reply=reply, tty=name, columns=columns))

            deadline = time.monotonic() + 30
            while not (directory / 'emit-hidden').exists():
                assert time.monotonic() < deadline, 'Timed out waiting to emit on the hidden tab'
                time.sleep(0.02)

            line = b'background Frost output before cursor reply ' + b'.' * 46 + b'\r\n'
            for _ in range(10):
                write(line * 1500)
            write(b'\x1b[2J\x1b[7;19H\x1b[6n')
            stripe(ALTERNATE_COLOR)
            reply = expect(b'\x1b[7;19R')
            publish(directory, 'hidden-output', dict(cursor_reply=reply, bytes=15000 * len(line),
                                                     columns=os.get_terminal_size(fd).columns))

            received = expect(b'x')
            write(b'\x1b[?1049l')
            publish(directory, 'alternate-closed', dict(input=received, tty=name))
        except Exception as error:
            publish(directory, 'producer-error', {'error': str(error)})
            raise
        finally:
            termios.tcsetattr(fd, termios.TCSANOW, previous)


def exercise(binary, output):
    from PIL import Image

    script = Path(__file__).resolve()
    spec = importlib.util.spec_from_file_location('native_smoke', script.with_name('native-desktop-smoke.py'))
    runner = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(runner)

    output.mkdir(mode=0o700, parents=True, exist_ok=False)
    smoke = runner.Smoke(binary, 'x11', output)
    workspace = Path(smoke.env['XDG_RUNTIME_DIR']) / 'workspace'
    smoke.env.update(ICED_BACKEND='tiny-skia', JTERM_WORKSPACE_STATE_DIR=str(workspace))
    smoke.config.write_text('shell = "/bin/bash"\nterminal_mode = "normal"\n'
                            'initial_width = 1100\ninitial_height = 760\nrestore_session = false\n'
                            'eva_companion_enabled = false\njsh_update_check = "never"\n')
    smoke.config.chmod(0o600)
    result = {'backend': 'x11', 'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest()}

    def receipt(name):
        def read():
            failure = output / 'producer-error.json'
            if failure.exists():
                raise AssertionError(failure.read_text())
            path = output / f'{name}.json'
            return json.loads(path.read_text()) if path.exists() else None

        return runner.wait_for(read, name)

    def painted_pixels(name, color):
        smoke.screenshot(name)
        with Image.open(output / f'{name}.png') as frame:
            return sum(pixel == color for pixel in frame.convert('RGB').getdata())

    try:
        smoke.start_desktop()
        # Own the broker child so it is reaped along with the test compositor.
        host = smoke.spawn([str(binary), '--jterm-session-host'], 'workspace-host')
        runner.wait_for((workspace / 'run/host.sock').is_socket, 'private workspace host')
        assert host.poll() is None
        application = smoke.spawn([str(binary)], 'frost')
        runner.wait_for(smoke.app_window, 'Frost main window')
        smoke.select_terminal(require_terminal=True)
        original = smoke.command('background-screen-original')
        smoke.text(shlex.join([sys.executable, str(script), '--produce', str(output)]))
        smoke.key('Return')
        opened = receipt('alternate-open')

        smoke.key('t', ('ctrl', 'shift'))
        peer = smoke.command('background-screen-peer')
        assert peer != original
        (output / 'emit-hidden').touch()
        hidden = receipt('hidden-output')
        assert hidden['columns'] == opened['columns']
        assert smoke.command('background-screen-peer-still-active') == peer
        assert painted_pixels('hidden-marker-absent', ALTERNATE_COLOR) == 0

        smoke.key('Page_Up', ('ctrl',))
        runner.wait_for(lambda: painted_pixels('restored-alternate', ALTERNATE_COLOR) > 100,
                        'restored alternate-screen marker', timeout=12)
        smoke.key('x')
        closed = receipt('alternate-closed')
        runner.wait_for(lambda: painted_pixels('restored-primary', PRIMARY_COLOR) > 100,
                        'restored primary-screen marker', timeout=12)
        assert smoke.command('background-screen-returned') == original
        assert opened['tty'] == closed['tty'] == original.rsplit(':', 1)[1]
        smoke.close_app(application)
        result.update(status='pass', alternate=opened, hidden=hidden, restored=closed,
                      original_shell=original, peer_shell=peer)
    except Exception as error:
        result.update(status='fail', error=str(error))
        raise
    finally:
        try:
            smoke.close()
        except Exception as error:
            result.update(status='fail', cleanup_error=str(error))
            raise
        finally:
            publish(output, 'result', result)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--produce', type=Path, help=argparse.SUPPRESS)
    parser.add_argument('--binary', type=Path)
    parser.add_argument('--output', type=Path)
    args = parser.parse_args()
    if args.produce is not None and args.binary is None and args.output is None:
        produce(args.produce)
    elif args.produce is None and args.binary is not None and args.output is not None:
        exercise(args.binary.resolve(strict=True), args.output.resolve())
    else:
        parser.error('--binary and --output are required')
