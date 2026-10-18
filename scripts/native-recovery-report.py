#!/usr/bin/env python3
"""Installed-binary recovery-banner acceptance in isolated X11/Wayland desktops.

This file is test-only. It sends real compositor keyboard/pointer input and reads
screenshots, real shell PID/TTY/stty output, and its own synthetic snapshot files.
No application-only messages, authentication bypasses, or user HOME are used.
--self-test runs only pure parsing/assertion tests: no compositor, PTY or chmod.
Ctrl+wheel, physical GPU/HiDPI and IME are explicitly outside this test's scope.
"""
import argparse
import csv
import errno
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import select
import signal
import stat
import struct
import subprocess
import time
import unittest
import uuid

READY = 'FROSTRECOVERYREADY'
SUMMARY = 'Recovered saved session: 1 invalid tab titles removed'
PRESERVED = 'The original saved session was preserved before repairs are saved.'
PAUSED = ('The original saved session could not be preserved. Automatic session saving '
          'is paused for this run; terminals remain usable.')
SCREEN = (1280, 900)
AUTOSAVE_OBSERVATION_SECONDS = 3.2  # Two existing 1500 ms ConfigTick intervals.
SUITE_BUDGET_SECONDS = 240  # Leave time for cleanup inside the 5-minute CI step.


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def fixture_bytes(home, repaired):
    snapshot = {
        'version': 2,
        'sessions': [{'cwd': str(home)}],
        'active_index': 0,
        'tabs': [{'tree': {'kind': 'leaf', 'session': 0}, 'focus': 0,
                  'title': 'bad\u0001title' if repaired else 'R07 CLEAN',
                  'pinned': False, 'marked': False, 'private_title': False}],
        'active_tab': 0,
    }
    return (json.dumps(snapshot, indent=2) + '\n').encode()


def parse_ocr(tsv):
    words = []
    for row in csv.DictReader(io.StringIO(tsv), delimiter='\t', quoting=csv.QUOTE_NONE):
        if row.get('level') != '5' or not row.get('text', '').strip():
            continue
        words.append({
            'text': row['text'].strip(), 'confidence': float(row['conf']),
            'left': int(row['left']), 'top': int(row['top']),
            'width': int(row['width']), 'height': int(row['height']),
            'line': tuple(int(row[key]) for key in ('page_num', 'block_num', 'par_num', 'line_num')),
        })
    return words


def ocr_lines(words):
    grouped = {}
    for word in words:
        grouped.setdefault(word['line'], []).append(word)
    lines = [sorted(line, key=lambda word: word['left']) for line in grouped.values()]
    return sorted(lines, key=lambda line: (min(word['top'] for word in line), line[0]['left']))


def exact_phrase(words, phrase):
    """Exact case/punctuation/token matches, allowing ordinary line wrapping."""
    expected = phrase.split()
    lines = ocr_lines(words)
    found = {}
    for index in range(len(lines)):
        # Banner paragraphs occupy at most three lines in the fixed test window.
        sequence = [word for line in lines[index:index + 3] for word in line]
        for start in range(len(sequence) - len(expected) + 1):
            match = sequence[start:start + len(expected)]
            if [word['text'] for word in match] == expected:
                identity = tuple((word['left'], word['top'], word['text']) for word in match)
                found[identity] = match
    return list(found.values())


def unique_dismiss(words):
    matches = [word for word in words if word['text'] == 'Dismiss']
    require(len(matches) == 1, f'Expected one exact Dismiss button, found {len(matches)}')
    word = matches[0]
    require(word['confidence'] >= 50 and word['width'] > 0 and word['height'] > 0,
            'Dismiss OCR box is not reliable')
    require(0 <= word['left'] < word['left'] + word['width'] <= SCREEN[0]
            and 0 <= word['top'] < word['top'] + word['height'] <= SCREEN[1],
            'Dismiss box is outside the captured desktop')
    return word


