#!/usr/bin/env python3
"""Exact observations never repair, inject, or accept incorrect text."""
import ast
from pathlib import Path
import unittest
from unittest.mock import patch

from native_clipboard_observation import live_prompt_target, locate_find_input, observe_exact_word


class ExactObservationTests(unittest.TestCase):
    @staticmethod
    def frame(rectangles, color=(168, 168, 168), inset=1):
        width, height = 520, 90
        pixels = bytearray([9, 12, 23] * (width * height))

        def paint(x, y):
            offset = (y * width + x) * 3
            pixels[offset:offset + 3] = bytes(color)

        for x, y, w, h in rectangles:
            for edge in (y, y + h - 1):
                for column in range(x + inset, x + w - inset):
                    paint(column, edge)
            for edge in (x, x + w - 1):
                for row in range(y + 1, y + h - 1):
                    paint(edge, row)
        return f'P6\n{width} {height}\n255\n'.encode() + pixels

    def test_control_geometry_follows_the_panel_instead_of_expected_text(self):
        for rectangle in ((10, 10, 220, 28), (280, 40, 220, 28)):
            self.assertEqual(locate_find_input(self.frame([rectangle])), rectangle)

    def test_focused_blue_border_preserves_exact_geometry(self):
        rectangle = (10, 10, 220, 28)
        for inset in (1, 2):
            self.assertEqual(locate_find_input(self.frame([rectangle], (68, 126, 255), inset)), rectangle)
        self.assertRaises(AssertionError, locate_find_input, self.frame([rectangle], (30, 80, 220)))

    def test_missing_ambiguous_wrong_width_and_truncated_frames_are_rejected(self):
        for frame in (self.frame([]), self.frame([(10, 10, 220, 28), (280, 40, 220, 28)]),
                      self.frame([(10, 10, 207, 28)]), self.frame([(10, 10, 220, 28)])[:-1],
                      b'P5\n1 1\n255\n\x00'):
            with self.subTest(length=len(frame)):
                self.assertRaises(AssertionError, locate_find_input, frame)

    def test_primary_target_requires_unique_visible_live_prompt(self):
        row = dict(text='FROST_SMOKE>', conf='91', top='782', left='371', width='102', height='13')
        self.assertEqual(live_prompt_target([row]), (497, 788))
        for rows in ([], [row, row], [dict(row, top='120')], [dict(row, conf='20')],
                     [dict(row, text='FROST_SMOKES')]):
            self.assertRaises(AssertionError, live_prompt_target, rows)

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

    def test_joined_and_separate_extra_characters_time_out(self):
        for tokens in (['FROSTWIDGET|'], ['|FROSTWIDGET'], ['FROSTWIDGETX'],
                       ['XFROSTWIDGET'], ['FROSTWIDGET', '|'], ['|', 'FROSTWIDGET'],
                       ['FROSTWIDGET', 'EXTRA'], ['EXTRA', 'FROSTWIDGET']):
            with self.subTest(tokens=tokens):
                observations = []
                with patch('native_clipboard_observation.time.monotonic', side_effect=[0, 0, 6]):
                    with self.assertRaises(AssertionError):
                        observe_exact_word(lambda label: [{'text': word} for word in tokens],
                                           'pasted', 'FROSTWIDGET', observations)
                self.assertEqual(observations, [{'artifact': 'pasted-00', 'words': tokens}])


class WidgetObservationSequenceTests(unittest.TestCase):
    @staticmethod
    def calls():
        # Parse without importing the native runner or starting a compositor.
        source = Path(__file__).with_name('native-clipboard-ui.py').read_text()
        return sorted((node for node in ast.walk(ast.parse(source))
                       if isinstance(node, ast.Call)), key=lambda node: node.lineno)

    @staticmethod
    def named(call, name):
        return isinstance(call.func, ast.Name) and call.func.id == name

    @staticmethod
    def key(call, key):
        return (isinstance(call.func, ast.Attribute) and call.func.attr == 'key'
                and isinstance(call.func.value, ast.Name) and call.func.value.id == 's'
                and [ast.literal_eval(arg) for arg in call.args] == [key, ('ctrl',)])

    def widget_sequence(self):
        calls = self.calls()
        paste = next(call for call in calls if self.key(call, 'v'))
        observe = next(call for call in calls if self.named(call, 'observe_exact_word'))
        stop = next(call for call in calls if self.named(call, 'stop') and call.lineno > paste.lineno)
        copy = next(call for call in calls if self.key(call, 'c') and call.lineno > paste.lineno)
        external = next(call for call in calls if self.named(call, 'external') and call.lineno > paste.lineno)
        return calls, paste, observe, stop, copy, external

    def test_select_once_before_observation_without_repair_or_paste_replay(self):
        calls, paste, observe, _, copy, external = self.widget_sequence()
        inputs = [call for call in calls if paste.lineno < call.lineno < external.lineno
                  and isinstance(call.func, ast.Attribute)
                  and isinstance(call.func.value, ast.Name) and call.func.value.id == 's'
                  and call.func.attr in ('key', 'text')]
        self.assertEqual(len(inputs), 2)
        select, observed_copy = inputs
        self.assertTrue(self.key(select, 'a'))
        self.assertIs(observed_copy, copy)
        self.assertLess(select.lineno, observe.lineno)
        self.assertLess(observe.lineno, copy.lineno)

    def test_source_stops_before_copy_and_exact_external_read(self):
        _, _, observe, stop, copy, external = self.widget_sequence()
        self.assertLess(observe.lineno, stop.lineno)
        self.assertLess(stop.lineno, copy.lineno)
        self.assertLess(copy.lineno, external.lineno)
        self.assertEqual([arg.id for arg in stop.args], ['proc', 'control'])
        self.assertEqual([ast.literal_eval(arg) for arg in external.args], [b'FROSTWIDGET'])


if __name__ == '__main__':
    unittest.main()
