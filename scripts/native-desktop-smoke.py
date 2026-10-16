#!/usr/bin/env python3
"""Real compositor/PTY smoke. No synthetic application messages or guard bypasses.

Requires an installed Frost binary. Isolated HOME/XDG dirs and compositor ensure
clipboard contents are synthetic. Failure retains logs/screenshots/transcripts.
This is functional smoke, not an IME, hardware GPU, or latency certification.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shlex
import subprocess
import time


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
        runtime = out / 'runtime'
        runtime.mkdir(mode=0o700)
        home.mkdir()
        for key, suffix in [('HOME', ''), ('XDG_CONFIG_HOME', '.config'),
                            ('XDG_DATA_HOME', '.local/share'), ('XDG_STATE_HOME', '.local/state'),
                            ('XDG_CACHE_HOME', '.cache')]:
            self.env[key] = str(home / suffix)
        self.env.update(XDG_RUNTIME_DIR=str(runtime), LIBGL_ALWAYS_SOFTWARE='1',
                        WGPU_BACKEND='vulkan', RUST_LOG='warn')
        config = home / '.config/frost'
        config.mkdir(parents=True)
        self.config = config / 'config.toml'
        (home / '.bash_profile').write_text(r"""export HISTFILE=/dev/null
# Public OSC 133 bash integration documented in Frost's README. No private
# authentication token or foreground guard is overridden by this fixture.
__frost_smoke_prompt() {
    local status=$?
    if [[ ${__frost_smoke_seen:-0} == 1 ]]; then printf '\e]133;D;%s\a' "$status"; fi
    __frost_smoke_seen=1
}
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
        proc = subprocess.Popen(args, env=self.env, stdout=log, stderr=log)
        self.processes.append(proc)
        return proc

    def start_desktop(self):
        if self.backend == 'x11':
            self.env['DISPLAY'] = ':99'
            server = self.spawn(['Xvfb', ':99', '-screen', '0', '1280x900x24', '-nolisten', 'tcp'], 'xvfb')
            wait_for(lambda: subprocess.run(['xdpyinfo'], env=self.env, stdout=subprocess.DEVNULL,
                                            stderr=subprocess.DEVNULL, timeout=3).returncode == 0, 'X11 display')
            assert server.poll() is None
            self.spawn(['openbox', '--sm-disable'], 'openbox')
        else:
            cfg = self.out / 'sway.conf'
            cfg.write_text('xwayland disable\noutput * resolution 1280x900\nseat seat0 fallback true\nfocus_follows_mouse no\n')
            self.env.update(WLR_BACKENDS='headless', WLR_RENDERER='pixman', WLR_LIBINPUT_NO_DEVICES='1', WLR_HEADLESS_OUTPUTS='1')
            server = self.spawn(['sway', '-d', '-c', str(cfg)], 'sway')
            runtime = Path(self.env['XDG_RUNTIME_DIR'])
            sock = wait_for(lambda: next((p for p in runtime.glob('wayland-*') if p.is_socket()), None), 'Wayland socket')
            self.env['WAYLAND_DISPLAY'] = sock.name.removesuffix('.lock')
            ipc = wait_for(lambda: next(runtime.glob('sway-ipc.*.sock'), None), 'Sway IPC')
            self.env['SWAYSOCK'] = str(ipc)
            assert server.poll() is None
        time.sleep(1)

    def key(self, key, modifiers=()):
        if self.backend == 'x11':
            self.run(['xdotool', 'key', '--clearmodifiers', '+'.join((*modifiers, key))])
        else:
            args = ['wtype']
            for mod in modifiers:
                args += ['-M', mod]
            args += ['-k', key]
            for mod in reversed(modifiers):
                args += ['-m', mod]
            self.run(args)
        time.sleep(0.15)

    def text(self, text):
        if self.backend == 'x11':
            self.run(['xdotool', 'type', '--clearmodifiers', '--delay', '8', '--', text])
        else:
            self.run(['wtype', '-d', '8', '--', text])

    def screenshot(self, name):
        target = str(self.out / (name + '.png'))
        if self.backend == 'x11':
            self.run(['import', '-window', 'root', target])
        else:
            self.run(['grim', target])

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
            time.sleep(0.5)
            self.key('v', ('ctrl', 'shift'))
        else:
            self.text(command)
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
