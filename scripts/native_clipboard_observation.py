"""Read-only exact OCR observations across the widget's cursor blink cycle."""
import time


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
