#!/usr/bin/env python3
"""Exact visible Mission control identification, never fuzzy clicking."""
import importlib.util
from pathlib import Path

spec = importlib.util.spec_from_file_location('mission_smoke', Path(__file__).with_name('native-mission-smoke.py'))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)

def rows(text, line='1', confidence='95'):
    return [dict(text=word, conf=confidence, page_num='1', block_num='1', par_num='1',
                 line_num=line, left=str(10 + 30 * index), top='50', width='29', height='15')
            for index, word in enumerate(text.split())]

control = rows('Use observed commits')
assert module.phrase(control, 'Use observed commits') == (10, 50, 99, 65)
assert module.phrase(rows('Use previous commits'), 'Use observed commits') is None
assert module.phrase(rows('Use observed commits', confidence='20'), 'Use observed commits') is None
assert module.phrase(rows('Use observed commits') + rows('Use observed commits', line='2'),
                     'Use observed commits') is None
assert module.phrase(rows('Use observed') + rows('commits', line='2'), 'Use observed commits') is None
assert module.phrase(rows('FrostMissionFixture') + rows('FrostMissionFixture', line='2'),
                     'FrostMissionFixture') is None
assert module.phrase(rows('FrostMissionFixture') + rows('FrostMissionFixture', line='2'),
                     'FrostMissionFixture', unique=False) is not None
print('Mission exact controls and five negative/duplicate observation contracts passed')

assert module.ready_control(control, 'Use observed commits', (10, 50, 99, 65))
assert not module.ready_control(control, 'Use observed commits', (10, 70, 99, 85))
assert not module.ready_control(control + rows('Reading the host...', line='2'),
                                'Use observed commits', (10, 50, 99, 65))
assert not module.ready_control(control + rows('Reading the host…', line='2'),
                                'Use observed commits', (10, 50, 99, 65))
print('Busy and moving controls cannot receive a native click')
