"""Exact native DPI oracle. Expected grids come from production Rust, never stty.

The small probe reuses built/locked iced and cosmic-text rlibs. It includes the
current reference Layout/native_viewport and extracts actual Metrics functions
verbatim. No GUI source, Cargo manifest, dependency or system setting is edited.
"""
from fractions import Fraction
import hashlib
import json
import math
import os
from pathlib import Path
import re
import shutil
import subprocess
import textwrap

ROOT = Path(__file__).resolve().parents[1]
SCALES = (1, 1.25, 1.5, 2, 1)


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def item(source, signature):
    declarations = list(re.finditer(r'^([ \t]*)' + re.escape(signature), source, re.M))
    require(len(declarations) == 1, 'Ambiguous production item: ' + signature)
    declaration = declarations[0]
    line, indent = declaration.start(), declaration.group(1)
    start = line + len(indent)
    opening = source.index('{', start)
    end = re.search(r'^' + re.escape(indent) + r'}[ \t]*$', source[opening:], re.M)
    require(end is not None, 'Missing production item closing brace')
    return textwrap.dedent(source[line:opening + end.end()])


def constant(source, name):
    matches = re.findall(r'^(?:pub )?const ' + name + r':[^;]+;', source, re.M)
    require(len(matches) == 1, 'Ambiguous production constant: ' + name)
    return matches[0]