def banner_evidence(words, expected):
    anchors = [line for line in ocr_lines(words) if ' '.join(word['text'] for word in line) == READY]
    require(len(anchors) == 1, 'Missing or ambiguous exact terminal readiness output')
    if expected == 'absent':
        require(not exact_phrase(words, 'Recovered saved session:'), 'Unexpected recovery summary')
        require(not exact_phrase(words, PRESERVED) and not exact_phrase(words, PAUSED),
                'Unexpected preservation notice')
        require(not [word for word in words if word['text'] == 'Dismiss'], 'Unexpected Dismiss button')
        return {'expected': expected, 'positive_anchor': READY}
    require(expected in ('repaired', 'blocked'), 'Unknown expected banner state')
    dismiss = unique_dismiss(words)
    # The button is a separate action column, not part of a wrapped paragraph.
    # Restrict text to the actual column left of its unique measured box.
    report_words = [word for word in words if word['left'] + word['width'] <= dismiss['left']]
    summary = exact_phrase(report_words, SUMMARY)
    notice = exact_phrase(report_words, PAUSED if expected == 'blocked' else PRESERVED)
    require(len(summary) == len(notice) == 1, 'Missing or ambiguous exact recovery/preservation wording')
    matched = summary[0] + notice[0]
    require(all(word['confidence'] >= 40 for word in matched), 'Recovery text OCR confidence is too low')
    top = min(word['top'] for word in matched)
    bottom = max(word['top'] + word['height'] for word in matched)
    require(top - 25 <= dismiss['top'] <= bottom + 25,
            'Dismiss is not vertically adjacent to the recovery banner')
    require(dismiss['left'] > min(word['left'] for word in matched) + 150,
            'Dismiss is not in the banner action column')
    return {'expected': expected, 'summary': SUMMARY,
            'notice': PAUSED if expected == 'blocked' else PRESERVED,
            'positive_anchor': READY, 'dismiss': dismiss}


def parse_record(line):
    fields = line.rstrip('\n').split('\t')
    require(len(fields) == 4, f'Bad shell record: {line!r}')
    label, pid, tty, size = fields
    dimensions = size.split()
    require(pid.isdecimal() and int(pid) > 0 and tty.startswith('/dev/pts/'), 'Invalid real shell identity')
    require(len(dimensions) == 2 and all(value.isdecimal() for value in dimensions), 'Invalid stty output')
    rows, cols = map(int, dimensions)
    require(rows > 0 and cols > 0, 'PTY dimensions must be positive')
    return {'label': label, 'pid': int(pid), 'tty': tty, 'rows': rows, 'cols': cols}


def identity(record):
    return record['pid'], record['tty']


def assert_zoom(base, enlarged, reduced, reset):
    require(identity(base) == identity(enlarged) == identity(reduced) == identity(reset),
            'Zoom changed the original shell')
    require(enlarged['rows'] < base['rows'] and enlarged['cols'] < base['cols'],
            'Font increase did not reduce actual PTY rows and columns')
    require(reduced['rows'] > base['rows'] and reduced['cols'] > base['cols'],
            'Font decrease below baseline did not increase actual PTY rows and columns')
    require((reset['rows'], reset['cols']) == (base['rows'], base['cols']),
            'Ctrl+0 did not restore the actual baseline PTY grid')


def assert_dismiss_geometry(before, after):
    require(identity(before) == identity(after), 'Dismiss changed the original shell')
    require(after['rows'] > before['rows'] and after['cols'] == before['cols'],
            'Dismiss did not release banner height while preserving terminal width')


def assert_scale(records):
    require(len(records) == 4 and len({identity(record) for record in records}) == 1,
            'Scale probe changed the original shell')
    for dimension in ('rows', 'cols'):
        require(records[0][dimension] > records[1][dimension] > records[2][dimension] > 0,
                f'Wayland scale did not strictly shrink PTY {dimension}')
        require(records[3][dimension] == records[0][dimension], f'Wayland scale did not restore {dimension}')


