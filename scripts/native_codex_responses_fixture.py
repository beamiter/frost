"""One synthetic Codex Responses turn for a disposable native test profile.

The two barriers let a display driver choose when to release the second
batch and when to finish the response; receipts record HTTP flushes only.
"""
from http.server import BaseHTTPRequestHandler, HTTPServer
import json
import re
import socket
import threading
import time


CLI_TAG = 'rust-v0.167.0-alpha.4'
MODEL = 'frost-fixture-model'
PROMPT = 'FROST OFFLINE TRANSCRIPT FIXTURE'
MAX_REQUEST = 256 * 1024
ROWS_PER_BATCH = 128
DELTAS_PER_BATCH = 8


def rows(first, stop):
    return ''.join(f'Frost row {n:04d}\n\n' for n in range(first, stop))


def sse(kind, **fields):
    payload = json.dumps({'type': kind, **fields}, separators=(',', ':'))
    return f'event: {kind}\ndata: {payload}\n\n'.encode('ascii')


def message(body):
    return {'id': 'msg-frost', 'type': 'message', 'role': 'assistant', 'phase': 'final_answer',
            'content': [{'type': 'output_text', 'text': body}]}


def is_fixture_request(body):
    if not isinstance(body, dict) or body.get('model') != MODEL or body.get('stream') is not True:
        return False
    inputs = body.get('input')
    if not isinstance(inputs, list):
        return False
    prompts = 0
    contexts = 0
    for item in inputs:
        if not isinstance(item, dict) or item.get('role') != 'user':
            continue
        content = item.get('content')
        if (item.get('type') != 'message' or not isinstance(content, list)
                or len(content) != 1 or not isinstance(content[0], dict)
                or content[0].get('type') != 'input_text'):
            return False
        value = content[0].get('text')
        if value == PROMPT:
            prompts += 1
        elif (isinstance(value, str) and value.startswith('<environment_context>')
              and value.endswith('</environment_context>')):
            # Tagged CLI sends a separate user-role metadata fragment before
            # the actual user prompt. Never retain its contents.
            contexts += 1
        else:
            return False
    return prompts == 1 and contexts <= 1


class Handler(BaseHTTPRequestHandler):
    def setup(self):
        self.request.settimeout(3)
        super().setup()

    def log_message(self, *_args):
        # Never log request headers, route strings, prompt, or body.
        pass

    def do_GET(self):
        if not re.fullmatch(r'/v1/models(?:\?client_version=[a-zA-Z0-9.+_-]{1,64})?', self.path):
            self.send_error(404)
            return
        body = json.dumps({'object': 'list', 'data': [
            {'id': MODEL, 'object': 'model', 'created': 0, 'owned_by': 'fixture'},
        ]}).encode('ascii')
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):
        if self.path != '/v1/responses':
            self.send_error(404)
            return
        fixture = self.server.fixture
        if fixture.claimed:
            self.send_error(409)
            return
        lengths = self.headers.get_all('Content-Length', [])
        if (len(lengths) != 1 or not lengths[0].isascii() or not lengths[0].isdecimal()
                or len(lengths[0]) > 6 or self.headers.get('Transfer-Encoding')):
            self.send_error(411)
            return
        length = int(lengths[0])
        if not 0 < length <= MAX_REQUEST:
            self.send_error(413)
            return
        try:
            body = self.rfile.read(length)
            if len(body) != length or not is_fixture_request(json.loads(body)):
                self.send_error(400)
                return
        except (ValueError, RecursionError, socket.timeout):
            self.send_error(400)
            return
        fixture.claimed = True
        try:
            self.send_response(200)
            self.send_header('Content-Type', 'text/event-stream')
            self.send_header('Cache-Control', 'no-cache')
            self.end_headers()
            self.stream(fixture)
        except OSError:
            fixture.record('client-disconnected')

    def stream(self, fixture):
        def emit(kind, **fields):
            self.wfile.write(sse(kind, **fields))
            self.wfile.flush()

        emit('response.created', response={'id': 'resp-frost'})
        emit('response.output_item.added', item=message(''))
        stride = ROWS_PER_BATCH // DELTAS_PER_BATCH
        for batch in range(2):
            start = 1 + batch * ROWS_PER_BATCH
            for first in range(start, start + ROWS_PER_BATCH, stride):
                if fixture.stop.is_set():
                    return
                emit('response.output_text.delta', delta=rows(first, first + stride))
                fixture.record('delta-flushed', first=first, last=first + stride - 1)
                if fixture.stop.wait(fixture.delta_interval):
                    return
            if not fixture.wait_for_release(batch + 1):
                return
        emit('response.output_item.done', item=message(rows(1, 2 * ROWS_PER_BATCH + 1)))
        emit('response.completed', response={'id': 'resp-frost', 'end_turn': True, 'usage': {
            'input_tokens': 1, 'input_tokens_details': None,
            'output_tokens': 1, 'output_tokens_details': None, 'total_tokens': 2,
        }})
        fixture.record('response-completed-flushed')


