#!/usr/bin/env python3
"""Exercise Frost's real Iced/Vim9 window with native XTest input.

Run under X11/Xvfb after cargo build --locked --bin frost:
  xvfb-run -a -s '-screen 0 1600x1000x24' python3 scripts/test-vim9-window.py target/debug/frost

Requires Python's standard library, xdotool, libX11, Vim 9 and ripgrep. All
files/configuration are isolated. Successful exit must be Vim's normal exit,
not a forced process termination or XDestroyWindow. This is not a Wayland or
physical-GPU/IME certification.
"""
from __future__ import annotations

import ctypes as C
import ctypes.util
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time


def require(condition: bool, message: str) -> None:
    if not condition:
        raise AssertionError(message)


def vim_string(value: Path) -> str:
    return "'" + str(value).replace("'", "''") + "'"


def read(path: Path) -> str:
    try:
        return path.read_text()
    except FileNotFoundError:
        return ""


def snapshot(path: Path) -> dict[str, bytes]:
    return {str(file.relative_to(path)): file.read_bytes() for file in path.rglob('*') if file.is_file()}


def wm_delete(window: int) -> None:
    """Send the same ICCCM request as a WM close button, without destroying X resources."""
    lib = C.CDLL(ctypes.util.find_library('X11') or 'libX11.so.6')
    class Data(C.Union):
        _fields_ = [('b', C.c_char * 20), ('s', C.c_short * 10), ('l', C.c_long * 5)]
    class ClientMessage(C.Structure):
        _fields_ = [('type', C.c_int), ('serial', C.c_ulong), ('send_event', C.c_int),
                    ('display', C.c_void_p), ('window', C.c_ulong), ('message_type', C.c_ulong),
                    ('format', C.c_int), ('data', Data)]
    class Event(C.Union):
        _fields_ = [('client', ClientMessage), ('pad', C.c_long * 24)]
    lib.XOpenDisplay.argtypes = [C.c_char_p]
    lib.XOpenDisplay.restype = C.c_void_p
    lib.XInternAtom.argtypes = [C.c_void_p, C.c_char_p, C.c_int]
    lib.XInternAtom.restype = C.c_ulong
    lib.XSendEvent.argtypes = [C.c_void_p, C.c_ulong, C.c_int, C.c_long, C.POINTER(Event)]
    lib.XSendEvent.restype = C.c_int
    lib.XFlush.argtypes = [C.c_void_p]
    lib.XCloseDisplay.argtypes = [C.c_void_p]
    display = lib.XOpenDisplay(None)
    require(bool(display), 'Cannot open the test X11 display')
    try:
        event = Event()
        event.client.type = 33  # ClientMessage
        event.client.send_event = 1
        event.client.display = display
        event.client.window = window
        event.client.message_type = lib.XInternAtom(display, b'WM_PROTOCOLS', 0)
        event.client.format = 32
        event.client.data.l[0] = lib.XInternAtom(display, b'WM_DELETE_WINDOW', 0)
        require(lib.XSendEvent(display, window, 0, 0, C.byref(event)) != 0, 'WM close request was not delivered')
        lib.XFlush(display)
    finally:
        lib.XCloseDisplay(display)