class PrivateDirectory:
    """Owns only a fresh Smoke case's config directory; checks inode before chmod."""
    def __init__(self, path, case_root):
        self.path = path
        require(path.resolve().is_relative_to(case_root.resolve()) and not path.is_symlink(),
                'Permission fixture escaped its fresh case directory')
        self.fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC)
        try:
            info = os.fstat(self.fd)
            self.owner = os.geteuid()
            self.inode = (info.st_dev, info.st_ino)
            require(stat.S_ISDIR(info.st_mode) and info.st_uid == self.owner, 'Fixture directory is not owned')
            self.verify()
            os.fchmod(self.fd, 0o700)
        except BaseException:
            os.close(self.fd)
            raise

    def verify(self):
        current, opened = os.stat(self.path, follow_symlinks=False), os.fstat(self.fd)
        require(stat.S_ISDIR(current.st_mode) and current.st_uid == opened.st_uid == self.owner,
                'Fixture directory owner/type changed')
        require((current.st_dev, current.st_ino) == (opened.st_dev, opened.st_ino) == self.inode,
                'Fixture directory identity changed')

    def set_mode(self, mode):
        self.verify()
        require(mode in (0o500, 0o700), 'Unexpected fixture permission mode')
        os.fchmod(self.fd, mode)
        require(stat.S_IMODE(os.fstat(self.fd).st_mode) == mode, 'Fixture chmod did not take effect')

    def prove_writable(self, expected):
        self.verify()
        name = '.r07-write-probe'
        try:
            fd = os.open(name, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                         0o600, dir_fd=self.fd)
        except OSError as error:
            require(not expected and error.errno in (errno.EACCES, errno.EROFS),
                    f'Unexpected write-probe failure: {error}')
            return
        try:
            os.write(fd, b'synthetic write-capability probe\n')
            os.fsync(fd)
        finally:
            os.close(fd)
            os.unlink(name, dir_fd=self.fd)
        require(expected, 'Read-only fixture is still writable; cannot test the preservation failure')

    def close(self):
        try:
            self.set_mode(0o700)
        finally:
            os.close(self.fd)


def write_new_private(path, data):
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, 'wb') as stream:
        stream.write(data)
        stream.flush()
        os.fsync(stream.fileno())


def read_regular(path):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC)
    with os.fdopen(fd, 'rb') as stream:
        info = os.fstat(stream.fileno())
        require(stat.S_ISREG(info.st_mode) and info.st_uid == os.geteuid() and info.st_nlink == 1,
                f'Unexpected fixture file identity: {path.name}')
        data = stream.read(1024 * 1024 + 1)
        require(len(data) <= 1024 * 1024, 'Fixture file exceeded snapshot cap')
        return data, (info.st_dev, info.st_ino)


