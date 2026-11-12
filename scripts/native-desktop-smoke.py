#!/usr/bin/env python3
"""Real compositor/PTY smoke. No synthetic application messages or guard bypasses.

Requires an installed Frost binary. Isolated HOME/XDG dirs and compositor ensure
clipboard contents are synthetic. Failure retains logs/screenshots/transcripts.
This is functional smoke, not an IME, hardware GPU, or latency certification.
"""
import argparse
import csv
import io
import hashlib
import json
import os
import re
import select
import struct
from pathlib import Path
import shlex
import subprocess
import time
import tempfile


def wait_for(test, label, timeout=30):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        value = test()
        if value:
            return value
        time.sleep(0.1)
    raise RuntimeError('Timed out: ' + label)


class Smoke:
    def __init__(self, binary, backend, out):
        self.binary, self.backend, self.out = binary, backend, out
        self.processes = []
        self.logs = []
        self.env = os.environ.copy()
        for name in ('DISPLAY', 'WAYLAND_DISPLAY', 'SWAYSOCK'):
            self.env.pop(name, None)
        home = out / 'home'
        # Unix-domain socket names have a108-byte limit. Deep evidence paths
        # must not prevent the real compositor from starting before app launch.
        self._runtime = tempfile.TemporaryDirectory(prefix='frost-native-', dir='/tmp')
        runtime = Path(self._runtime.name)
        (out / 'native-runtime.json').write_text(json.dumps({'runtime': str(runtime),
            'reason': 'private short Unix-domain socket directory', 'removed_after_cleanup': True}, indent=2))
        home.mkdir()
        for key, suffix in [('HOME', ''), ('XDG_CONFIG_HOME', '.config'),
                            ('XDG_DATA_HOME', '.local/share'), ('XDG_STATE_HOME', '.local/state'),
                            ('XDG_CACHE_HOME', '.cache')]:
            self.env[key] = str(home / suffix)
        self.env.update(FROST_SMOKE_SHELL_TRACE=str(out / 'synthetic-shell-trace.log'), FROST_SMOKE_GRID_DIR=str(out), XDG_RUNTIME_DIR=str(runtime), LIBGL_ALWAYS_SOFTWARE='1',
                        WGPU_BACKEND='vulkan', RUST_LOG='warn,iced_wgpu::window::compositor=info')
        config = home / '.config/frost'
        config.mkdir(parents=True)
        self.config = config / 'config.toml'
        (home / '.bash_profile').write_text(r"""export HISTFILE=/dev/null
exec 9>>"$FROST_SMOKE_SHELL_TRACE"
BASH_XTRACEFD=9
set -x
# Public OSC 133 bash integration documented in Frost's README. No private
# authentication token or foreground guard is overridden by this fixture.
__frost_smoke_prompt() {
    local status=$?
    if [[ ${__frost_smoke_seen:-0} == 1 ]]; then printf '\e]133;D;%s\a' "$status"; fi
    __frost_smoke_seen=1
}
g() { printf '%s %s %s\n' "$$" "$(tty)" "$(stty size)" > "$FROST_SMOKE_GRID_DIR/$1-stty.txt"; }
PROMPT_COMMAND=__frost_smoke_prompt
PS0=$'\e]133;C\a'
PS1='\[\e]133;A\a\]FROST_SMOKE> \[\e]133;B\a\]'
""")
        self.transcript = out / 'shell-sentinels.txt'

    def run(self, args, **kwargs):
        return subprocess.run(args, env=self.env, check=True, timeout=30, **kwargs)

    def spawn(self, args, label):
        log = open(self.out / (label + '.log'), 'w')
        self.logs.append(log)
        # Opt-in protocol metadata from the actual Frost child only.
        env = self.env.copy()
        env.pop('WAYLAND_DEBUG', None)
        if (self.backend == 'wayland' and env.get('FROST_SMOKE_WAYLAND_DEBUG') == '1'
                and args and args[0] == str(self.binary)):
            env['WAYLAND_DEBUG'] = 'client'
        proc = subprocess.Popen(args, env=env, stdout=log, stderr=log)
        self.processes.append(proc)
        return proc

    def start_desktop(self):
        if self.backend == 'x11':
            self.env['DISPLAY'] = ':99'
            server = self.spawn(['Xvfb', ':99', '-screen', '0', '1280x900x24', '-nolisten', 'tcp'], 'xvfb')
            deadline = time.monotonic() + 30
            readiness = self.out / 'x11-readiness.jsonl'
            def display_ready():
                assert server.poll() is None, 'Xvfb exited before display readiness'
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    return False
                attempt = {'monotonic': time.monotonic()}
                try:
                    probe = subprocess.run(['xdpyinfo'], env=self.env, stdout=subprocess.DEVNULL,
                                           stderr=subprocess.PIPE, text=True, timeout=min(3, remaining))
                    attempt.update(returncode=probe.returncode, stderr=probe.stderr[:512])
                except subprocess.TimeoutExpired:
                    attempt['timeout'] = True
                with readiness.open('a') as log:
                    log.write(json.dumps(attempt) + '\n')
                return attempt.get('returncode') == 0
            # Only readiness probes may repeat; no app launch or input is retried.
            wait_for(display_ready, 'X11 display within 30 seconds', timeout=30)
            assert server.poll() is None
            self.spawn(['openbox', '--sm-disable'], 'openbox')
        else:
            cfg = self.out / 'sway.conf'
            cfg.write_text('xwayland disable\noutput * resolution 1280x900\nseat seat0 fallback true\nfocus_follows_mouse no\n')
            self.env.update(WLR_BACKENDS='headless', WLR_RENDERER='pixman', WLR_LIBINPUT_NO_DEVICES='1', WLR_HEADLESS_OUTPUTS='1')
            # The fixture uses the headless Pixman renderer. Sway's host-driver
            # check still sees installed Nvidia modules even though no GPU is
            # used; its documented flag permits this isolated software run.
            server = self.spawn(['sway', '--unsupported-gpu', '-d', '-c', str(cfg)], 'sway')
            runtime = Path(self.env['XDG_RUNTIME_DIR'])
            sock = wait_for(lambda: next((p for p in runtime.glob('wayland-*') if p.is_socket()), None), 'Wayland socket')
            self.env['WAYLAND_DISPLAY'] = sock.name.removesuffix('.lock')
            ipc = wait_for(lambda: next(runtime.glob('sway-ipc.*.sock'), None), 'Sway IPC')
            self.env['SWAYSOCK'] = str(ipc)
            assert server.poll() is None
        time.sleep(1)
        if self.backend == 'wayland':
            # Measure the empty private compositor before launching Frost.
            # Glass edge checks composite source RGBA over this observation,
            # never infer the background or expected panel color from the app.
            self.screenshot('desktop-backdrop')
            ppm = self.run(['grim', '-t', 'ppm', '-'], capture_output=True).stdout
            (self.out / 'desktop-backdrop.ppm').write_bytes(ppm)
            header = re.match(br'P6\s+(\d+)\s+(\d+)\s+255\s', ppm)
            assert header and tuple(map(int, header.groups())) == (1280, 900)
            data = ppm[header.end():]
            samples = [list(data[(y * 1280 + x) * 3:(y * 1280 + x) * 3 + 3])
                       for x in (10, 320, 960, 1270) for y in (10, 200, 700, 890)]
            assert all(color == samples[0] for color in samples), samples
            self.desktop_backdrop = samples[0]
            (self.out / 'desktop-backdrop.json').write_text(json.dumps({
                'rgb': self.desktop_backdrop, 'samples': samples, 'captured_before_app': True}))

    def key(self, key, modifiers=()):
        if self.backend == 'x11':
            self.run(['xdotool', 'key', '--clearmodifiers', '+'.join((*modifiers, key))])
        else:
            # Each wtype invocation hotplugs a new virtual keyboard. Allow
            # compositor capability/focus events to reach the app before input.
            args = ['wtype', '-s', '250']
            for mod in modifiers:
                args += ['-M', mod]
            args += ['-k', key]
            for mod in reversed(modifiers):
                args += ['-m', mod]
            args += ['-s', '50']
            self.run(args)
        time.sleep(0.15)

    def text(self, text):
        if self.backend == 'x11':
            self.run(['xdotool', 'type', '--clearmodifiers', '--delay', '8', '--', text])
        else:
            self.run(['wtype', '-s', '250', '-d', '8', '--', text])

    def screenshot(self, name):
        target = str(self.out / (name + '.png'))
        if self.backend == 'x11':
            self.run(['import', '-window', 'root', target])
        else:
            self.run(['grim', target])

    def observe_frame(self, label, enhanced=False):
        # Screenshots and OCR are read-only observations, never app messages.
        self._frame_number = getattr(self, '_frame_number', 0) + 1
        name = f'{label}-{self._frame_number:03d}'
        self.screenshot(name)
        source = self.out / (name + '.png')
        offset_x = offset_y = 0
        if enhanced:
            # Only the centered close modal is relevant to this observation.
            # Keep the full raw frame, and record the bounded crop transform.
            # Whole-screen OCR of 32 sidebar rows can both lose the actual
            # modal and spend longer than the clipboard's 3-second deadline.
            header = source.read_bytes()[:24]
            assert header[:8] == b'\x89PNG\r\n\x1a\n', 'Expected raw PNG frame'
            width, height = struct.unpack('>II', header[16:24])
            crop_w, crop_h = width * 3 // 5, height * 3 // 10
            offset_x, offset_y = (width - crop_w) // 2, (height - crop_h) // 2
            source = self.out / (name + '.ocr.png')
            crop = f'{crop_w}x{crop_h}+{offset_x}+{offset_y}'
            self.run(['convert', str(self.out / (name + '.png')), '-crop', crop, '+repage',
                      '-colorspace', 'Gray', '-negate', '-resize', '300%', str(source)],
                     capture_output=True)
            (self.out / (name + '.ocr-transform.json')).write_text(json.dumps({
                'raw_frame': name + '.png', 'crop': [offset_x, offset_y, crop_w, crop_h],
                'scale': 3, 'required_text': ['Closing Frost', 'Eva disconnected', 'Close anyway']
            }, indent=2))
        tsv = self.run(['tesseract', str(source), 'stdout',
                        '--psm', '11', 'tsv'], text=True, capture_output=True).stdout
        (self.out / (name + '.tsv')).write_text(tsv)
        rows = [row for row in csv.DictReader(io.StringIO(tsv), delimiter='\t',
                quoting=csv.QUOTE_NONE) if row['level'] == '5' and row['text'].strip()]
        if enhanced:
            for row in rows:
                for field in ('left', 'top', 'width', 'height'):
                    row[field] = str(round(int(row[field]) / 3))
                row['left'] = str(int(row['left']) + offset_x)
                row['top'] = str(int(row['top']) + offset_y)
        return name, rows

    @staticmethod
    def surface_state(rows):
        text = ' '.join(row['text'] for row in rows)
        api = re.search(r'\bAPI conversation\b', text, re.I) is not None
        terminal = (re.search(r'\bWorkspace\s*/', text) is not None
                    and re.search(r'\b(?:Block|Chat) session\b', text) is not None)
        if api == terminal:
            return None
        return 'api' if api else 'terminal'

    def select_terminal(self, require_terminal=False):
        # Background import must leave a fresh launch on its terminal. Other
        # scenarios may intentionally open an API; Escape closes it once.
        # Another Escape reaches Bash readline and changes the next key into
        # a meta command. Observe the visible owner before sending any input.
        observed = []
        def observe():
            name, rows = self.observe_frame('terminal-owner')
            state = self.surface_state(rows)
            observed.append({'frame': name, 'state': state})
            return state
        initial = wait_for(observe, 'visible API or terminal surface')
        if require_terminal and initial != 'terminal':
            (self.out / f'terminal-owner-{self._frame_number:03d}.json').write_text(
                json.dumps({'initial': initial, 'escape_presses': 0,
                            'required_terminal': True, 'observations': observed}, indent=2))
            raise AssertionError('Background chat migration opened the API view')
        if initial == 'api':
            self.key('Escape')
            wait_for(lambda: observe() == 'terminal', 'API closed and terminal visible')
        (self.out / f'terminal-owner-{self._frame_number:03d}.json').write_text(
            json.dumps({'initial': initial, 'escape_presses': int(initial == 'api'),
                        'required_terminal': require_terminal,
                        'observations': observed}, indent=2))

    @staticmethod
    def close_button(rows):
        text = ' '.join(row['text'] for row in rows)
        # The headless fixture has no Eva host. Only acknowledge this precise
        # visible disconnected-Eva warning, never a running-job or other modal.
        if not all(value in text for value in ('Closing Frost', 'Eva disconnected',
                                                'Close anyway')):
            return None
        matches = []
        for left, right in zip(rows, rows[1:]):
            if left['text'] != 'Close' or right['text'] != 'anyway':
                continue
            if any(left[key] != right[key] for key in
                   ('page_num', 'block_num', 'par_num', 'line_num')):
                continue
            x = int(left['left'])
            y = min(int(left['top']), int(right['top']))
            width = int(right['left']) + int(right['width']) - x
            height = max(int(left['height']), int(right['height']))
            matches.append((x + width // 2, y + height // 2))
        return matches[0] if len(matches) == 1 else None

    def close_app(self, app):
        requested = time.monotonic()
        approved = requested
        # Previous cases may leave the cursor over this same dialog button.
        # Park it before observing text; never infer obscured button wording.
        if self.backend == 'x11':
            self.run(['xdotool', 'mousemove', '--sync', '1279', '450'])
        else:
            proc = getattr(self, '_close_pointer', None)
            if proc is not None:
                assert proc.poll() is None, 'Close pointer disappeared'
                proc.stdin.write(b'move 1279 450 1280 900\n')
                proc.stdin.flush()
                self.pointer_reply(proc, 'done')
        if self.backend == 'x11':
            self.key('F4', ('alt',))
        else:
            self.run(['swaymsg', '[app_id="io.github.beamiter.frost"]', 'kill'])
        def closed_or_dialog():
            if app.poll() is not None:
                return 'closed'
            name, rows = self.observe_frame('graceful-close', enhanced=True)
            button = self.close_button(rows)
            return (name, button) if button else None
        result = wait_for(closed_or_dialog, 'application exit or disconnected Eva close dialog')
        if result != 'closed':
            name, (x, y) = result
            approved = time.monotonic()
            if self.backend == 'x11':
                self.run(['xdotool', 'mousemove', '--sync', str(x), str(y), 'click', '1'])
            else:
                pointer = self.env.get('FROST_SMOKE_POINTER')
                assert pointer, 'Wayland graceful close requires the public protocol pointer'
                # A one-shot pointer can withdraw seat capability before the
                # application drains its button-release event. Keep this public
                # protocol device alive until the whole fixture is cleaned up.
                proc = getattr(self, '_close_pointer', None)
                if proc is None:
                    log = open(self.out / 'close-pointer.log', 'w')
                    self.logs.append(log)
                    proc = subprocess.Popen([pointer], env=self.env, stdin=subprocess.PIPE,
                                            stdout=subprocess.PIPE, stderr=log, bufsize=0)
                    self.processes.append(proc)
                    self._close_pointer = proc
                    self.pointer_reply(proc, 'ready')
                assert proc.poll() is None, 'Close pointer disappeared'
                proc.stdin.write(f'click {x} {y} 1280 900\n'.encode())
                proc.stdin.flush()
                self.pointer_reply(proc, 'done')
            (self.out / (name + '.json')).write_text(json.dumps({
                'visible_warning': 'Eva disconnected', 'action': 'Close anyway',
                'button_center': [x, y]}, indent=2))
        app.wait(timeout=20)
        assert app.returncode == 0, app.returncode
        return {'requested': requested, 'approved': approved, 'exited': time.monotonic()}

    @staticmethod
    def pointer_reply(proc, expected):
        data = bytearray()
        deadline = time.monotonic() + 5
        while not data.endswith(b'\n'):
            remaining = deadline - time.monotonic()
            assert remaining > 0 and select.select([proc.stdout], [], [], remaining)[0], 'Close pointer timeout'
            chunk = os.read(proc.stdout.fileno(), 1)
            assert chunk, 'Close pointer EOF'
            data.extend(chunk)
            assert len(data) <= 64, 'Invalid close pointer response'
        assert data == (expected + '\n').encode(), data

    def grid_record(self, label):
        assert re.fullmatch(r'dpi-[0-9]+', label), label
        path = self.out / (label + '-stty.txt')
        assert not path.exists(), 'DPI observation must not reuse a previous receipt'
        # This short fixture command fits with the prompt at 200% DPI (27 columns).
        # Record identity and stty in this same command; a second long sentinel
        # would grow the input dock before the matching native screenshot.
        self.text('g ' + label)
        self.key('Return')
        wait_for(lambda: path.exists() and path.read_text().endswith('\n'), 'exact DPI shell record')
        pid, tty, rows, cols = path.read_text().split()
        assert re.fullmatch(r'/dev/pts/[0-9]+', tty), tty
        return {'pid': int(pid), 'tty': tty, 'rows': int(rows), 'cols': int(cols)}

    def wait_clipboard_owner(self, owner, expected, label):
        # Probe only this isolated synthetic selection, never retry paste/Return.
        # A sleep is not proof that wl-copy owns the selection yet.
        def ready():
            assert owner.poll() is None, 'Clipboard owner exited before readiness'
            try:
                probe = subprocess.run(['wl-paste', '--no-newline'], env=self.env,
                                       capture_output=True, timeout=1)
            except subprocess.TimeoutExpired:
                return False
            return probe.returncode == 0 and probe.stdout == expected
        wait_for(ready, 'exact clipboard owner ' + label, timeout=5)

    def command(self, sentinel, paste=False):
        command = "printf '%s:%s:%s\\n' " + shlex.quote(sentinel) + ' "$$" "$(tty)" >> ' + shlex.quote(str(self.transcript))
        if paste:
            if self.backend == 'x11':
                # Keep the owner as a tracked child instead of daemonizing.
                log = open(self.out / ('clipboard-' + sentinel + '.log'), 'w')
                self.logs.append(log)
                owner = subprocess.Popen(['xclip', '-selection', 'clipboard', '-quiet'], env=self.env,
                                         stdin=subprocess.PIPE, stdout=log, stderr=log)
            else:
                log = open(self.out / ('clipboard-' + sentinel + '.log'), 'w')
                self.logs.append(log)
                owner = subprocess.Popen(['wl-copy', '--foreground'], env=self.env,
                                         stdin=subprocess.PIPE, stdout=log, stderr=log)
            self.processes.append(owner)
            owner.stdin.write(command.encode())
            owner.stdin.close()
            if self.backend == 'wayland':
                self.wait_clipboard_owner(owner, command.encode(), sentinel)
            else:
                time.sleep(0.5)
            self.key('v', ('ctrl', 'shift'))
        else:
            self.text(command)
        self.screenshot(sentinel + '-before-enter')
        self.key('Return')
        wait_for(lambda: self.transcript.exists() and any(line.startswith(sentinel + ':') for line in self.transcript.read_text().splitlines()),
                 'PTY sentinel ' + sentinel)
        # The command must execute exactly once; this does not simulate IME composition.
        time.sleep(0.3)
        matches = [line for line in self.transcript.read_text().splitlines() if line.startswith(sentinel + ':')]
        assert len(matches) == 1, matches
        return matches[0].split(':', 1)[1]

    def app_window(self):
        if self.backend == 'x11':
            result = subprocess.run(['xdotool', 'search', '--onlyvisible', '--classname', 'io.github.beamiter.frost'],
                                    env=self.env, text=True, capture_output=True, timeout=3)
            if result.returncode == 0 and result.stdout.strip():
                window = result.stdout.splitlines()[-1]
                self.run(['xdotool', 'windowactivate', '--sync', window])
                return window
            return None
        result = subprocess.run(['swaymsg', '-t', 'get_tree', '-r'], env=self.env, text=True, capture_output=True, timeout=3)
        if result.returncode:
            return None
        tree = json.loads(result.stdout)
        (self.out / 'sway-tree.json').write_text(result.stdout)
        def find(node):
            if node.get('app_id') == 'io.github.beamiter.frost':
                return node['id']
            for child in node.get('nodes', []) + node.get('floating_nodes', []):
                hit = find(child)
                if hit:
                    return hit
        found = find(tree)
        if found:
            self.run(['swaymsg', '[con_id=' + str(found) + ']', 'focus'])
        return found

    def exercise(self, mode):
        self.config.write_text('shell = "/bin/bash"\nterminal_mode = "' + mode + '"\ninitial_width = 1100\ninitial_height = 760\nrestore_session = false\n')
        app = self.spawn([str(self.binary)], 'frost-' + mode)
        try:
            wait_for(self.app_window, 'Frost window ' + mode)
            time.sleep(2)
            assert app.poll() is None
            self.screenshot(mode + '-startup')
            self.select_terminal(require_terminal=True)
            first_identity = self.command(mode + '-typed')
            self.command(mode + '-paste', paste=True)
            self.key('t', ('ctrl', 'shift'))
            time.sleep(1)
            second_identity = self.command(mode + '-second-session')
            assert second_identity != first_identity, 'New session shortcut did not change PTY identity'
            self.key('Page_Up', ('ctrl',))
            returned_identity = self.command(mode + '-focus-returned')
            assert returned_identity == first_identity, 'Previous session shortcut did not restore PTY identity'
            self.screenshot(mode + '-completed')
        except Exception:
            self.screenshot(mode + '-failure-before-cleanup')
            raise
        finally:
            if app.poll() is None:
                app.terminate()
                try:
                    app.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    app.kill()
                    app.wait()

    def close(self):
        for proc in reversed(self.processes):
            if proc.poll() is None:
                proc.terminate()
                try:
                    proc.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    proc.kill()
                    proc.wait()
        for log in self.logs:
            log.close()
        self._runtime.cleanup()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True, type=Path)
    parser.add_argument('--backend', required=True, choices=['x11', 'wayland'])
    parser.add_argument('--output', required=True, type=Path)
    args = parser.parse_args()
    args.binary = args.binary.resolve(strict=True)
    args.output = args.output.resolve()
    args.output.mkdir(parents=True, exist_ok=False)
    result = {'backend': args.backend, 'binary_sha256': hashlib.sha256(args.binary.read_bytes()).hexdigest(),
              'passed_modes': [], 'excluded': ['IME composition', 'physical GPU', 'multi-output scale',
              'live mode switch', 'recovery', 'OSC52 initial allocation and UI latency']}
    smoke = Smoke(args.binary, args.backend, args.output)
    try:
        smoke.start_desktop()
        if args.backend == 'wayland':
            # Match the genuine clipboard/IME fixtures: keep a keyboard on the
            # headless seat while one-shot wtype devices come and go. Otherwise
            # selection can remain nil until the next input device is created.
            smoke.spawn(['wtype', '-s', '600000', '-k', 'F24'], 'smoke-seat-keyboard')
        for mode in ('normal', 'block', 'chat'):
            smoke.exercise(mode)
            result['passed_modes'].append(mode)
        result['status'] = 'pass'
    except Exception as error:
        result.update(status='fail', error=str(error))
        try:
            smoke.screenshot('failure')
        except Exception:
            pass
        raise
    finally:
        smoke.close()
        (args.output / 'result.json').write_text(json.dumps(result, indent=2))


if __name__ == '__main__':
    main()
