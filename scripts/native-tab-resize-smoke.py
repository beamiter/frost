#!/usr/bin/env python3
"""Check real tab-switch/resize input against a private X11 Frost and PTY.

The inline CLI fixture records SIGWINCH and paints distinguishable colors for
intermediate and complete redraws. Requires Xvfb, Openbox, xdotool, bash and
Pillow. All input and screen capture run on a private X11 display.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import select
import shutil
import signal
import subprocess
import sys
import time

from native_x11_capture import X11StripCapture


REPLAY_COLOR = (249, 18, 81)


def ready_color(columns):
    return (24 + columns % 180, 239, 97)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True, type=Path)
    parser.add_argument('--output', required=True, type=Path)
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    for program in ['Xvfb', 'openbox', 'xdotool', 'bash']:
        if not shutil.which(program):
            parser.error('Missing test dependency: ' + program)
    os.umask(0o077)
    root = args.output.resolve()
    root.mkdir(mode=0o700, parents=True, exist_ok=False)
    for name in ['config/frost', 'data', 'state', 'cache', 'runtime']:
        (root / name).mkdir(mode=0o700, parents=True)
    log_path = root / 'resize.jsonl'
    fixture = root / 'codex'
    fixture.write_text(f'''import json, os, signal, time
from pathlib import Path
log = Path(os.environ['FROST_TAB_RESIZE_ROOT']) / 'resize.jsonl'
resize_requested = False

def record(event):
    size = os.get_terminal_size()
    with log.open('a') as stream:
        stream.write(json.dumps({{'event': event, 'cols': size.columns,
                                 'rows': size.lines}}) + '\\n')

def resized(*_):
    global resize_requested
    record('SIGWINCH')
    resize_requested = True

def stripe(color):
    rgb = ';'.join(map(str, color))
    print(f'\\x1b[7;5H\\x1b[48;2;{{rgb}}m' + ' ' * 40 +
          '\\x1b[0m\\x1b[11;1H', end='', flush=True)

signal.signal(signal.SIGWINCH, resized)
print('\\x1b]0;CLI_RESIZE_FIXTURE\\x07'
      '\\x1b]133;A\\x07$ \\x1b]133;B\\x07codex\\r\\n'
      '\\x1b]133;C;cmdline_url=codex\\x07', end='', flush=True)
for row in range(300):
    print('INLINE_CLI_FIXTURE_%04d' % row)
print('CLI_READY', flush=True)
record('start')
while True:
    time.sleep(.01)
    if not resize_requested:
        continue
    resize_requested = False
    cols = os.get_terminal_size().columns
    print('\\x1b[H\\x1b[2J\\x1b[3J', end='', flush=True)
    for _ in range(10):
        print('\\x1b[?2026h', end='')
        stripe({REPLAY_COLOR!r})
        print('\\x1b[?2026l', end='', flush=True)
        time.sleep(.025)
    print('\\x1b[?2026h\\x1b[H\\x1b[2J', end='')
    stripe((24 + cols % 180, 239, 97))
    print('\\x1b[?2026l', end='', flush=True)
''')
    bashrc = root / 'bashrc'
    bashrc.write_text("PS1='\\[\\e]133;A\\a\\]$ \\[\\e]133;B\\a\\]'\n"
                      "PROMPT_COMMAND=\"printf '\\e]0;SHELL_RESIZE_FIXTURE\\a'\"\n")
    launcher = root / 'shell'
    launcher.write_text('#!' + sys.executable + '\nimport os\nfrom pathlib import Path\n'
                        'try:\n    Path(' + repr(str(root / 'started')) + ').open("x").close()\n'
                        'except FileExistsError:\n    os.execv(' + repr(shutil.which('bash'))
                        + ', ["bash", "--noprofile", "--rcfile", ' + repr(str(bashrc)) + ', "-i"])\n'
                        'os.execv(' + repr(sys.executable) + ', [' + repr(sys.executable)
                        + ', ' + repr(str(fixture)) + '])\n')
    launcher.chmod(0o700)
    (root / 'config/frost/config.toml').write_text(
        'shell = ' + json.dumps(str(launcher)) + '\nrestore_session = false\n'
        'jsh_update_check = "never"\nterminal_mode = "block"\n'
        'initial_width = 1356\ninitial_height = 704\nfont_size = 14\nchrome_font_size = 14\n')
    env = os.environ.copy()
    for name in ['DISPLAY', 'WAYLAND_DISPLAY', 'SWAYSOCK', 'DBUS_SESSION_BUS_ADDRESS']:
        env.pop(name, None)
    env.update(XDG_CONFIG_HOME=str(root / 'config'), XDG_DATA_HOME=str(root / 'data'),
               XDG_STATE_HOME=str(root / 'state'), XDG_CACHE_HOME=str(root / 'cache'),
               XDG_RUNTIME_DIR=str(root / 'runtime'),
               JTERM_WORKSPACE_STATE_DIR=str(root / 'workspace-host'),
               FROST_TAB_RESIZE_ROOT=str(root), LIBGL_ALWAYS_SOFTWARE='1',
               ICED_BACKEND='tiny-skia', RUST_LOG='error')
    processes, streams = [], []
    result = {'binary': str(binary), 'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
              'scope': 'native X11 input and frames with real PTY SIGWINCH',
              'status': 'running'}

    def spawn(command, name, **kwargs):
        stream = (root / (name + '.log')).open('w')
        streams.append(stream)
        process = subprocess.Popen(command, env=env, cwd=root, stderr=stream,
                                   stdout=kwargs.pop('stdout', stream),
                                   start_new_session=True, **kwargs)
        processes.append(process)
        return process

    def run(*command, check=True):
        return subprocess.run(list(map(str, command)), env=env, capture_output=True,
                              text=True, check=check, timeout=10)

    def wait(predicate, description):
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            value = predicate()
            if value:
                return value
            time.sleep(.05)
        raise AssertionError('Timed out: ' + description)

    def records():
        return [json.loads(line) for line in log_path.read_text().splitlines()]

    def key(chord, title):
        assert run('xdotool', 'getactivewindow').stdout.strip() == window
        run('xdotool', 'key', '--clearmodifiers', chord)
        wait(lambda: title in run('xdotool', 'getwindowname', window).stdout, title)
        # Allow queued layout and PTY signals to reach the fixture before asserting.
        time.sleep(.4)

    try:
        xvfb = spawn(['Xvfb', '-displayfd', '1', '-screen', '0', '1500x900x24',
                      '-nolisten', 'tcp', '-noreset'], 'xvfb', stdout=subprocess.PIPE, text=True)
        assert select.select([xvfb.stdout], [], [], 10)[0], 'Xvfb did not start'
        number = xvfb.stdout.readline().strip()
        assert number.isdigit(), 'Invalid private X11 display'
        env['DISPLAY'] = ':' + number
        wait(lambda: run('xdotool', 'getdisplaygeometry', check=False).returncode == 0,
             'private X11 connection')
        spawn(['openbox', '--sm-disable'], 'openbox')
        app = spawn([str(binary)], 'frost')
        window = wait(lambda: run('xdotool', 'search', '--onlyvisible', '--pid', app.pid,
                                 check=False).stdout.strip().splitlines(), 'Frost window')[-1]
        run('xdotool', 'windowactivate', '--sync', window)
        wait(lambda: log_path.exists() and records(), 'CLI fixture output')
        time.sleep(1)
        before = records()
        key('ctrl+shift+t', 'SHELL_RESIZE_FIXTURE')
        for _ in range(3):
            key('ctrl+Prior', 'CLI_RESIZE_FIXTURE')
            key('ctrl+Next', 'SHELL_RESIZE_FIXTURE')
        result['switch_sigwinches'] = len(records()) - len(before)
        assert result['switch_sigwinches'] == 0, 'Tab switching resized the hidden CLI'

        # A real window resize is applied once the hidden CLI becomes visible.
        run('xdotool', 'windowsize', '--sync', window, '1256', '800')
        time.sleep(.5)
        assert records() == before, 'Window resize touched an invisible PTY'
        key('ctrl+Prior', 'CLI_RESIZE_FIXTURE')
        wait(lambda: len(records()) > len(before), 'resize on reveal')
        assert records()[-1]['rows'] != before[-1]['rows'], 'Real resize was lost'
        resized = records()
        key('ctrl+Next', 'SHELL_RESIZE_FIXTURE')
        key('ctrl+Prior', 'CLI_RESIZE_FIXTURE')
        assert records() == resized, 'Switching after a real resize retriggered SIGWINCH'
        result['resize_on_reveal'] = resized[-1]

        # A single resize can still replay earlier rows in several PTY writes.
        # Sample the actual screen until the new frame shows up.
        capture = X11StripCapture(env['DISPLAY'], (0, 0, 1500, 900))
        try:
            def colors():
                return {color for count, color in capture.capture().getcolors(65536)
                        if count >= 80}

            old_color = ready_color(resized[-1]['cols'])
            def old_frame():
                painted = colors()
                return old_color in painted and REPLAY_COLOR not in painted

            wait(old_frame, 'original frame')
            run('xdotool', 'windowsize', '--sync', window, '1296', '800')
            began = time.monotonic()
            retained_frame_seen = False

            def new_frame():
                nonlocal retained_frame_seen
                painted = colors()
                assert REPLAY_COLOR not in painted, 'An intermediate CLI replay reached the screen'
                latest = records()[-1]
                if latest['cols'] == resized[-1]['cols']:
                    return False
                retained_frame_seen |= old_color in painted
                return ready_color(latest['cols']) in painted

            wait(new_frame, 'complete resize repaint')
            assert retained_frame_seen, 'The previous frame disappeared during the CLI replay'
            result['retained_previous_frame'] = retained_frame_seen
            result['settled_frame_ms'] = round((time.monotonic() - began) * 1000)
        finally:
            capture.close()
        single = records()
        assert len(single) == len(resized) + 1, 'A single resize sent multiple PTY resizes'

        # Cross multiple grid boundaries while the inline CLI is visible.
        # One xdotool process issues the same rapid configure burst as a drag.
        burst = []
        for step in range(1, 9):
            burst.extend(['windowsize', window, str(1296 + 18 * step),
                          str(800 - 22 * step), 'sleep', '.018'])
        run('xdotool', *burst)
        wait(lambda: len(records()) > len(single), 'settled resize after a visible drag')
        time.sleep(.4)
        dragged = records()
        result['visible_drag_sigwinches'] = len(dragged) - len(single)
        assert result['visible_drag_sigwinches'] == 1, 'Drag sent multiple PTY resizes'
        assert dragged[-1]['cols'] > single[-1]['cols'], 'Final drag width was lost'
        assert dragged[-1]['rows'] < single[-1]['rows'], 'Final drag height was lost'

        # Splitting legitimately changes the CLI's pane, but hiding that split
        # behind a normal shell tab must keep the same dimensions on return.
        key('ctrl+shift+e', 'SHELL_RESIZE_FIXTURE')
        wait(lambda: len(records()) > len(dragged), 'CLI split-pane resize')
        split = records()
        key('ctrl+Next', 'SHELL_RESIZE_FIXTURE')
        key('ctrl+Prior', 'SHELL_RESIZE_FIXTURE')
        assert records() == split, 'Tab switching resized an invisible split pane'
        # Zooming the sibling must not resize the hidden CLI either.
        key('ctrl+shift+z', 'SHELL_RESIZE_FIXTURE')
        assert records() == split, 'Zooming the sibling resized an invisible pane'
        key('ctrl+shift+z', 'SHELL_RESIZE_FIXTURE')
        assert records() == split, 'Unzooming needlessly resized the CLI'
        result['split_switch_sigwinches'] = 0
        result['status'] = 'pass'
    except Exception as error:
        result.update(status='fail', error=str(error))
        raise
    finally:
        if log_path.exists():
            result['pty_records'] = records()
        (root / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
        print(json.dumps(result, indent=2), flush=True)
        # Session hosts detach. Match this private test's exact environment token,
        # so no unrelated terminal or live user job is eligible for cleanup.
        token = ('FROST_TAB_RESIZE_ROOT=' + str(root)).encode()
        for process in Path('/proc').iterdir():
            if process.name.isdigit():
                try:
                    if token in (process / 'environ').read_bytes().split(b'\0'):
                        os.kill(int(process.name), signal.SIGTERM)
                except (ProcessLookupError, FileNotFoundError, PermissionError):
                    pass
        for process in reversed(processes):
            if process.poll() is None:
                process.terminate()
            try:
                process.wait(timeout=3)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
        for stream in streams:
            stream.close()


if __name__ == '__main__':
    main()