def build_probe(output, deps=None, rustc=None):
    """Compile only a small observer against existing release dependencies."""
    output = Path(output).resolve()
    output.mkdir(parents=True, exist_ok=False)
    deps = Path(deps or Path(os.environ.get('CARGO_TARGET_DIR', ROOT / 'target')) / 'release/deps').resolve()
    rustc = str(rustc or shutil.which('rustc') or 'rustc')
    sources = {name: (ROOT / name).read_text() for name in
               ('src/main.rs', 'src/native_shell.rs', 'src/terminal_view.rs',
                'src/native_viewport.rs', 'src/terminal.rs', 'src/ui_typography.rs')}
    core = (ROOT.parent / 'jterm_core/src/reference_shell.rs').resolve(strict=True)
    hashes = {name: hashlib.sha256(text.encode()).hexdigest() for name, text in sources.items()}
    hashes[str(core)] = hashlib.sha256(core.read_bytes()).hexdigest()
    source_digest = hashlib.sha256()
    for path in sorted((ROOT / 'src').rglob('*.rs')):
        source_digest.update(str(path.relative_to(ROOT)).encode() + b'\0' + path.read_bytes())
    terminal = sources['src/terminal_view.rs']
    font_start = terminal.index('thread_local! {\n    // Share the font database')
    font_end = terminal.index('/// A missing named font', font_start)
    methods = '\n'.join(item(sources[name], signature) for name, signature in (
        ('src/native_shell.rs', 'pub(crate) fn reference_layout('),
        ('src/main.rs', 'fn term_width('),
        ('src/main.rs', 'fn term_height('),
        ('src/main.rs', 'fn block_reading_bar_height(')))
    body = (ROOT / 'scripts/native_scale_harness.rs').read_text()
    replacements = {
        'core-path': json.dumps(str(core)),
        'viewport-path': json.dumps(str((ROOT / 'src/native_viewport.rs').resolve())),
        'typography-path': json.dumps(str((ROOT / 'src/ui_typography.rs').resolve())),
        'metrics': item(terminal, 'pub struct Metrics {') + '\n' + item(terminal, 'impl Metrics {'),
        'font-system': terminal[font_start:font_end],
        'font-advance': item(terminal, 'fn measure_mono_ascii_advance('),
        'terminal-constants': '\n'.join(constant(terminal, key) for key in ('SCROLLBAR_WIDTH', 'BLOCK_GUTTER_WIDTH')),
        'clamp': '\n'.join(constant(sources['src/terminal.rs'], key) for key in
                           ('MAX_TERMINAL_COLS', 'MAX_TERMINAL_ROWS')) + '\n' +
                 item(sources['src/terminal.rs'], 'pub fn clamp_terminal_dimensions('),
        'methods': methods,
        'banner': item(sources['src/main.rs'], 'fn banner_height('),
    }
    for name, value in replacements.items():
        token = '@' + name + '@'
        require(body.count(token) == 1, 'Missing harness boundary: ' + token)
        body = body.replace(token, value)
    generated = output / 'production_probe.rs'
    generated.write_text(body)
    fingerprints = sorted((deps.parent / '.fingerprint').glob('frost-*/bin-frost.json'),
                          key=lambda path: path.stat().st_mtime, reverse=True)
    require(fingerprints, 'Build Frost release with its locked compiler before the geometry probe')
    app_fingerprint = json.loads(fingerprints[0].read_text())
    compiler_id = app_fingerprint['rustc']
    libraries = {}
    for name in ('iced', 'cosmic_text'):
        candidates = []
        dependency_ids = [entry[3] for entry in app_fingerprint['deps'] if entry[1] == name]
        require(len(dependency_ids) == 1, 'Missing production dependency identity: ' + name)
        for library in deps.glob('lib' + name + '-*.rlib'):
            folder = name.replace('_', '-') + '-' + library.stem.split('-')[-1]
            fingerprint = deps.parent / '.fingerprint' / folder / ('lib-' + name + '.json')
            checksum = fingerprint.with_suffix('')
            if (fingerprint.exists() and checksum.exists() and
                    json.loads(fingerprint.read_text()).get('rustc') == compiler_id and
                    int.from_bytes(bytes.fromhex(checksum.read_text()), 'little') == dependency_ids[0]):
                candidates.append(library)
        require(len(candidates) == 1, 'Expected one same-compiler locked ' + name + ' rlib; use a clean release dependency directory')
        libraries[name] = candidates[0]
    command = ([os.environ['RUSTC_WRAPPER']] if os.environ.get('RUSTC_WRAPPER') else [])
    command += [rustc, '--edition=2021', '-C', 'opt-level=1', '-L', 'dependency=' + str(deps)]
    if os.environ.get('CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER'):
        command += ['-C', 'linker=' + os.environ['CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER']]
    for name, library in libraries.items():
        command += ['--extern', name + '=' + str(library)]
    command += [str(generated), '-o', str(output / 'production_probe')]
    build_env = dict(os.environ, RUSTUP_AUTO_INSTALL='0')
    run = subprocess.run(command, capture_output=True, text=True, env=build_env)
    (output / 'compile.stdout').write_text(run.stdout)
    (output / 'compile.stderr').write_text(run.stderr)
    require(run.returncode == 0, 'Production geometry probe compile failed; see compile.stderr')
    for name, text in sources.items():
        require((ROOT / name).read_text() == text, 'Product source changed while building probe')
    require(hashlib.sha256(core.read_bytes()).hexdigest() == hashes[str(core)], 'Core changed while building probe')
    after_digest = hashlib.sha256()
    for path in sorted((ROOT / 'src').rglob('*.rs')):
        after_digest.update(str(path.relative_to(ROOT)).encode() + b'\0' + path.read_bytes())
    require(after_digest.hexdigest() == source_digest.hexdigest(), 'Rust source set changed while building probe')
    manifest = {'production_sources_sha256': hashes, 'production_fingerprint': str(fingerprints[0]),
                'rust_sources_sha256': source_digest.hexdigest(),
                'cargo_toml_sha256': hashlib.sha256((ROOT / 'Cargo.toml').read_bytes()).hexdigest(),
                'cargo_lock_sha256': hashlib.sha256((ROOT / 'Cargo.lock').read_bytes()).hexdigest(),
                'rustc': subprocess.check_output([rustc, '--version'], text=True, env=build_env).strip(),
                'command': command, 'rlibs': {name: {'path': str(path), 'sha256': hashlib.sha256(path.read_bytes()).hexdigest()}
                                            for name, path in libraries.items()},
                'generated_sha256': hashlib.sha256(generated.read_bytes()).hexdigest()}
    (output / 'source-manifest.json').write_text(json.dumps(manifest, indent=2))
    return output / 'production_probe'


def expected(probe, width, height, *, font_family='DejaVu Sans Mono', font_size=14,
             line_spacing=1, padding=2, reading_bar=True, recovery=False, environ=None):
    values = (width, height, font_size, line_spacing, padding)
    require(all(math.isfinite(value) and value > 0 for value in values[:4]) and
            math.isfinite(padding) and padding >= 0,
            'Invalid independently configured geometry/font input')
    command = ([os.environ['FROST_SCALE_PROBE_RUNTIME_WRAPPER']]
               if os.environ.get('FROST_SCALE_PROBE_RUNTIME_WRAPPER') else [])
    command += [str(probe), str(width), str(height), font_family, str(font_size),
               str(line_spacing), str(padding), str(int(reading_bar)), str(int(recovery))]
    result = json.loads(subprocess.check_output(command, text=True, env=environ))
    require(result['metrics']['mono_advance_exact'], 'Named font could not be independently measured by production shaper')
    return result


