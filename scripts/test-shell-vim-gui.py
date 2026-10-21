#!/usr/bin/env python3
"""Exercise a real shell -> native Vim in the same GUI -> original shell round trip.

Use under Xvfb with --binary and an unused --output directory. Requires
xdotool, ImageMagick import, Pillow and Vim 9. No installed config is changed.
"""
import argparse
import json
import os
from pathlib import Path
import shlex
import subprocess
import time

from PIL import Image


def main():
    os.umask(0o077)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True, type=Path)
    parser.add_argument('--frontend', choices=('frost', 'forge', 'anvil'), default='frost')
    parser.add_argument('--terminal-mode', choices=('normal', 'vte', 'block'), default=None)
    parser.add_argument('--output', required=True, type=Path)
    args = parser.parse_args()
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    for name in ('config', 'state', 'cache', 'data', 'runtime', 'project'):
        (out / name).mkdir(mode=0o700)
    frontend = args.frontend
    terminal_mode = args.terminal_mode or ('normal' if frontend == 'frost' else 'vte')
    (out / 'config' / frontend).mkdir()
    configuration = (
        'font_size = 18.0\ninitial_width = 1200.0\ninitial_height = 800.0\n'
        'restore_session = false\n'
        if frontend == 'frost' else
        'font = "Monospace 14"\nopacity = 1.0\ntab_placement = "top"\nsidebar_visible = false\n'
    )
    configuration += (
        f'terminal_mode = "{terminal_mode}"\nshell = "/bin/bash"\n'
        'ascii_organism_enabled = false\njsh_update_check = "never"\n'
    )
    (out / 'config' / frontend / 'config.toml').write_text(configuration, encoding='utf-8')
    first = out / 'project' / 'first file.txt'
    second = out / 'project' / 'second file.txt'
    original = ''.join(f'GUI_ROW_{number:04d} Unicode 界 e\u0301\n' for number in range(1, 181))
    first.write_text(original, encoding='utf-8')
    second.write_text(original, encoding='utf-8')
    state_file = out / 'vim-state.json'
    vimrc = out / 'fixture.vim'
    quote = lambda value: "'" + str(value).replace("'", "''") + "'"
    vimrc.write_text(
        'set nocompatible viminfofile=NONE noswapfile nomore notermguicolors\n'
        'set runtimepath=$VIMRUNTIME packpath= noloadplugins\n'
        'set nowrap mouse=a laststatus=0 nonumber norelativenumber nospell nolist conceallevel=0\n'
        'syntax on\n'
        'highlight Normal guifg=#ccddee guibg=#101820 ctermfg=15 ctermbg=1\n'
        'highlight NativeRgb guifg=#19c3a7 guibg=#112233 ctermfg=196 ctermbg=21\n'
        'autocmd BufEnter * syntax match NativeRgb /^GUI_ROW_.*/\n'
        'function! FixtureState(timer) abort\n'
        f"call writefile([json_encode({{'cwd': getcwd(), 'argv': argv(), 'active': win_getid(), 'mode': mode(1), 'line': line('.'), 'file': expand('%:p'), 'modified': &modified, 'windows': map(getwininfo(), {{_, w -> {{'id': w.winid, 'col': w.wincol, 'row': w.winrow, 'width': w.width, 'height': w.height, 'topline': w.topline}}}})}})], {quote(str(state_file) + '.tmp')})\n"
        f'call rename({quote(str(state_file) + ".tmp")}, {quote(state_file)})\n'
        'endfunction\n'
        "call timer_start(30, function('FixtureState'), {'repeat': -1})\n", encoding='utf-8')
    env = os.environ.copy()
    env.pop('WAYLAND_DISPLAY', None)
    env.pop('JTERM_VIM9_BIN', None)
    env.pop('JTERM_VIM9_AUTO_GUI', None)
    env.update({
        'XDG_CONFIG_HOME': str(out / 'config'), 'XDG_STATE_HOME': str(out / 'state'),
        'XDG_CACHE_HOME': str(out / 'cache'), 'XDG_DATA_HOME': str(out / 'data'),
        'XDG_RUNTIME_DIR': str(out / 'runtime'), 'SHELL': '/bin/bash',
        'VIMINIT': "execute 'source ' . fnameescape(" + quote(vimrc) + ')',
        'WGPU_BACKEND': 'gl', 'LIBGL_ALWAYS_SOFTWARE': '1', 'WINIT_UNIX_BACKEND': 'x11',
    })
    log = (out / 'application.log').open('w')
    app = subprocess.Popen([str(args.binary.resolve())], cwd=out / 'project', env=env,
                           stdout=log, stderr=log)
    report = {}

    def xdo(*parts):
        result = subprocess.run(['xdotool', *map(str, parts)], env=env,
                                text=True, capture_output=True, timeout=5)
        if result.returncode and parts[0] != 'search':
            raise RuntimeError('xdotool failed: ' + result.stderr)
        return result.stdout.strip()

    def wait(probe, label):
        until = time.monotonic() + 25
        while time.monotonic() < until:
            value = probe()
            if value:
                return value
            if app.poll() is not None:
                raise RuntimeError(f'{label}: terminal exited {app.returncode}')
            time.sleep(0.04)
        raise RuntimeError('Timed out: ' + label)

    def state():
        try:
            return json.loads(state_file.read_text())
        except (FileNotFoundError, json.JSONDecodeError):
            return {}

    def type_command(text):
        xdo('type', '--clearmodifiers', '--delay', '8', text)
        xdo('key', 'Return')

    def vim_command(text):
        xdo('key', 'Escape')
        type_command(':' + text)

    def native_pixels(window, name):
        capture = out / (name + '.png')
        subprocess.run(['import', '-window', str(window), str(capture)], env=env,
                       check=True, timeout=5)
        image = Image.open(capture).convert('RGB')
        return sum(count for count, color in (image.getcolors(image.width * image.height) or [])
                   if color == (0x19, 0xc3, 0xa7))

    try:
        terminal = wait(lambda: xdo('search', '--onlyvisible', '--pid', app.pid),
                        'ordinary terminal').splitlines()[0]
        xdo('windowsize', '--sync', terminal, 1200, 800)
        xdo('windowfocus', '--sync', terminal)
        time.sleep(0.3)
        # A real prompt executes the command. Neither --ide nor the internal
        # frontend route is passed by this test.
        status_file = out / 'exit-zero'
        type_command('cd ' + shlex.quote(str(out / 'project')) + '; vim +42 -- ' + shlex.quote(first.name) + ' ' + shlex.quote(second.name)
                     + '; printf "%s" "$?" > ' + shlex.quote(str(status_file)))
        editor = terminal
        wait(lambda: state().get('line') == 42, 'editor in original tab')
        assert xdo('search', '--onlyvisible', '--pid', app.pid).splitlines() == [terminal]
        xdo('windowfocus', '--sync', editor)
        initial = wait(lambda: state() if state().get('line') == 42 else None, 'literal +line argument')
        assert initial['cwd'] == str(out / 'project')
        assert initial['argv'] == [first.name, second.name]
        assert initial['file'] == str(first)
        assert not status_file.exists(), 'shell advanced before Vim exited'
        report['auto_gui_rgb_pixels'] = wait(lambda: native_pixels(editor, 'shell-native'), 'native GUI RGB')
        assert report['auto_gui_rgb_pixels'] > 30
        report['shell_argv_cwd'] = initial
        vim_command('normal! gg')
        vim_command('vsplit')
        split = wait(lambda: state() if len(state().get('windows', [])) == 2 else None, 'two Vim splits')
        windows = sorted(split['windows'], key=lambda w: w['col'])
        active = split['active']
        target = next(w for w in windows if w['id'] != active)
        # Derive point from window's actual screen cell columns. The native
        # buffer has no optional toolbar/sidebar consuming this geometry.
        geom = xdo('getwindowgeometry', '--shell', editor)
        width = int(next(line.split('=')[1] for line in geom.splitlines() if line.startswith('WIDTH=')))
        height = int(next(line.split('=')[1] for line in geom.splitlines() if line.startswith('HEIGHT=')))
        x = width * (0.75 if target == windows[1] else 0.25)
        xdo('mousemove', '--window', editor, int(x), int(height * 0.4))
        xdo('click', '--repeat', '3', '--delay', '40', 5)
        scrolled = wait(lambda: state() if any(w['id'] == target['id'] and w['topline'] > target['topline']
                                              for w in state().get('windows', [])) else None,
                        'hovered inactive split scrolled')
        assert scrolled['active'] == active, 'scroll stole Vim focus'
        peer = next(w for w in scrolled['windows'] if w['id'] == active)
        assert peer['topline'] == next(w['topline'] for w in windows if w['id'] == active)
        assert first.read_text(encoding='utf-8') == original and not scrolled['modified']
        report['inactive_split_scroll'] = scrolled
        report['scrolled_rgb_pixels'] = wait(lambda: native_pixels(editor, 'shell-scrolled'), 'scrolled GUI RGB')
        xdo('windowfocus', '--sync', editor)
        vim_command("call setline(1, 'GUI_UNSAVED_CHANGE')")
        wait(lambda: state().get('modified'), 'modified buffer')
        vim_command('qa')
        time.sleep(0.25)
        assert not status_file.exists(), 'refused quit must keep shell waiting'
        assert state().get('modified'), 'refused quit must retain edits'
        vim_command('write')
        wait(lambda: first.read_text().startswith('GUI_UNSAVED_CHANGE'), 'saved edit')
        report['unsaved_quit_protected'] = True
        vim_command('qa')
        wait(lambda: status_file.exists(), 'return to original shell')
        assert status_file.read_text() == '0'
        assert app.poll() is None, 'original terminal must remain alive'
        report['shell_return_zero'] = True
        report['same_gui_window'] = True
        report['frontend'] = frontend
        report['terminal_mode'] = terminal_mode
        # A second entry validates that previous GUI authority was retired and
        # that a nonzero :cquit result reaches the original shell unchanged.
        xdo('windowfocus', '--sync', terminal)
        status_nonzero = out / 'exit-seven'
        type_command('vim -- ' + shlex.quote(first.name) + '; printf "%s" "$?" > '
                     + shlex.quote(str(status_nonzero)))
        editor = terminal
        wait(lambda: not status_nonzero.exists() and state().get('line') == 1, 'second editor in original tab')
        xdo('windowfocus', '--sync', editor)
        wait(lambda: native_pixels(editor, 'second-entry') > 30, 'second GUI attached')
        vim_command('cq 7')
        wait(lambda: status_nonzero.exists(), 'nonzero Vim return')
        assert status_nonzero.read_text() == '7'
        report['shell_cquit_status'] = 7
        xdo('windowfocus', '--sync', terminal)
        batch_status = out / 'batch-status'
        type_command('vim -es -u NONE -i NONE -n +q; printf "%s" "$?" > '
                     + shlex.quote(str(batch_status)))
        wait(lambda: batch_status.exists(), 'batch Vim remains terminal command')
        assert batch_status.read_text() == '0'
        assert xdo('search', '--onlyvisible', '--pid', app.pid).splitlines() == [terminal]
        report['batch_vim_no_gui'] = True
        (out / 'result.json').write_text(json.dumps(report, indent=2) + '\n')
        print(json.dumps(report, indent=2))
        type_command('exit')
        app.wait(timeout=10)
    except BaseException:
        report['last_vim_state'] = state()
        report['focus'] = xdo('getwindowfocus')
        for label in ('terminal', 'editor'):
            if label in locals():
                try:
                    native_pixels(locals()[label], 'failure-' + label)
                except (OSError, subprocess.SubprocessError):
                    pass
        (out / 'failure.json').write_text(json.dumps(report, indent=2) + '\n')
        raise
    finally:
        if app.poll() is None:
            app.terminate()
            try:
                app.wait(timeout=5)
            except subprocess.TimeoutExpired:
                app.kill()
                app.wait(timeout=5)
        (out / 'last-state.json').write_text(json.dumps(state(), indent=2) + '\n')
        log.close()


if __name__ == '__main__':
    main()