class Case:
    def __init__(self, module, args, name, result, deadline):
        self.module, self.args, self.name, self.result, self.deadline = module, args, name, result, deadline
        self.out = args.output / name
        self.out.mkdir(mode=0o700)
        self.smoke = module.Smoke(args.binary, args.backend, self.out)
        for key in ('WAYLAND_SOCKET', 'DBUS_SESSION_BUS_ADDRESS', 'SESSION_MANAGER', 'SSH_AUTH_SOCK'):
            self.smoke.env.pop(key, None)
        self.records = self.out / 'shell-records.tsv'
        self.smoke.env['FROST_RECOVERY_RECORD'] = str(self.records)
        self.directory = None
        self.pointer = None
        self.app = None
        self.window = None
        self.original = None
        self.counter = 0
        self.started = time.monotonic()
        self.result.update(case=name, status='running', shell_records=[], assertions=[])

    def budget(self):
        require(time.monotonic() < self.deadline, 'Recovery suite budget exhausted; preserving cleanup time')

    def record(self, label, command=None, original=False):
        self.budget()
        require(all(char.isalnum() or char == '-' for char in label), 'Unsafe synthetic label')
        self.smoke.text((command + '; ' if command else '') + 'r07 ' + label)
        self.smoke.key('Return')
        def observed():
            if not self.records.exists():
                return None
            matches = [parse_record(line) for line in self.records.read_text().splitlines(keepends=True)
                       if line.endswith('\n') and line.startswith(label + '\t')]
            require(len(matches) <= 1, 'Shell command executed more than once: ' + label)
            return matches[0] if matches else None
        record = self.module.wait_for(observed, 'real PTY record ' + label, timeout=10)
        if original:
            require(identity(record) == identity(self.original), 'Input moved away from the original tab')
        self.result['shell_records'].append(record)
        return record

    def ready(self, label):
        return self.record(label, 'clear; r07_ready', original=self.original is not None)

    def observe(self, expected, label):
        deadline = min(self.deadline, time.monotonic() + 12)
        last_error = None
        while time.monotonic() < deadline:
            self.budget()
            self.counter += 1
            name = f'{label}-{self.counter:02d}'
            self.smoke.screenshot(name)
            png = self.out / (name + '.png')
            data = png.read_bytes()
            require(data[:8] == b'\x89PNG\r\n\x1a\n' and struct.unpack('>II', data[16:24]) == SCREEN,
                    'Unexpected desktop capture extent; refusing guessed pointer coordinates')
            tsv = self.smoke.run(['tesseract', str(png), 'stdout', '--psm', '11', 'tsv'],
                                 text=True, capture_output=True).stdout
            (self.out / (name + '.tsv')).write_text(tsv)
            words = parse_ocr(tsv)
            try:
                evidence = banner_evidence(words, expected)
                evidence['screenshot'] = name + '.png'
                (self.out / (name + '.json')).write_text(json.dumps(evidence, indent=2))
                return evidence
            except AssertionError as error:
                last_error = str(error)
                (self.out / (name + '.json')).write_text(json.dumps({'error': last_error, 'words': words}, indent=2))
                time.sleep(0.2)  # Retry observation only; never re-send input/click.
        raise AssertionError(f'{label}: {last_error}')

    def pointer_reply(self, expected):
        data = bytearray()
        deadline = time.monotonic() + 5
        while not data.endswith(b'\n'):
            remaining = deadline - time.monotonic()
            require(remaining > 0 and select.select([self.pointer.stdout], [], [], remaining)[0],
                    'Native pointer reply timed out')
            chunk = os.read(self.pointer.stdout.fileno(), 1)
            require(chunk and len(data) < 64, 'Invalid native pointer reply')
            data.extend(chunk)
        require(data == (expected + '\n').encode(), f'Unexpected native pointer reply: {data!r}')

    def dismiss(self, evidence):
        require(self.smoke.app_window() == self.window, 'Application window changed before Dismiss')
        before = self.record('before-dismiss-grid', original=True)
        box = evidence['dismiss']
        x, y = box['left'] + box['width'] // 2, box['top'] + box['height'] // 2
        if self.args.backend == 'x11':
            self.smoke.run(['xdotool', 'mousemove', '--sync', str(x), str(y), 'click', '1'])
        else:
            self.pointer.stdin.write(f'click {x} {y} {SCREEN[0]} {SCREEN[1]}\n'.encode())
            self.pointer.stdin.flush()
            self.pointer_reply('done')
        after = self.ready('after-dismiss-ready')
        assert_dismiss_geometry(before, after)
        self.result['dismiss_geometry'] = [before, after]
        self.observe('absent', 'dismissed')
        self.result['assertions'].append('Dismiss increased actual PTY rows without changing columns or original shell')

    def check_original_unchanged(self, label, wait=False):
        require(self.result.get('writable_proven_after_paused_notice'), 'No writable positive control')
        if wait:
            started = time.monotonic()
            time.sleep(AUTOSAVE_OBSERVATION_SECONDS)
            self.result.setdefault('writable_observations', []).append(
                {'label': label, 'elapsed_seconds': time.monotonic() - started})
        self.directory.verify()
        require(stat.S_IMODE(os.fstat(self.directory.fd).st_mode) == 0o700, 'Directory became read-only again')
        actual, inode = read_regular(self.snapshot)
        require(actual == self.raw and inode == self.source_inode,
                'Blocked run replaced or changed its original snapshot at ' + label)
        require(not list(self.snapshot.parent.glob('session_history.json.recovered-*')), 'Blocked run created a late backup')
        self.result['assertions'].append('Writable directory; original bytes and inode unchanged: ' + label)

    def backups(self):
        files = sorted(self.snapshot.parent.glob('session_history.json.recovered-*'))
        require(len(files) == (1 if self.name == 'repaired' else 0), 'Unexpected original-backup count')
        for path in files:
            uuid.UUID(path.name.removeprefix('session_history.json.recovered-'))
            require(read_regular(path)[0] == self.raw, 'Backup differs from exact input bytes')
            require(stat.S_IMODE(path.stat().st_mode) == 0o600, 'Backup is not private')
        self.result['backup_names'] = [path.name for path in files]

    def tab_round_trip(self, label):
        self.smoke.key('t', ('ctrl', 'shift'))
        other = self.record(label + '-new-tab')
        require(identity(other) != identity(self.original), 'New tab did not create a different real shell')
        # Default Ctrl+1 is session:jump:0; do not assume where new tabs insert.
        self.smoke.key('1', ('ctrl',))
        returned = self.record(label + '-original-tab', original=True)
        require(self.smoke.app_window() == self.window, 'Tab operation replaced the application window')
        return returned

    def geometry(self):
        base = self.record('zoom-baseline', original=True)
        self.smoke.key('equal', ('ctrl',))
        self.smoke.key('equal', ('ctrl',))
        enlarged = self.record('zoom-increased', original=True)
        for _ in range(4):
            self.smoke.key('minus', ('ctrl',))
        reduced = self.record('zoom-decreased', original=True)
        self.smoke.key('0', ('ctrl',))
        reset = self.record('zoom-reset', original=True)
        assert_zoom(base, enlarged, reduced, reset)
        self.result['zoom'] = [base, enlarged, reduced, reset]
        self.result['assertions'].append('Real Ctrl+=/Ctrl+-/Ctrl+0 shrank, enlarged and restored stty rows/columns in the original shell')
        if self.args.backend == 'wayland':
            dimensions = []
            for index, scale in enumerate((1, 1.5, 2, 1)):
                self.smoke.run(['swaymsg', 'output', 'HEADLESS-1', 'scale', str(scale)])
                time.sleep(1)
                outputs = self.smoke.run(['swaymsg', '-t', 'get_outputs', '-r'], text=True, capture_output=True).stdout
                (self.out / f'scale-{index}-outputs.json').write_text(outputs)
                actual = [output for output in json.loads(outputs) if output.get('active') and output['name'] == 'HEADLESS-1']
                require(len(actual) == 1 and actual[0]['scale'] == scale, 'Compositor did not apply the requested scale')
                measured = self.record(f'scale-{index}', original=True)
                measured['scale'] = scale
                dimensions.append(measured)
                self.smoke.screenshot(f'scale-{index}')
            assert_scale(dimensions)
            self.result['scaling'] = dimensions
            self.result['assertions'].append('Real Wayland 1/1.5/2/1 scale: monotonic stty change and exact grid reset')

    def execute(self):
        self.budget()
        smoke = self.smoke
        self.directory = PrivateDirectory(smoke.config.parent, self.out)
        config = ('shell = "/bin/bash"\nterminal_mode = "normal"\nrestore_session = true\n'
                  'font_size = 14\nchrome_font_size = 14\nui_scale = 1\n'
                  'initial_width = 1100\ninitial_height = 760\nascii_organism_enabled = false\n'
                  'jsh_update_check = "never"\n')
        write_new_private(smoke.config, config.encode())
        write_new_private(smoke.config.parent / 'instance.lock', b'')
        home = Path(smoke.env['HOME'])
        with (home / '.bash_profile').open('a') as profile:
            profile.write(r'''
r07() { printf '%s\t%s\t%s\t%s\n' "$1" "$$" "$(tty)" "$(stty size)" >> "$FROST_RECOVERY_RECORD"; }
r07_ready() { printf '\106\122\117\123\124\122\105\103\117\126\105\122\131\122\105\101\104\131\n'; }
''')
        self.raw = fixture_bytes(home, self.name != 'clean')
        self.snapshot = smoke.config.parent / 'session_history.json'
        write_new_private(self.snapshot, self.raw)
        self.source_inode = read_regular(self.snapshot)[1]
        write_new_private(self.out / 'fixture-original.json', self.raw)
        self.result['fixture_sha256'] = sha256(self.raw)
        self.result['config_directory_inode'] = self.directory.inode
        if self.name == 'blocked':
            self.directory.set_mode(0o500)
            self.directory.prove_writable(False)
        smoke.start_desktop()
        if self.args.backend == 'wayland':
            smoke.spawn(['wtype', '-s', '600000', '-k', 'F24'], 'persistent-test-keyboard')
            log = open(self.out / 'pointer.log', 'w')
            smoke.logs.append(log)
            self.pointer = subprocess.Popen([str(self.args.pointer)], env=smoke.env,
                                            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=log, bufsize=0)
            smoke.processes.append(self.pointer)
            self.pointer_reply('ready')
        self.app = smoke.spawn([str(smoke.binary)], 'frost-' + self.name)
        self.window = self.module.wait_for(smoke.app_window, 'recovery test window', timeout=20)
        self.result['window_id'] = self.window
        require(self.app.poll() is None, 'Frost exited during startup')
        require((smoke.config.parent / 'instance.lock').read_text() == str(self.app.pid),
                'Test process did not acquire the real first-instance lock')
        self.original = self.ready('original-ready')
        expected = 'absent' if self.name == 'clean' else self.name
        self.observe(expected, 'startup')
        self.backups()
        if self.name == 'blocked':
            self.directory.verify()
            require(stat.S_IMODE(os.fstat(self.directory.fd).st_mode) == 0o500,
                    'Application changed the read-only fixture before paused OCR')
            self.directory.set_mode(0o700)
            self.directory.prove_writable(True)
            self.result['writable_proven_after_paused_notice'] = True
            self.check_original_unchanged('permission restored and two autosave ticks elapsed', wait=True)
        if self.name == 'repaired':
            self.geometry()
        self.tab_round_trip('visible-banner')
        self.ready('after-tab-ready')
        evidence = self.observe(expected, 'after-successful-tab-actions')
        if self.name != 'clean':
            if self.name == 'blocked':
                self.check_original_unchanged('successful tab creation while banner visible')
            self.dismiss(evidence)
            if self.name == 'blocked':
                self.check_original_unchanged('Dismiss and two autosave ticks elapsed', wait=True)
            self.tab_round_trip('dismissed-banner')
            self.ready('after-dismissed-tab-ready')
            self.observe('absent', 'after-dismissed-tab-actions')
            if self.name == 'blocked':
                self.check_original_unchanged('post-Dismiss tab creation and autosave ticks', wait=True)
        smoke.screenshot('before-graceful-close')
        if self.args.backend == 'x11':
            smoke.key('F4', ('alt',))
        else:
            smoke.run(['swaymsg', f'[con_id={self.window}]', 'kill'])
        self.app.wait(timeout=20)
        require(self.app.returncode == 0, f'Frost did not close gracefully: {self.app.returncode}')
        if self.name == 'blocked':
            self.check_original_unchanged('graceful close with a writable directory')
        else:
            saved = json.loads(read_regular(self.snapshot)[0])
            require(len(saved['sessions']) == (2 if self.name == 'clean' else 3),
                    'Successful tab operations were not persisted')
            if self.name == 'repaired':
                require(saved['tabs'][0].get('title') is None, 'Invalid fixture title was persisted')
            self.result['assertions'].append('Ordinary session writes persisted the actual new tabs after close')
        self.backups()
        lines = self.records.read_text().splitlines(keepends=True)
        require(all(line.endswith('\n') for line in lines), 'Incomplete final shell record')
        records = [parse_record(line) for line in lines]
        labels = [record['label'] for record in records]
        require(len(labels) == len(set(labels)) == len(self.result['shell_records']),
                'Shell record commands were duplicated or unaccounted for')
        self.result['status'] = 'pass'

    def cleanup(self):
        errors = []
        try:
            if self.directory is not None:
                self.directory.close()
        except Exception as error:
            errors.append('permission restore: ' + repr(error))
        try:
            self.smoke.close()
        except Exception as error:
            errors.append('process cleanup: ' + repr(error))
        if errors:
            self.result.update(status='fail', cleanup_errors=errors)
        self.result['elapsed_seconds'] = time.monotonic() - self.started
        (self.out / 'result.json').write_text(json.dumps(self.result, indent=2))
        require(not errors, '; '.join(errors))


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--self-test', action='store_true')
    for flag in ('runner', 'binary', 'pointer', 'output'):
        parser.add_argument('--' + flag, type=Path)
    parser.add_argument('--backend', choices=['x11', 'wayland'])
    args = parser.parse_args(argv)
    if args.self_test:
        suite = unittest.defaultTestLoader.loadTestsFromTestCase(PureTests)
        return 0 if unittest.TextTestRunner(verbosity=2).run(suite).wasSuccessful() else 1
    for name in ('runner', 'binary', 'backend', 'output'):
        if getattr(args, name) is None:
            parser.error('--' + name + ' is required')
    if args.backend == 'wayland' and args.pointer is None:
        parser.error('--pointer is required for Wayland')
    require(os.name == 'posix' and os.geteuid() != 0, 'Run as a non-root Unix user; readonly proof is mandatory')
    args.runner = args.runner.resolve(strict=True)
    args.binary = args.binary.resolve(strict=True)
    if args.pointer is not None:
        args.pointer = args.pointer.resolve(strict=True)
    args.output = args.output.resolve()
    args.output.mkdir(mode=0o700, parents=True, exist_ok=False)
    spec = importlib.util.spec_from_file_location('native_smoke', args.runner)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    result = {'backend': args.backend, 'binary_sha256': sha256(args.binary.read_bytes()),
              'status': 'running', 'cases': [],
              'not_covered': ['Ctrl+mouse-wheel zoom (no reliable modifier/pointer injection assertion)',
                              'physical GPU/HiDPI', 'IME', 'multi-output scaling'],
              'autosave_observation_seconds': AUTOSAVE_OBSERVATION_SECONDS}
    def interrupted(number, _frame):
        raise InterruptedError('Interrupted by signal ' + str(number))
    previous = {number: signal.signal(number, interrupted) for number in (signal.SIGTERM, signal.SIGINT)}
    started = time.monotonic()
    deadline = started + SUITE_BUDGET_SECONDS
    try:
        for name in ('clean', 'repaired', 'blocked'):
            details = {}
            result['cases'].append(details)
            case = Case(module, args, name, details, deadline)
            try:
                case.execute()
            except BaseException as error:
                details.update(status='fail', error=repr(error))
                try:
                    case.smoke.screenshot('failure-before-cleanup')
                except Exception:
                    pass
                raise
            finally:
                case.cleanup()
        result['status'] = 'pass'
    except BaseException as error:
        result.update(status='fail', error=repr(error))
        raise
    finally:
        result['elapsed_seconds'] = time.monotonic() - started
        (args.output / 'result.json').write_text(json.dumps(result, indent=2))
        for number, handler in previous.items():
            signal.signal(number, handler)
    return 0


