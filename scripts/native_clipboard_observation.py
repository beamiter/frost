"""Read-only exact OCR observations across the widget's cursor blink cycle."""
import csv
import io
import json
import re
import time


def locate_find_input(ppm):
    """Locate the actual fixed-width Find input border without reading text.

    The production Find input is 220 logical pixels wide. This desktop fixture
    runs at scale 1. Border pixels locate that control even when the status text
    moves the entire panel. Missing or ambiguous geometry never guesses a crop.
    """
    header = re.match(br'P6\s+(\d+)\s+(\d+)\s+255\s', ppm)
    if not header:
        raise AssertionError('Expected an eight-bit native PPM frame')
    width, height = map(int, header.groups())
    pixels = ppm[header.end():]
    if width <= 0 or height <= 0 or len(pixels) != width * height * 3:
        raise AssertionError('Invalid native frame extent or pixel bytes')

    def border(x, y):
        offset = (y * width + x) * 3
        red, green, blue = pixels[offset:offset + 3]
        neutral = 112 <= red <= 224 and max(red, green, blue) - min(red, green, blue) <= 8
        # Current Iced focused input uses the exact accent-blue border. Keep
        # the independent 220px rectangle and unique-control checks unchanged.
        focused = all(abs(actual - expected) <= 2 for actual, expected in
                      zip((red, green, blue), (68, 126, 255)))
        return neutral or focused

    runs = {}
    for y in range(height):
        start = None
        for x in range(width + 1):
            selected = x < width and border(x, y)
            if selected and start is None:
                start = x
            if not selected and start is not None:
                # Rounded/antialiased corners remove one pixel at either end.
                if x - start in (216, 218):
                    runs.setdefault((start, x - 1), []).append(y)
                start = None
    rectangles = set()
    for (left, right), rows in runs.items():
        for top in rows:
            for bottom in rows:
                if not 22 <= bottom - top + 1 <= 36:
                    continue
                inset = (220 - (right - left + 1)) // 2
                x, end = left - inset, right + inset
                if x < 0 or end >= width:
                    continue
                samples = list(range(top + 4, bottom - 3))
                if all(sum(border(edge, y) for y in samples) >= len(samples) * 0.8
                       for edge in (x, end)):
                    rectangles.add((x, top, end - x + 1, bottom - top + 1))
    if len(rectangles) != 1:
        raise AssertionError({'native_find_input_rectangles': sorted(rectangles)})
    return rectangles.pop()


def capture_find_text(png, run):
    """Use one fixed control-only OCR pipeline, preserving every raw frame."""
    ppm = run(['convert', str(png), '-strip', '-depth', '8', 'ppm:-'],
              capture_output=True).stdout
    metadata = png.with_suffix('.find-ocr.json')
    try:
        x, y, width, height = locate_find_input(ppm)
    except AssertionError as error:
        metadata.write_text(json.dumps({'raw': png.name, 'geometry_error': str(error)}, indent=2))
        return []  # Retry observation, never input; the exact gate still times out.
    processed = png.with_suffix('.find-ocr.png')
    command = ['convert', str(png), '-crop',
               f'{width - 6}x{height - 6}+{x + 3}+{y + 3}', '+repage',
               '-colorspace', 'Gray', '-negate', '-filter', 'Lanczos',
               '-resize', '400%', str(processed)]
    run(command, capture_output=True)
    tsv = run(['tesseract', str(processed), 'stdout', '--psm', '7', 'tsv'],
              text=True, capture_output=True).stdout
    tsv_path = png.with_suffix('.find-ocr.tsv')
    tsv_path.write_text(tsv)
    metadata.write_text(json.dumps({
        'raw': png.name, 'native_input_rect': [x, y, width, height],
        'processed': processed.name, 'tsv': tsv_path.name,
        'processing': command, 'coordinate_space': 'OCR crop only; not pointer coordinates',
    }, indent=2))
    return [row for row in csv.DictReader(io.StringIO(tsv), delimiter='\t',
                                         quoting=csv.QUOTE_NONE) if row['text'].strip()]


def observe_exact_word(capture, label, expected, observations, timeout=5):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        artifact = f'{label}-{len(observations):02d}'
        rows = capture(artifact)
        tokens = [row['text'] for row in rows]
        observations.append({'artifact': artifact, 'words': tokens})
        # Preserve the existing exact, unique-token requirement. In particular,
        # a caret-suffixed word must not be normalized into the expected word.
        if tokens.count(expected) == 1:
            return
    raise AssertionError({'expected_unique_word': expected, 'observations': observations})


def live_prompt_target(rows, width=1280, height=900):
    """Find the fixture's actual lower input prompt, not a transcript row."""
    prompts = [row for row in rows if row['text'] == 'FROST_SMOKE>'
               and float(row['conf']) >= 80 and int(row['top']) > height // 2]
    if len(prompts) != 1:
        raise AssertionError({'unique_live_prompt_required': prompts})
    row = prompts[0]
    x = int(row['left']) + int(row['width']) + 24
    y = int(row['top']) + int(row['height']) // 2
    if not (0 <= x < width and height // 2 < y < height):
        raise AssertionError('Live prompt escaped the native frame')
    return x, y