class LoopbackResponses:
    """Own one bounded, single-turn HTTP server on IPv4 loopback."""

    def __init__(self, *, barrier_timeout=20, delta_interval=.075):
        if not 0 < barrier_timeout <= 30 or not 0 <= delta_interval <= .25:
            raise ValueError('Invalid test timing')
        self.barrier_timeout = barrier_timeout
        self.delta_interval = delta_interval
        self.stop = threading.Event()
        self._waiting = None
        self._released = 0
        self.claimed = False
        self._receipts = []
        self._changed = threading.Condition()
        self.server = HTTPServer(('127.0.0.1', 0), Handler)
        self.server.fixture = self
        self.thread = threading.Thread(target=self.server.serve_forever,
                                       kwargs={'poll_interval': .05}, daemon=True)

    @property
    def base_url(self):
        return f'http://127.0.0.1:{self.server.server_address[1]}/v1'

    def __enter__(self):
        self.thread.start()
        return self

    def __exit__(self, *_args):
        self.stop.set()
        with self._changed:
            self._changed.notify_all()
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=1)

    def record(self, phase, **extra):
        with self._changed:
            self._receipts.append({'phase': phase, 'monotonic_ns': time.monotonic_ns(), **extra})
            self._changed.notify_all()

    def receipts(self):
        with self._changed:
            return [r.copy() for r in self._receipts]

    def wait_receipt(self, phase, timeout=5, **match):
        with self._changed:
            def find():
                return next((r for r in self._receipts if r['phase'] == phase
                             and all(r.get(k) == v for k, v in match.items())), None)
            receipt = self._changed.wait_for(find, timeout=timeout)
            if receipt is None:
                raise TimeoutError(f'No fixture receipt for {phase}')
            return receipt.copy()

    def wait_for_release(self, batch):
        with self._changed:
            self._waiting = batch
            self.record('barrier', batch=batch)
            released = self._changed.wait_for(
                lambda: self._released == batch or self.stop.is_set(), self.barrier_timeout)
            self._waiting = None
            if not released:
                self.record('barrier-timeout', batch=batch)
            return released and not self.stop.is_set()

    def release(self, batch):
        with self._changed:
            if self._waiting != batch or self._released != batch - 1 or self.stop.is_set():
                raise RuntimeError('Barrier is not waiting')
            self._released = batch
            self.record('driver-release', batch=batch)

    def write_profile(self, root, *, mode):
        """Fresh local config and a child environment (do not merge parent env)."""
        if mode not in ('scrollback', 'fullscreen'):
            raise ValueError('Invalid transcript mode')
        root = root.resolve()
        root.mkdir(mode=0o700, exist_ok=False)
        names = ('home', 'codex-home', 'config', 'data', 'state', 'cache', 'runtime', 'work')
        for name in names:
            (root / name).mkdir(mode=0o700)
        fullscreen = mode == 'fullscreen'
        config = f'''model = "{MODEL}"
model_provider = "frost_fixture"
approval_policy = "never"
sandbox_mode = "read-only"
web_search = "disabled"
check_for_update_on_startup = false

[analytics]
enabled = false

[feedback]
enabled = false

[model_providers.frost_fixture]
name = "Frost local fixture"
base_url = "{self.base_url}"
wire_api = "responses"
requires_openai_auth = false
supports_websockets = false
request_max_retries = 0
stream_max_retries = 0

[tui]
alternate_screen = "{'always' if fullscreen else 'never'}"
fullscreen_transcript = {str(fullscreen).lower()}
animations = false
auto_recap = false
notifications = false
'''
        (root / 'codex-home/config.toml').write_text(config, encoding='utf-8')
        env = {'PATH': '/usr/bin:/bin', 'HOME': str(root / 'home'),
               'CODEX_HOME': str(root / 'codex-home'),
               'LANG': 'C.UTF-8', 'TERM': 'xterm-256color',
               'NO_PROXY': '127.0.0.1', 'no_proxy': '127.0.0.1'}
        for name in ('config', 'data', 'state', 'cache', 'runtime'):
            env['XDG_' + name.upper() + ('_HOME' if name != 'runtime' else '_DIR')] = str(root / name)
        return env, root / 'work'
