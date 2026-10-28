#!/usr/bin/env python3
"""Cursor-blink observations never repair, inject, or accept incorrect text."""
import unittest
from unittest.mock import patch

from native_clipboard_observation import locate_find_input, observe_exact_word


class ExactObservationTests(unittest.TestCase):
    @staticmethod
    def frame(rectangles):
        width, height = 520, 90
        pixels = bytearray([9, 12, 23] * (width * height))

        def paint(x, y):
            offset = (y * width + x) * 3
            pixels[offset:offset + 3] = bytes([168] * 3)

        for x, y, w, h in rectangles:
            for edge in (y, y + h - 1):
                for column in range(x + 1, x + w - 1):
                    paint(column, edge)
            for edge in (x, x + w - 1):
                for row in range(y + 1, y + h - 1):
                    paint(edge, row)
        return f'P6\n{width} {height}\n255\n'.encode() + pixels

    def test_control_geometry_follows_the_panel_instead_of_expected_text(self):
        for rectangle in ((10, 10, 220, 28), (280, 40, 220, 28)):
            self.assertEqual(locate_find_input(self.frame([rectangle])), rectangle)

    def test_missing_ambiguous_wrong_width_and_truncated_frames_are_rejected(self):
        for frame in (self.frame([]), self.frame([(10, 10, 220, 28), (280, 40, 220, 28)]),
                      self.frame([(10, 10, 207, 28)]), self.frame([(10, 10, 220, 28)])[:-1],
                      b'P5\n1 1\n255\n\x00'):
            with self.subTest(length=len(frame)):
                self.assertRaises(AssertionError, locate_find_input, frame)

    def test_blink_frame_is_retained_before_exact_frame(self):
        artifacts, observations = [], []
        frames = iter([['FROSTWIDGETI'], ['FROSTWIDGET']])

        def capture(label):
            artifacts.append(label)
            return [{'text': text} for text in next(frames)]

        with patch('native_clipboard_observation.time.monotonic', side_effect=[0, 0, 1]):
            observe_exact_word(capture, 'pasted', 'FROSTWIDGET', observations)
        self.assertEqual(artifacts, ['pasted-00', 'pasted-01'])
        self.assertEqual([item['words'] for item in observations],
                         [['FROSTWIDGETI'], ['FROSTWIDGET']])

    def test_missing_wrong_partial_and_duplicate_tokens_time_out(self):
        for tokens in ([], ['WRONG'], ['FROSTWIDGXT'], ['FROSTWIDGETI'], ['FROST', 'WIDGET'],
                       ['FROSTWIDGET', 'FROSTWIDGET']):
            with self.subTest(tokens=tokens):
                observations = []
                with patch('native_clipboard_observation.time.monotonic', side_effect=[0, 0, 6]):
                    with self.assertRaises(AssertionError):
                        observe_exact_word(lambda label: [{'text': word} for word in tokens],
                                           'pasted', 'FROSTWIDGET', observations)
                self.assertEqual(observations, [{'artifact': 'pasted-00', 'words': tokens}])


if __name__ == '__main__':
    unittest.main()