def owned_geometry(outputs, tree, pid, scale):
    """Actual compositor client bounds/DPR, not requested outer window size."""
    active = [entry for entry in outputs if entry.get('active') is True]
    require(len(active) == 1, 'Expected exactly one active private output')
    output = active[0]
    require(Fraction(str(output['scale'])) == Fraction(str(scale)) and output.get('transform') == 'normal',
            'Actual output scale/transform differs from requested scale')
    mode, rect = output['current_mode'], output['rect']
    for axis, pixels in (('width', mode['width']), ('height', mode['height'])):
        require(0 <= Fraction(pixels) - rect[axis] * Fraction(str(scale)) < Fraction(str(scale)),
                'Physical/logical output extent is inconsistent')
    found = []
    def visit(node):
        if node.get('app_id') == 'io.github.beamiter.frost':
            found.append(node)
        for child in node.get('nodes', []) + node.get('floating_nodes', []):
            visit(child)
    visit(tree)
    require(len(found) == 1, 'Absent/ambiguous actual Frost surface')
    node = found[0]
    require(node.get('pid') == pid and node.get('focused') is True, 'Native surface PID/focus mismatch')
    client, outer = node['window_rect'], node['rect']
    require(all(type(client.get(key)) is int for key in ('x', 'y', 'width', 'height')) and
            client['width'] > 0 and client['height'] > 0, 'Invalid native client rectangle')
    require(0 <= client['x'] and 0 <= client['y'] and
            client['x'] + client['width'] <= outer['width'] and
            client['y'] + client['height'] <= outer['height'], 'Native client escaped owned container')
    require(rect['x'] <= outer['x'] + client['x'] and rect['y'] <= outer['y'] + client['y'] and
            outer['x'] + client['x'] + client['width'] <= rect['x'] + rect['width'] and
            outer['y'] + client['y'] + client['height'] <= rect['y'] + rect['height'],
            'Native client escaped private output')
    return {'scale': float(scale), 'output_name': output['name'], 'physical': [mode['width'], mode['height']],
            'output_rect': rect, 'client': {'x': outer['x'] + client['x'], 'y': outer['y'] + client['y'],
                                         'width': client['width'], 'height': client['height']},
            'node_id': node['id'], 'pid': pid}


def verify_shell_pixels(ppm, native, production):
    """Check actually rendered panel edges, independently of PTY/grid reports.

    Narrow strips avoid labels/icons; vertical votes tolerate only isolated
    row highlights. Missing/wrong panels cannot be 'explained' by expected
    Layout alone. Raw native PNG is retained by the caller, never repainted.
    """
    header = re.match(br'P6\s+(\d+)\s+(\d+)\s+255\s', ppm)
    require(header is not None, 'Expected native eight-bit PPM frame')
    width, height = map(int, header.groups())
    data = ppm[header.end():]
    require([width, height] == native['physical'] and len(data) == width * height * 3,
            'Raw frame does not match actual physical output')
    client, output = native['client'], native['output_rect']
    scale = Fraction(str(native['scale']))
    top = production['titlebar_height'] + 100
    bottom = client['height'] - 60
    require(bottom > top, 'Insufficient native panel strip for independent geometry observation')
    ys = [top + (bottom - top) * (i + .5) / 32 for i in range(32)]

    def vote(x, color):
        px = math.floor((Fraction(str(client['x'] + x)) - output['x']) * scale)
        count = 0
        for y in ys:
            py = math.floor((Fraction(str(client['y'] + y)) - output['y']) * scale)
            require(0 <= px < width and 0 <= py < height, 'Panel observation escaped native frame')
            offset = (py * width + px) * 3
            count += all(abs(data[offset + channel] - color[channel]) <= 2 for channel in range(3))
        return count / len(ys)

    palette, rail = production['palette'], production['rail_width']
    observations = {'rail': vote(3, palette['rail'])}
    require(observations['rail'] >= .75, 'Native rail missing or incorrect geometry')
    nav, inspector = production['navigation_width'], production['inspector_width']
    observations['navigation_left'] = vote(rail + 3, palette['navigation'])
    observations['inspector_right'] = vote(client['width'] - 3, palette['inspector'])
    for name, visible in (('navigation_left', nav > 0), ('inspector_right', inspector > 0)):
        require(observations[name] >= .75 if visible else observations[name] <= .10,
                'Actual native panel visibility differs from production responsive Layout: ' + name)
    if nav:
        observations['navigation_right'] = vote(rail + nav - 3, palette['navigation'])
        require(observations['navigation_right'] >= .75, 'Navigation right boundary incorrect')
    if inspector:
        observations['inspector_left'] = vote(client['width'] - inspector + 3, palette['inspector'])
        require(observations['inspector_left'] >= .75, 'Inspector left boundary incorrect')
    return {'passed': True, 'raw_ppm_sha256': hashlib.sha256(ppm).hexdigest(), 'edge_votes': observations}


