"""Pure pixel and chronology witnesses for synthetic X11 scrollback QA.

Only driver-owned absolute row markers are decoded. An HTTP or PTY receipt
is not counted as presentation. Capture requires a separately measured grid.
"""
from dataclasses import dataclass


MARKER_START = (243, 15, 177)
MARKER_END = (18, 242, 132)
NIBBLE_COLORS = tuple(tuple((30 + n * 13, g, 214) for n in range(16))
                      for g in (43, 101, 163))


def marker_colors(row):
    if type(row) is not int or not 1 <= row <= 4095:
        raise ValueError('Synthetic row outside marker range')
    return (MARKER_START, *(bank[(row >> shift) & 15]
                             for bank, shift in zip(NIBBLE_COLORS, (8, 4, 0))),
            MARKER_END)


def marker_ansi(row):
    """Five separate ANSI-color cells outside the selectable payload."""
    return ''.join(f'\x1b[48;2;{r};{g};{b}m ' for r, g, b in marker_colors(row)) + '\x1b[0m'


class InvalidWitness(ValueError):
    """Pixels or driver timestamps cannot uniquely support the assertion."""


def require(condition, reason):
    if not condition:
        raise InvalidWitness(reason)


@dataclass(frozen=True)
class Grid:
    # Marker samples are local to the captured pixel strip; this identity
    # includes the window/geometry and the PTY as measured by the driver.
    target: tuple
    x: tuple[int, ...]
    y: tuple[int, ...]

    def validate(self, img):
        require(len(self.x) == 5 and 4 <= len(self.y) <= 512 and img.mode == 'RGB',
                'Missing bounded RGB grid')
        for points, limit in ((self.x, img.width), (self.y, img.height)):
            require(all(type(p) is int and 0 <= p < limit for p in points),
                    'Grid sample outside the capture')
            require(all(b - a >= 3 for a, b in zip(points, points[1:])),
                    'Grid samples are unordered or cannot fit neighbor probes')


@dataclass(frozen=True)
class Frame:
    grid: Grid
    image: object   # PIL RGB image returned by native_x11_capture
    started: int    # same driver monotonic ns for all Frame/click/stream facts
    finished: int


def _pixel(img, x, y):
    require(1 <= x < img.width - 1 and 1 <= y < img.height - 1,
            'Neighborhood probes outside the capture')
    colors = {img.getpixel((x + dx, y + dy)) for dy in (-1, 0, 1) for dx in (-1, 0, 1)}
    require(len(colors) == 1, 'Ambiguous paint: use cell-interior probes')
    return colors.pop()


def decoded_row_ids(frame):
    """Decode every intact observed row marker, without asserting a sequence."""
    frame.grid.validate(frame.image)
    require(frame.started < frame.finished, 'Invalid capture duration')
    ids = []
    for y in frame.grid.y:
        colors = tuple(_pixel(frame.image, x, y) for x in frame.grid.x)
        require(colors[0] == MARKER_START and colors[-1] == MARKER_END,
                'Missing intact synthetic row marker')
        require(all(color in bank for bank, color in zip(NIBBLE_COLORS, colors[1:-1])),
                'Unrecognized synthetic row marker color')
        a, b, c = [bank.index(color) for bank, color in zip(NIBBLE_COLORS, colors[1:-1])]
        ids.append((a << 8) + (b << 4) + c)
    require(all(row >= 1 for row in ids), 'Invalid synthetic row marker')
    return tuple(ids)


def row_ids(frame):
    ids = decoded_row_ids(frame)
    require(all(b == a + 1 for a, b in zip(ids, ids[1:])),
            'Synthetic rows are not a contiguous ordered span')
    return ids


def verify_page_up(before, after, key_events, *, pty_rows, ownership, fixture_flushes=()):
    """Check one isolated bare PageUp (not resize, scroll wheel, or tab chord).

    Call after freezing the synthetic producer at a barrier. Driver must log
    every sent key and every flush timestamp covering both sampled frames.
    """
    require(ownership == 'frost-scrollback', 'Frost page size applies only to Frost-owned scrollback')
    require(before.grid == after.grid, 'Window or grid identity changed')
    require(type(pty_rows) is int and len(before.grid.y) == pty_rows,
            'Measured PTY rows differ from the captured complete grid')
    require(len(key_events) == 1 and key_events[0][0] == 'Prior',
            'Expected exactly one bare PageUp')
    # The xdotool exit receipt is driver-side injection, not app delivery.
    _, sent, returned = key_events[0]
    require(before.finished < sent <= returned < after.started,
            'Captures do not bracket the one input')
    require(not any(before.started <= f <= after.finished for f in fixture_flushes),
            'An output write made PageUp distance ambiguous')
    older = row_ids(after)
    newer = row_ids(before)
    page = max(1, pty_rows - 2)
    require(all(a - b == page for a, b in zip(newer, older)),
            'The post-input viewport did not move by one complete Frost page')
    return {'page_rows': page, 'before_first': newer[0], 'after_first': older[0]}
