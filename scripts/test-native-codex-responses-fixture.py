#!/usr/bin/env python3
"""Exercise the driver contract through actual HTTP and temporary profiles."""
import http.client
import json
import os
from pathlib import Path
import tempfile
import tomllib
import unittest
from unittest.mock import patch
from urllib.parse import urlsplit

from native_codex_responses_fixture import (
    DELTAS_PER_BATCH, MAX_REQUEST, MODEL, PROMPT, ROWS_PER_BATCH,
    LoopbackResponses, rows,
)


def open_client(fixture):
    url = urlsplit(fixture.base_url)
    return http.client.HTTPConnection(url.hostname, url.port, timeout=4)


def request_turn(connection, prompt=PROMPT, extra_user=None):
    texts = ['<environment_context>\n<cwd>/fixture</cwd>\n</environment_context>', prompt]
    if extra_user is not None:
        texts.append(extra_user)
    connection.request('POST', '/v1/responses', json.dumps({
        'model': MODEL, 'stream': True, 'input': [{
            'type': 'message', 'role': 'user',
            'content': [{'type': 'input_text', 'text': text}],
        } for text in texts],
    }), {'Content-Type': 'application/json'})
    return connection.getresponse()


def event(response):
    line = response.readline()
    if not line:
        return None
    if not line.startswith(b'event: '):
        raise AssertionError('Missing SSE event name')
    tag = line.removeprefix(b'event: ').strip().decode()
    line = response.readline()
    if not line.startswith(b'data: ') or response.readline() != b'\n':
        raise AssertionError('Invalid SSE framing')
    payload = json.loads(line.removeprefix(b'data: '))
    if payload['type'] != tag:
        raise AssertionError('Type differs from event')
    return payload


class FixtureTest(unittest.TestCase):
    def test_two_gated_batches_one_exact_turn(self):
        with LoopbackResponses(barrier_timeout=3, delta_interval=.001) as f:
            c = open_client(f)
            self.addCleanup(c.close)
            with self.assertRaises(RuntimeError):
                f.release(1)
            with request_turn(c, 'not the synthetic prompt') as invalid:
                self.assertEqual(invalid.status, 400)
                invalid.read()
            with request_turn(c, extra_user='unexpected history') as invalid:
                self.assertEqual(invalid.status, 400)
                invalid.read()
            self.assertEqual(f.receipts(), [])
            with request_turn(c) as stream:
                self.assertEqual(stream.status, 200)
                self.assertEqual(stream.getheader('Content-Type'), 'text/event-stream')
                self.assertEqual(event(stream)['type'], 'response.created')
                self.assertEqual(event(stream)['item']['content'][0]['text'], '')
                all_text = []
                for batch in (1, 2):
                    for _ in range(DELTAS_PER_BATCH):
                        frame = event(stream)
                        self.assertEqual(frame['type'], 'response.output_text.delta')
                        all_text.append(frame['delta'])
                    barrier = f.wait_receipt('barrier', batch=batch)
                    self.assertEqual(len([r for r in f.receipts()
                                          if r['phase'] == 'delta-flushed']),
                                     batch * DELTAS_PER_BATCH)
                    self.assertFalse(any(r['phase'] == 'response-completed-flushed'
                                         for r in f.receipts()))
                    with self.assertRaises(RuntimeError):
                        f.release(3 - batch)
                    f.release(batch)
                    with self.assertRaises(RuntimeError):
                        f.release(batch)
                    released = f.wait_receipt('driver-release', batch=batch)
                    self.assertLessEqual(barrier['monotonic_ns'], released['monotonic_ns'])
                expected = rows(1, 2 * ROWS_PER_BATCH + 1)
                self.assertEqual(''.join(all_text), expected)
                self.assertEqual(event(stream)['item']['content'][0]['text'], expected)
                self.assertEqual(event(stream)['type'], 'response.completed')
                self.assertIsNone(event(stream))
            f.wait_receipt('response-completed-flushed')
            with request_turn(c) as duplicate:
                self.assertEqual(duplicate.status, 409)

    def test_timeout_does_not_complete_or_admit_late_release(self):
        with LoopbackResponses(barrier_timeout=.25, delta_interval=0) as f:
            c = open_client(f)
            self.addCleanup(c.close)
            with request_turn(c) as stream:
                events = list(iter(lambda: event(stream), None))
            self.assertEqual([e['type'] for e in events].count('response.output_text.delta'),
                             DELTAS_PER_BATCH)
            self.assertFalse(any(e['type'] == 'response.completed' for e in events))
            f.wait_receipt('barrier-timeout', batch=1)
            with self.assertRaises(RuntimeError):
                f.release(1)

    def test_cap_and_route_do_not_claim_turn(self):
        with LoopbackResponses(delta_interval=0) as f:
            c = open_client(f)
            self.addCleanup(c.close)
            for method, route, headers, status in (
                ('POST', '/v1/responses', {'Content-Length': str(MAX_REQUEST + 1)}, 413),
                ('POST', '/v1/responses', {'Transfer-Encoding': 'chunked'}, 411),
                ('POST', '/not-responses', {}, 404),
                ('GET', '/v1/models?client_version=0.167.0-alpha.4', {}, 200),
                ('GET', '/v1/models?unrelated=probe', {}, 404),
            ):
                c.request(method, route, headers=headers)
                with c.getresponse() as response:
                    self.assertEqual(response.status, status)
                    if status == 200:
                        self.assertEqual(json.load(response)['data'][0]['id'], MODEL)
                    else:
                        response.read()
            self.assertEqual(f.receipts(), [])
            self.assertFalse(f.claimed)

    def test_profiles_are_fresh_local_and_mode_specific(self):
        with tempfile.TemporaryDirectory() as temp, LoopbackResponses() as f:
            root = Path(temp)
            with patch.dict(os.environ, {'OPENAI_API_KEY': 'canary-key',
                                          'HTTP_PROXY': 'http://fixture-canary.invalid',
                                          'CODEX_HOME': '/should-not-read-profile'}):
                for mode, alternate in (('scrollback', 'never'), ('fullscreen', 'always')):
                    profile = root / mode
                    env, cwd = f.write_profile(profile, mode=mode)
                    self.assertNotIn('OPENAI_API_KEY', env)
                    self.assertNotIn('HTTP_PROXY', env)
                    self.assertEqual(cwd, profile / 'work')
                    self.assertEqual(env['HOME'], str(profile / 'home'))
                    self.assertEqual(env['CODEX_HOME'], str(profile / 'codex-home'))
                    self.assertEqual(profile.stat().st_mode & 0o777, 0o700)
                    file = Path(env['CODEX_HOME']) / 'config.toml'
                    with file.open('rb') as fh:
                        parsed = tomllib.load(fh)
                    provider = parsed['model_providers'][parsed['model_provider']]
                    self.assertEqual(provider['base_url'], f.base_url)
                    self.assertIs(provider['supports_websockets'], False)
                    self.assertIs(provider['requires_openai_auth'], False)
                    self.assertEqual(provider['request_max_retries'], 0)
                    self.assertEqual(provider['stream_max_retries'], 0)
                    self.assertEqual(parsed['tui']['alternate_screen'], alternate)
                    self.assertEqual(parsed['tui']['fullscreen_transcript'],
                                     mode == 'fullscreen')
                    self.assertEqual(parsed['sandbox_mode'], 'read-only')
                    with self.assertRaises(FileExistsError):
                        f.write_profile(profile, mode=mode)


if __name__ == '__main__':
    unittest.main()