def run_sequence(smoke, app_pid, probe, record_shell, *, recovery=False):
    """Real compositor/input path, independent of clipboard and Ready OCR gates."""
    import time
    def observe(label, scale):
        values = {}
        for kind in ('outputs', 'tree'):
            raw = smoke.run(['swaymsg', '-t', 'get_' + kind, '-r'], capture_output=True, text=True).stdout
            (smoke.out / (label + '-' + kind + '.json')).write_text(raw)
            values[kind] = json.loads(raw)
        return owned_geometry(values['outputs'], values['tree'], app_pid, scale)
    # Installed font identity is recorded before any grid is read. Exact named
    # family and production shaper must both resolve; missing fonts fail closed.
    raw_font = smoke.run(['fc-match', '-f', '%{family}\n%{file}\n', 'DejaVu Sans Mono'],
                         capture_output=True, text=True).stdout
    font_lines = raw_font.splitlines()
    require(len(font_lines) == 2 and 'DejaVu Sans Mono' in font_lines[0].split(','),
            'Explicit native-test monospace font is unavailable')
    font_file = Path(font_lines[1]).resolve(strict=True)
    (smoke.out / 'independent-font.json').write_text(json.dumps({
        'family': font_lines[0], 'file': str(font_file),
        'sha256': hashlib.sha256(font_file.read_bytes()).hexdigest(),
        'metrics_source': 'verbatim production cosmic-text measurement, no PTY input'}, indent=2))
    records = []
    for index, scale in enumerate(SCALES):
        label = 'dpi-' + str(index)
        replies = json.loads(smoke.run(['swaymsg', '-r', 'output', 'HEADLESS-1', 'scale', str(scale)],
                                      capture_output=True, text=True).stdout)
        require(replies and all(reply.get('success') is True for reply in replies), 'Compositor rejected DPI change')
        time.sleep(1)
        before = observe(label + '-before', scale)
        client = before['client']
        production = expected(probe, client['width'], client['height'], recovery=recovery, environ=smoke.env)
        # Expected geometry is computed BEFORE actual stty, never calibrated by it.
        shell = record_shell(label)
        require([shell['cols'], shell['rows']] == production['grid'],
                'Actual PTY differs from production viewport/font grid at scale ' + str(scale))
        smoke.screenshot(label)
        after = observe(label + '-after', scale)
        require(before == after, 'Native layout changed during grid/frame observation')
        png = smoke.out / (label + '.png')
        ppm = smoke.run(['convert', str(png), '-strip', '-depth', '8', 'ppm:-'], capture_output=True).stdout
        pixels = verify_shell_pixels(ppm, before, production)
        record = {'scale': scale, 'native': before, 'production': production, 'shell': shell,
                  'native_shell_pixels_verified': pixels['passed'], 'pixel_evidence': pixels,
                  'raw_png': png.name, 'raw_png_sha256': hashlib.sha256(png.read_bytes()).hexdigest()}
        records.append(record)
        (smoke.out / (label + '-geometry.json')).write_text(json.dumps(record, indent=2))
    assert_sequence(records)
    return records


def assert_sequence(records):
    require(len(records) == len(SCALES), 'Incomplete DPI sequence')
    require(tuple(record['scale'] for record in records) == SCALES, 'Wrong requested DPI sequence')
    identity = (records[0]['shell']['pid'], records[0]['shell']['tty'])
    require(type(identity[0]) is int and identity[0] > 1 and
            re.fullmatch(r'/dev/pts/[0-9]+', identity[1]), 'Invalid real shell PID/TTY')
    for record in records:
        shell, grid = record['shell'], record['production']['grid']
        require((shell['pid'], shell['tty']) == identity, 'Scaling replaced the original shell/TTY')
        require([shell['cols'], shell['rows']] == grid, 'Actual PTY differs from exact production viewport/font grid')
        require(all(type(value) is int and value > 0 for value in grid), 'Nonpositive production grid')
        require(record['production']['metrics'] == records[0]['production']['metrics'],
                'DPI change also changed independently measured logical font metrics')
        require(record.get('native_shell_pixels_verified') is True, 'Missing independent native panel geometry evidence')
    for field in ('client', 'node_id', 'pid'):
        require(records[0]['native'][field] == records[-1]['native'][field], 'DPI reset did not restore actual owned window geometry')
    require(records[0]['production'] == records[-1]['production'], 'DPI reset did not restore production layout/font geometry')