def main() -> None:
    require(len(sys.argv) == 2, 'Usage: test-vim9-window.py PATH_TO_FROST')
    binary = Path(sys.argv[1]).resolve(strict=True)
    require(bool(os.environ.get('DISPLAY')), 'Run this test under X11 or xvfb-run')
    require(shutil.which('xdotool') is not None, 'Install xdotool')
    with tempfile.TemporaryDirectory(prefix='frost-vim9-window-') as directory:
        root = Path(directory)
        home, project, state, data = [root / value for value in ('home', 'project', 'state', 'data')]
        for folder in (home, project, state, data, root / 'config', root / 'cache', root / 'runtime'):
            folder.mkdir(mode=0o700)
        (state / 'existing-workspace.bin').write_bytes(b'shell-state-must-remain-untouched\x00\xff')
        (data / 'existing-data.bin').write_bytes(b'existing-terminal-data\x00')
        before_state, before_data = snapshot(state), snapshot(data)
        first, second = project / 'first.txt', project / 'second odd |%#.txt'
        first.write_text('original\n')
        second.write_text('second\n')
        ready, modified, current = [root / name for name in ('ready', 'modified', 'current')]
        vimrc = (
            'set nocompatible nomore noswapfile\nset shortmess+=I\n'
            'set errorformat=%f:%l:%c:%m\n'
            f'autocmd VimEnter * call writefile(["ready"], {vim_string(ready)})\n'
            f'autocmd BufEnter * call writefile([expand("%:p")], {vim_string(current)})\n'
            f'autocmd TextChanged,TextChangedI,BufModifiedSet * call writefile([string(&modified)], {vim_string(modified)})\n'
        )
        (home / '.vimrc').write_text(vimrc)
        env = dict(os.environ, HOME=str(home), XDG_CONFIG_HOME=str(root / 'config'),
                   XDG_CACHE_HOME=str(root / 'cache'), XDG_STATE_HOME=str(state),
                   XDG_DATA_HOME=str(data), XDG_RUNTIME_DIR=str(root / 'runtime'),
                   LIBGL_ALWAYS_SOFTWARE='1')
        for variable in ('WAYLAND_DISPLAY', 'VIMINIT', 'EXINIT', 'GVIMINIT'):
            env.pop(variable, None)
        log_path = root / 'window.log'
        with log_path.open('wb') as log:
            process = subprocess.Popen([str(binary), '--ide', str(first)], cwd=project, env=env, stdout=log, stderr=log)
            try:
                def wait(predicate, description, timeout=15.0):
                    deadline = time.monotonic() + timeout
                    while not predicate():
                        require(process.poll() is None, f'Window exited unexpectedly while {description}: {read(log_path)[-8000:]}')
                        require(time.monotonic() < deadline, f'Timed out {description}: {read(log_path)[-8000:]}')
                        time.sleep(0.04)

                def xdo(*args: str) -> str:
                    return subprocess.check_output(['xdotool', *args], env=env, text=True, timeout=5).strip()

                wait(lambda: read(ready) == 'ready\n', 'waiting for real Vim startup', 30)
                window = int(xdo('search', '--onlyvisible', '--name', '^Frost - Vim9 IDE$').splitlines()[-1])
                xdo('windowfocus', '--sync', str(window))
                time.sleep(0.3)
                # No --window option: xdotool uses real XTest events, not a
                # synthetic application-level key message.
                xdo('type', '--clearmodifiers', '--delay', '30', 'inative-gui ')
                xdo('key', '--clearmodifiers', 'Escape')
                wait(lambda: read(modified) == '1\n', 'editing the real Vim buffer')
                xdo('key', '--clearmodifiers', 'ctrl+shift+s')
                wait(lambda: read(first) == 'native-gui original\n', 'saving through the native Save All shortcut')

                xdo('key', '--clearmodifiers', 'ctrl+shift+o')
                time.sleep(0.2)
                xdo('key', '--clearmodifiers', 'ctrl+a')
                xdo('type', '--clearmodifiers', '--delay', '5', str(second))
                xdo('key', '--clearmodifiers', 'Return')
                wait(lambda: read(current).strip() == str(second), 'opening a special filename through the native form')
                xdo('type', '--clearmodifiers', '--delay', '30', 'idirty ')
                xdo('key', '--clearmodifiers', 'Escape')
                wait(lambda: read(modified) == '1\n', 'making the second buffer dirty')

                wm_delete(window)
                time.sleep(0.8)
                require(process.poll() is None, 'WM close discarded a modified Vim buffer')
                require(read(second) == 'second\n', 'Close implicitly saved a dirty file')
                xdo('key', '--clearmodifiers', 'ctrl+shift+s')
                wait(lambda: read(second) == 'dirty second\n', 'saving after dirty-close refusal')

                # The native build form must preserve literal user review and
                # then start the same typed async build path as its Apply button.
                built = project / 'native-build.marker'
                xdo('key', '--clearmodifiers', 'ctrl+shift+b')
                time.sleep(0.2)
                xdo('type', '--clearmodifiers', '--delay', '10', "printf 'first.txt:1:1:build diagnostic\\n'; printf 'built\\n' > native-build.marker; exit 3")
                xdo('key', '--clearmodifiers', 'Return')
                wait(lambda: read(built) == 'built\n', 'building through the native form')
                time.sleep(0.3)
                wm_delete(window)
                require(process.wait(timeout=10) == 0, 'Saved editor did not exit gracefully')
                require(snapshot(state) == before_state, 'IDE changed persistent terminal state')
                require(snapshot(data) == before_data, 'IDE changed persistent terminal data')
                print('PASS: real XTest edit, native save/open/build, WM dirty-close refusal, graceful exit, unchanged terminal state/data')
            finally:
                # Cleanup applies only to this explicitly created test process
                # after a failure. It is never evidence for a successful exit.
                if process.poll() is None:
                    process.terminate()
                    try:
                        process.wait(timeout=3)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait(timeout=3)


if __name__ == '__main__':
    main()
