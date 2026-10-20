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
        nested = project / 'src'
        nested.mkdir()
        quick_a, quick_b = nested / 'deep needle a.rs', nested / 'deep needle b.rs'
        quick_a.write_text('line one\nline two\nline three\n')
        quick_b.write_text('line one\nline two\nline three\n')
        (project / '.git').mkdir()
        (project / '.gitignore').write_text('ignored/\n')
        ignored = project / 'ignored'
        ignored.mkdir()
        # An exact filename match would outrank both fuzzy matches if ignore
        # handling regressed, so selecting the second result detects it.
        (ignored / 'dpndl').write_text('not a quick-open candidate\n')
        position, windows, buffers = [root / name for name in ('position', 'windows', 'buffers')]
        ready, modified, current, mode = [root / name for name in ('ready', 'modified', 'current', 'mode')]
        vimrc = (
            'set nocompatible nomore noswapfile\nset shortmess+=I\n'
            'set errorformat=%f:%l:%c:%m\n'
            f'autocmd VimEnter * call writefile(["ready"], {vim_string(ready)})\n'
            f'autocmd ModeChanged * call writefile([v:event.new_mode], {vim_string(mode)})\n'
            f'autocmd BufEnter * call writefile([expand("%:p")], {vim_string(current)})\n'
            f'autocmd TextChanged,TextChangedI,BufWritePost * call writefile([string(&modified)], {vim_string(modified)})\n'
            # Channel callbacks can move the actual cursor without immediately
            # firing CursorMoved. Sample Vim's real position independently.
            'function! FrostRecordCursor(timer)\n'
            f'  call writefile([printf("%d:%d", line("."), col("."))], {vim_string(position)})\n'
            f'  call writefile([string(winnr("$"))], {vim_string(windows)})\n'
            f'  call writefile(map(getbufinfo({{"buflisted": 1}}), {{_, info -> info.name}}), {vim_string(buffers)})\n'
            'endfunction\n'
            "autocmd VimEnter * call timer_start(40, function('FrostRecordCursor'), {'repeat': -1})\n"
        )
        (home / '.vimrc').write_text(vimrc)
        env = dict(os.environ, HOME=str(home), XDG_CONFIG_HOME=str(root / 'config'),
                   XDG_CACHE_HOME=str(root / 'cache'), XDG_STATE_HOME=str(state),
                   XDG_DATA_HOME=str(data), XDG_RUNTIME_DIR=str(root / 'runtime'),
                   LIBGL_ALWAYS_SOFTWARE='1', ICED_BACKEND='wgpu')
        for variable in ('WAYLAND_DISPLAY', 'VIMINIT', 'EXINIT', 'GVIMINIT'):
            env.pop(variable, None)
        log_path = root / 'window.log'
        with log_path.open('wb') as log:
            process = subprocess.Popen([str(binary), '--ide', str(first)], cwd=project, env=env, stdout=log, stderr=log)
            try:
                def wait(predicate, description, timeout=15.0):
                    deadline = time.monotonic() + timeout
                    while True:
                        value = predicate()
                        if value:
                            return value
                        require(process.poll() is None, f'Window exited unexpectedly while {description}: {read(log_path)[-8000:]}')
                        require(time.monotonic() < deadline, f'Timed out {description}: {read(log_path)[-8000:]}')
                        time.sleep(0.04)

                def xdo(*args: str) -> str:
                    return subprocess.check_output(['xdotool', *args], env=env, text=True, timeout=5).strip()

                def visible_window():
                    result = subprocess.run(
                        ['xdotool', 'search', '--onlyvisible', '--name', '^Frost - Vim9 IDE$'],
                        env=env, text=True, capture_output=True, timeout=5,
                    )
                    windows = result.stdout.splitlines()
                    return windows[-1] if result.returncode == 0 and windows else None

                wait(lambda: read(ready) == 'ready\n', 'waiting for real Vim startup', 30)
                window = int(wait(visible_window, 'waiting for the visible WGPU window', 30))
                xdo('windowfocus', '--sync', str(window))
                time.sleep(0.3)
                # No --window option: xdotool uses real XTest events, not a
                # synthetic application-level key message.
                xdo('type', '--clearmodifiers', '--delay', '30', 'inative-gui ')
                xdo('key', '--clearmodifiers', 'Escape')
                wait(lambda: read(modified) == '1\n', 'editing the real Vim buffer')
                # Save immediately after Escape, while Vim may still be
                # finishing terminal key decoding. The request must reach its
                # next safe state instead of requiring another manual save.
                xdo('key', '--clearmodifiers', 'ctrl+shift+s')
                wait(lambda: read(first) == 'native-gui original\n', 'saving immediately after Escape with native Save All')
                wait(lambda: read(mode) == 'n\n', 'returning to Vim normal mode')

                xdo('key', '--clearmodifiers', 'ctrl+shift+o')
                time.sleep(0.2)
                xdo('key', '--clearmodifiers', 'ctrl+a')
                xdo('type', '--clearmodifiers', '--delay', '5', str(second))
                xdo('key', '--clearmodifiers', 'Return')
                wait(lambda: read(current).strip() == str(second), 'opening a special filename through the native form')
                xdo('type', '--clearmodifiers', '--delay', '30', 'idirty ')
                xdo('key', '--clearmodifiers', 'Escape')
                wait(lambda: read(modified) == '1\n', 'making the second buffer dirty')
                wait(lambda: read(mode) == 'n\n', 'finishing the second edit')

                wm_delete(window)
                time.sleep(0.8)
                require(process.poll() is None, 'WM close discarded a modified Vim buffer')
                require(read(second) == 'second\n', 'Close implicitly saved a dirty file')
                xdo('key', '--clearmodifiers', 'ctrl+shift+s')
                wait(lambda: read(second) == 'dirty second\n', 'saving after dirty-close refusal')
                # Quick Open is a native project filename picker, including
                # subsequence matching and keyboard result selection. The
                # ignored exact match must never displace either fuzzy result.
                xdo('key', '--clearmodifiers', 'ctrl+shift+p')
                time.sleep(0.8)
                xdo('type', '--clearmodifiers', '--delay', '30', 'dpndl')
                time.sleep(0.25)
                xdo('key', '--clearmodifiers', 'Down')
                xdo('key', '--clearmodifiers', 'Return')
                wait(lambda: read(current).strip() == str(quick_b), 'opening the selected recursive fuzzy match')

                xdo('key', '--clearmodifiers', 'ctrl+shift+g')
                time.sleep(0.2)
                xdo('key', '--clearmodifiers', 'ctrl+a')
                xdo('type', '--clearmodifiers', '--delay', '30', '2:3')
                xdo('key', '--clearmodifiers', 'Return')
                wait(lambda: read(position).strip() == '2:3', 'jumping through the native line:column form')
                xdo('type', '--clearmodifiers', '--delay', '30', 'iGOTO-')
                xdo('key', '--clearmodifiers', 'Escape')
                wait(lambda: read(modified) == '1\n', 'editing after native line navigation')
                wait(lambda: read(mode) == 'n\n', 'finishing the edit before native buffer actions')
                xdo('key', '--clearmodifiers', 'ctrl+shift+w')
                time.sleep(0.35)
                require(read(current).strip() == str(quick_b), 'Close buffer discarded unsaved changes')
                require(read(quick_b) == 'line one\nline two\nline three\n', 'Close buffer implicitly saved')
                xdo('key', '--clearmodifiers', 'ctrl+s')
                wait(lambda: read(quick_b) == 'line one\nliGOTO-ne two\nline three\n', 'saving with native Ctrl+S')
                xdo('key', '--clearmodifiers', 'ctrl+shift+Page_Up')
                wait(lambda: read(current).strip() == str(second), 'switching to the previous buffer')
                xdo('key', '--clearmodifiers', 'ctrl+shift+Page_Down')
                wait(lambda: read(current).strip() == str(quick_b), 'switching to the next buffer')
                xdo('key', '--clearmodifiers', 'ctrl+shift+w')
                wait(lambda: read(current).strip() in {str(first), str(second)}, 'closing the saved current buffer')
                # This intentionally follows NextBuffer immediately. Resolving
                # the target from the UI's last 80 ms snapshot closes the wrong
                # (previous) buffer instead of the actual current Vim buffer.
                def closed_only_current():
                    names = read(buffers).splitlines()
                    return str(second) in names and str(quick_b) not in names
                wait(closed_only_current, 'retaining the previous buffer after rapid current-buffer close')

                xdo('key', '--clearmodifiers', 'ctrl+shift+p')
                time.sleep(0.2)
                xdo('type', '--clearmodifiers', '--delay', '20', 'no_such_project_file_xyz')
                xdo('key', '--clearmodifiers', 'Escape')
                time.sleep(0.15)
                require(process.poll() is None, 'Cancelling empty Quick Open closed the editor')
                # Click the first split control in the native tools strip. The
                # independent Vim sampler must observe a real second window.
                xdo('mousemove', '--window', str(window), '130', '54')
                xdo('click', '1')
                wait(lambda: read(windows).strip() == '2', 'splitting through the native toolbar')

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
                print('PASS: real XTest edit, native save/open/build, recursive fuzzy picker and selection, line:column jump, native split, buffer navigation/dirty-close refusal, WM dirty-close refusal, graceful exit, unchanged terminal state/data')
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