class PureTests(unittest.TestCase):
    """No external programs, file creation, chmod, GUI or PTY in these tests."""
    @staticmethod
    def words(lines):
        output = []
        for index, line in enumerate(lines):
            left = 20
            for token in line.split():
                output.append({'text': token, 'confidence': 95.0, 'left': left, 'top': 40 + index * 20,
                               'width': len(token) * 7, 'height': 15, 'line': (1, index + 1, 1, 1)})
                left += len(token) * 7 + 7
        return output

    def test_minimal_fixture_has_only_one_title_defect(self):
        clean = json.loads(fixture_bytes(Path('/synthetic/home'), False))
        repaired = json.loads(fixture_bytes(Path('/synthetic/home'), True))
        repaired['tabs'][0]['title'] = clean['tabs'][0]['title']
        self.assertEqual(clean, repaired)
        self.assertEqual(clean['tabs'][0]['tree'], {'kind': 'leaf', 'session': 0})

    def test_clean_requires_positive_anchor(self):
        self.assertRaises(AssertionError, banner_evidence, [], 'absent')
        self.assertEqual(banner_evidence(self.words([READY]), 'absent')['positive_anchor'], READY)
        self.assertRaises(AssertionError, banner_evidence, self.words([READY, SUMMARY]), 'absent')

    def test_exact_banner_and_dismiss_not_fuzzy_or_ambiguous(self):
        words = self.words([PRESERVED, SUMMARY, READY])
        button = {'text': 'Dismiss', 'confidence': 95, 'left': 1000, 'top': 50,
                  'width': 60, 'height': 15, 'line': (1, 99, 1, 1)}
        words.append(button)
        self.assertEqual(banner_evidence(words, 'repaired')['summary'], SUMMARY)
        self.assertRaises(AssertionError, banner_evidence, words, 'blocked')
        self.assertRaises(AssertionError, unique_dismiss, words + [dict(button)])
        altered = [dict(word, text='recovered') if word['text'] == 'Recovered' else word for word in words]
        self.assertRaises(AssertionError, banner_evidence, altered, 'repaired')

    def test_phrase_can_wrap_without_approximate_matching(self):
        words = self.words(['Recovered saved session: 1', 'invalid tab titles removed'])
        self.assertEqual(len(exact_phrase(words, SUMMARY)), 1)
        self.assertFalse(exact_phrase(words, SUMMARY.replace('1', '2')))

    def test_wrapped_paused_text_is_not_confused_with_action_column(self):
        tokens = PAUSED.split()
        words = self.words([' '.join(tokens[:14]), ' '.join(tokens[14:]), SUMMARY, READY])
        words.append({'text': 'Dismiss', 'confidence': 95, 'left': 1100, 'top': 50,
                      'width': 60, 'height': 15, 'line': (1, 99, 1, 1)})
        self.assertEqual(banner_evidence(words, 'blocked')['notice'], PAUSED)

    def test_numeric_zoom_and_identity_checks(self):
        base = parse_record('base\t123\t/dev/pts/2\t40 100\n')
        smaller = dict(base, rows=35, cols=90)
        larger = dict(base, rows=45, cols=110)
        assert_zoom(base, smaller, larger, dict(base))
        self.assertRaises(AssertionError, assert_zoom, base, dict(base), larger, dict(base))
        self.assertRaises(AssertionError, assert_zoom, base, smaller, dict(base), dict(base))
        self.assertRaises(AssertionError, assert_zoom, base, dict(smaller, pid=456), larger, dict(base))
        self.assertRaises(AssertionError, parse_record, 'bad\t123\t/dev/pts/2\t0 100\n')

    def test_dismiss_reclaims_height_in_the_same_shell_only(self):
        before = {'pid': 123, 'tty': '/dev/pts/2', 'rows': 40, 'cols': 100}
        assert_dismiss_geometry(before, dict(before, rows=44))
        self.assertRaises(AssertionError, assert_dismiss_geometry, before, dict(before))
        self.assertRaises(AssertionError, assert_dismiss_geometry, before, dict(before, rows=44, cols=101))
        self.assertRaises(AssertionError, assert_dismiss_geometry, before, dict(before, rows=44, pid=456))

    def test_scale_requires_real_monotonic_change_and_reset(self):
        base = {'pid': 123, 'tty': '/dev/pts/2', 'rows': 40, 'cols': 100}
        records = [base, dict(base, rows=25, cols=65), dict(base, rows=18, cols=48), dict(base)]
        assert_scale(records)
        self.assertRaises(AssertionError, assert_scale, [base] * 4)
        self.assertRaises(AssertionError, assert_scale, records[:-1] + [dict(base, rows=39)])


if __name__ == '__main__':
    raise SystemExit(main())
