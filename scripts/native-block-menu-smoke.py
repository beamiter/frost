#!/usr/bin/env python3
"""Exercise block context menus in an isolated X11 desktop and real shell.

Requires Xvfb, openbox, xdotool, ImageMagick and tesseract. Keeps screenshots
and application logs, including on failure. No application messages are injected.
"""
import argparse
import csv
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import shlex
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True, type=Path)
    parser.add_argument('--output', required=True, type=Path)
    args = parser.parse_args()
    os.umask(0o077)
    binary = args.binary.resolve(strict=True)
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    spec = importlib.util.spec_from_file_location(
        'native_smoke', Path(__file__).with_name('native-desktop-smoke.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    smoke = module.Smoke(binary, 'x11', out)
    smoke.env['RUST_BACKTRACE'] = '1'
    result = {'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
              'backend': 'x11', 'passed_modes': [], 'status': 'running'}

    def click(point, button=1):
        smoke.run(['xdotool', 'mousemove', str(point[0]), str(point[1]),
                   'click', str(button)])

    def center(word):
        return (int(word['left']) + int(word['width']) // 2,
                int(word['top']) + int(word['height']) // 2)

    frame_number = 0

    def frame(label):
        nonlocal frame_number
        frame_number += 1
        name = f'{label}-{frame_number:03d}'
        smoke.screenshot(name)
        # Preserve the raw frame; normalize only the OCR copy of dark UI text.
        normalized = out / (name + '.ocr.png')
        smoke.run(['convert', str(out / (name + '.png')), '-colorspace', 'Gray',
                   '-negate', '-resize', '200%', str(normalized)], capture_output=True)
        tsv = smoke.run(['tesseract', str(normalized), 'stdout', '--psm', '11', 'tsv'],
                        text=True, capture_output=True).stdout
        (out / (name + '.tsv')).write_text(tsv)
        rows = [row for row in csv.DictReader(io.StringIO(tsv), delimiter='\t',
                quoting=csv.QUOTE_NONE) if row['level'] == '5' and row['text'].strip()]
        for row in rows:
            for field in ('left', 'top', 'width', 'height'):
                row[field] = str(round(int(row[field]) / 2))
        return rows

    def observe(app, label, menu_open):
        def ready():
            assert app.poll() is None, f'Frost exited with {app.returncode}'
            rows = frame(label)
            words = {row['text'] for row in rows}
            visible = {'Command', 'Block', 'Close'} <= words
            return rows if visible == menu_open else None
        return module.wait_for(ready, label, timeout=15)

    try:
        smoke.start_desktop()
        for mode in ('block', 'chat'):
            smoke.config.write_text(
                'shell = "/bin/bash"\nterminal_mode = "' + mode + '"\n'
                'initial_width = 1100\ninitial_height = 760\nrestore_session = false\n')
            app = smoke.spawn([str(binary)], 'frost-' + mode)
            window = module.wait_for(smoke.app_window, 'Frost window ' + mode)
            time.sleep(2)
            smoke.text("printf 'FROST_CONTEXT_TARGET\\n'")
            smoke.key('Return')

            def output_visible():
                assert app.poll() is None, f'Frost exited with {app.returncode}'
                rows = frame(mode + '-output')
                matches = [row for row in rows if row['text'] == 'FROST_CONTEXT_TARGET']
                return center(max(matches, key=lambda row: int(row['top']))) if matches else None
            target = module.wait_for(output_visible, 'completed synthetic block output')

            # Leave a real command pending throughout every menu transition.
            # Only the final Return may execute it, with its exact text intact.
            sentinel = out / (mode + '-prompt-preserved.txt')
            smoke.text("printf 'FROST_CONTEXT_PROMPT_OK\\n' > " + shlex.quote(str(sentinel)))
            for dismissal in ('escape', 'close-button', 'outside'):
                click(target, 3)
                rows = observe(app, mode + '-menu-' + dismissal, True)
                assert not sentinel.exists(), 'Opening a menu executed pending shell input'
                if dismissal == 'escape':
                    smoke.key('Escape')
                elif dismissal == 'close-button':
                    close = [row for row in rows if row['text'] == 'Close']
                    assert len(close) == 1, 'Expected one visible menu close button'
                    click(center(close[0]))
                else:
                    geometry = smoke.run(
                        ['xdotool', 'getwindowgeometry', '--shell', window],
                        text=True, capture_output=True).stdout
                    bounds = dict(line.split('=', 1) for line in geometry.splitlines())
                    click((int(bounds['X']) + int(bounds['WIDTH']) - 30,
                           int(bounds['Y']) + int(bounds['HEIGHT']) - 50))
                observe(app, mode + '-dismissed-' + dismissal, False)
                assert not sentinel.exists(), 'Dismissing a menu executed pending shell input'

            if mode == 'block':
                smoke.key('F10', ('shift',))
                observe(app, mode + '-keyboard-menu', True)
                smoke.key('Escape')
                observe(app, mode + '-keyboard-dismissed', False)
            smoke.key('Return')
            module.wait_for(sentinel.exists, 'pending command after menu dismissal')
            assert sentinel.read_text() == 'FROST_CONTEXT_PROMPT_OK\n'
            assert app.poll() is None
            result['passed_modes'].append(mode)
            app.terminate()
            app.wait(timeout=10)
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
        (out / 'result.json').write_text(json.dumps(result, indent=2))
    print(json.dumps(result, indent=2))


if __name__ == '__main__':
    main()
