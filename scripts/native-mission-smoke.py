#!/usr/bin/env python3
"""Visible Mission lifecycle through public native input, not application hooks.

Five disposable Git checkouts prove local inspection and fail-closed remote
status. They are not the real five-repository delivery combination.
"""
import argparse
import csv
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import sqlite3
import subprocess
import tempfile
import time


def phrase(rows, text, unique=True):
    wanted = text.split()
    found = []
    for start in range(len(rows)):
        words = rows[start:start + len(wanted)]
        if [word['text'] for word in words] != wanted:
            continue
        if any(float(word['conf']) < 65 for word in words):
            continue
        if len({tuple(word[k] for k in ('page_num', 'block_num', 'par_num', 'line_num'))
                for word in words}) != 1:
            continue
        left = min(int(w['left']) for w in words)
        top = min(int(w['top']) for w in words)
        right = max(int(w['left']) + int(w['width']) for w in words)
        bottom = max(int(w['top']) + int(w['height']) for w in words)
        found.append((left, top, right, bottom))
    return found[0] if found and (not unique or len(found) == 1) else None


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True, type=Path)
    parser.add_argument('--backend', required=True, choices=['x11', 'wayland'])
    parser.add_argument('--output', required=True, type=Path)
    args = parser.parse_args()
    out = args.output.resolve()
    out.mkdir(mode=0o700, parents=True, exist_ok=False)
    spec = importlib.util.spec_from_file_location('native_smoke', Path(__file__).with_name('native-desktop-smoke.py'))
    runner = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(runner)
    smoke = runner.Smoke(args.binary.resolve(strict=True), args.backend, out)
    temp = tempfile.TemporaryDirectory(prefix='fm-', dir='/tmp')
    checkouts, heads = [], {}
    for index, name in enumerate(('jterm_core', 'anvil', 'forge', 'ember', 'frost')):
        checkout = Path(temp.name) / str(index)
        checkout.mkdir()
        def git(*command):
            return subprocess.check_output(['git', '-C', str(checkout), *command], text=True).strip()
        git('init', '-b', 'master')
        git('config', 'user.name', 'Native Mission fixture')
        git('config', 'user.email', 'native-fixture@example.invalid')
        manifest = f'[package]\nname="{name}"\nversion="0.0.0"\n'
        lock = 'version = 4\n'
        if index:
            core = heads['beamiter/jterm_core']
            manifest += ('[dependencies]\njterm_core = { git="https://github.com/beamiter/jterm_core.git", '
                         f'rev="{core}" }}\n')
            lock += ('[[package]]\nname="jterm_core"\nversion="0.2.0"\n'
                     f'source="git+https://github.com/beamiter/jterm_core.git?rev={core}#{core}"\n')
        (checkout / 'Cargo.toml').write_text(manifest)
        (checkout / 'Cargo.lock').write_text(lock)
        git('add', 'Cargo.toml', 'Cargo.lock')
        git('commit', '-m', 'Disposable native Mission observation')
        heads['beamiter/' + name] = git('rev-parse', 'HEAD')
        checkouts.append(checkout)
    result = {'status': 'running', 'backend': args.backend,
              'binary_sha256': hashlib.sha256(args.binary.read_bytes()).hexdigest(),
              'fixture_heads': heads, 'assertions': [],
              'not_covered': ['real five-repository delivery', 'CI evidence ingestion', 'physical GPU']}
    pointer = None
    app = None
    sequence = 0

    def pointer_command(command):
        nonlocal pointer
        if pointer is None:
            path = smoke.env.get('FROST_SMOKE_POINTER')
            assert path, 'Public Wayland pointer required'
            log = (out / 'mission-pointer.log').open('w')
            smoke.logs.append(log)
            pointer = subprocess.Popen([path], env=smoke.env, stdin=subprocess.PIPE,
                                       stdout=subprocess.PIPE, stderr=log, bufsize=0)
            smoke.processes.append(pointer)
            smoke._close_pointer = pointer
            smoke.pointer_reply(pointer, 'ready')
        pointer.stdin.write((command + '\n').encode())
        pointer.stdin.flush()
        smoke.pointer_reply(pointer, 'done')

    def move(x, y):
        if args.backend == 'x11':
            smoke.run(['xdotool', 'mousemove', '--sync', str(x), str(y)])
        else:
            pointer_command(f'move {x} {y} 1280 900')

    def click(x, y):
        if args.backend == 'x11':
            smoke.run(['xdotool', 'mousemove', '--sync', str(x), str(y), 'click', '1'])
        else:
            pointer_command(f'click {x} {y} 1280 900')
        time.sleep(.15)

    def scroll(amount):
        move(1150, 450)
        if args.backend == 'x11':
            smoke.run(['xdotool', 'click', '--repeat', str(abs(amount)), '--delay', '60',
                       '5' if amount > 0 else '4'])
        else:
            pointer_command('scroll ' + str(amount))
        time.sleep(.2)

    def observe(label, control=False):
        nonlocal sequence
        sequence += 1
        move(1279, 450)
        name = f'{sequence:03d}-{label}'
        smoke.screenshot(name)
        source = out / (name + '.ocr.png')
        # Blue native buttons have dark text, unlike the light body labels.
        # A fixed luminance split preserves their glyphs; neither text nor
        # click coordinates are inferred from the expected control.
        transform = ['-threshold', '25%'] if control else ['-negate']
        scale = 3 if control else 2
        smoke.run(['convert', str(out / (name + '.png')), '-colorspace', 'Gray',
                   *transform, '-resize', f'{scale * 100}%', str(source)], capture_output=True)
        raw = smoke.run(['tesseract', str(source), 'stdout', '--psm', '11', 'tsv'],
                        text=True, capture_output=True).stdout
        (out / (name + '.tsv')).write_text(raw)
        rows = [r for r in csv.DictReader(io.StringIO(raw), delimiter='\t', quoting=csv.QUOTE_NONE)
                if r['level'] == '5' and r['text'].strip()]
        for row in rows:
            for key in ('left', 'top', 'width', 'height'):
                row[key] = str(round(int(row[key]) / scale))
        return name, rows

    def locate(text, label, direction=1, unique=True, control=False):
        for attempt in range(7):
            name, rows = observe(label, control)
            bounds = phrase(rows, text, unique)
            if bounds:
                (out / (name + '.target.json')).write_text(json.dumps({'text': text, 'bounds': bounds, 'profile': 'button-luminance' if control else 'body-inverse'}))
                return bounds
            if attempt < 6:
                scroll(3 * direction)
        raise AssertionError('Exact visible Mission control not found: ' + text)

    def press(text, label, direction=1):
        left, top, right, bottom = locate(text, label, direction, control=True)
        click((left + right) // 2, (top + bottom) // 2)

    def fill(label, value):
        left, top, right, bottom = locate(label, 'field-' + label.replace('/', '-'))
        if bottom > 760:
            scroll(2)
            left, top, right, bottom = locate(label, 'field-visible-' + label.replace('/', '-'))
        # A native labelled input begins immediately below its observed label.
        click(left + 50, bottom + 18)
        smoke.key('a', ('ctrl',))
        smoke.text(value)

    def stored():
        records = []
        for db in (out / 'home').rglob('*.sqlite*'):
            if db.name.endswith(('-wal', '-shm')):
                continue
            try:
                with sqlite3.connect(db.as_uri() + '?mode=ro', uri=True, timeout=.2) as connection:
                    rows = connection.execute('SELECT plan_json, report_json FROM workspace_missions').fetchall()
                records.extend((json.loads(plan), json.loads(report) if report else None) for plan, report in rows)
            except sqlite3.OperationalError:
                continue
        assert len(records) <= 1, 'A visible Create action created duplicate Missions'
        return records[0] if records else None

    try:
        smoke.start_desktop()
        smoke.config.write_text('shell="/bin/bash"\nterminal_mode="block"\nrestore_session=false\n'
                                'initial_width=1100\ninitial_height=760\n')
        app = smoke.spawn([str(smoke.binary)], 'frost-mission')
        runner.wait_for(smoke.app_window, 'Mission application window')
        time.sleep(2)
        smoke.select_terminal(require_terminal=True)
        press('Missions', 'open-missions')
        press('New Mission', 'new-mission')
        fill('Title', 'FrostMissionFixture')
        left, top, right, bottom = locate('Acceptance goal', 'new-goal')
        click(left + 50, bottom + 25)
        smoke.text('Inspect exact local commits without claiming delivery')
        for name, path in zip(('jterm_core', 'anvil', 'forge', 'ember', 'frost'), checkouts):
            fill('beamiter/' + name, str(path))
        press('Create Mission', 'create-mission')
        saved, _ = runner.wait_for(stored, 'visible Mission creation persisted')
        assert saved['title'] == 'FrostMissionFixture' and saved['revision'] == 1
        assert saved['goal'] == 'Inspect exact local commits without claiming delivery'
        assert [Path(r['path']) for r in saved['repositories']] == checkouts
        result['mission_id'] = saved['id']
        result['assertions'].append('Native labelled inputs created exactly one five-repository host plan')
        scroll(-20)
        press('New Mission', 'collapse-new-form', -1)
        press('Inspect delivery', 'inspect-local-commits')
        plan, report = runner.wait_for(lambda: (value if (value := stored()) and value[1] else None),
                                      'host inspection persisted', timeout=30)
        assert len(report['repositories']) == 5
        for repo in report['repositories']:
            assert repo['observed']['head'] == heads[repo['identity']['full_name']]
            assert not repo['pushed'] and not repo['integrated'] and not repo['validated']
        result['assertions'].append('Inspect read all five exact Git heads and did not invent remote or validation success')
        press('Use observed commits', 'adopt-fresh-observation')
        saved, _ = runner.wait_for(lambda: (value if (value := stored()) and value[0]['revision'] == 2 else None),
                                  'fresh candidate adoption persisted')
        assert all(r['candidate'] == heads[r['identity']['full_name']] for r in saved['repositories'])
        result['assertions'].append('One explicit adoption froze the five observed candidates')
        press('Inspect delivery', 'inspect-frozen-candidates')
        runner.wait_for(lambda: (value if (value := stored()) and value[1] else None), 'second inspection persisted')
        scroll(-20)
        press('Refresh', 'refresh-mission')
        time.sleep(1)
        press('Use observed commits', 'reject-historical-adoption')
        time.sleep(1)
        assert stored()[0]['revision'] == 2, 'Refresh allowed stale candidate adoption'
        result['assertions'].append('Refresh retired adoption permission; clicking the historical control could not write revision 3')
        scroll(-20)
        fill('Title', 'UnsavedMissionTitle')
        locate('Unsaved edits:', 'unsaved-warning')
        scroll(-20)
        press('Refresh', 'refresh-with-draft')
        time.sleep(1)
        locate('UnsavedMissionTitle', 'draft-preserved')
        assert stored()[0]['title'] == 'FrostMissionFixture'
        press('Reload saved', 'reload-saved-plan')
        time.sleep(1)
        scroll(-20)
        locate('FrostMissionFixture', 'saved-title-restored', unique=False)
        result['assertions'].append('Refresh preserved the visible unsaved title; explicit Reload restored the saved title')
        result['saved_plan'], result['last_host_report'] = stored()
        smoke.close_app(app)
        result['status'] = 'pass'
    except BaseException as error:
        result['error'] = repr(error)
        try:
            smoke.screenshot('failure-before-cleanup')
        except Exception:
            pass
        raise
    finally:
        smoke.close()
        temp.cleanup()
        (out / 'result.json').write_text(json.dumps(result, indent=2) + '\n')


if __name__ == '__main__':
    main()
