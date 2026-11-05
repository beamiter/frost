#!/usr/bin/env python3
"""Display-free exact-DPI oracle controls using the actual production Rust probe.

Set FROST_SCALE_PROBE to an already built, source-manifested observer. This
does not certify any native UI, clipboard, compositor, or app binary.
"""
import copy
import os
from pathlib import Path
import unittest
import native_scale_geometry as g


class ExactGeometry(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.probe = Path(os.environ['FROST_SCALE_PROBE']).resolve(strict=True)

    def sequence(self):
        # Independent synthetic native client observations; expected grid is
        # computed by current source before generating any fake PTY record.
        clients = [(1276, 873), (1020, 698), (849, 573), (636, 423), (1276, 873)]
        records = []
        for scale, (width, height) in zip(g.SCALES, clients):
            prod = g.expected(self.probe, width, height)
            records.append({'scale': scale, 'production': prod,
                            'native_shell_pixels_verified': True,
                            'native': {'client': {'x': 2, 'y': 25, 'width': width, 'height': height},
                                       'node_id': 5, 'pid': 100},
                            'shell': {'pid': 101, 'tty': '/dev/pts/99',
                                      'cols': prod['grid'][0], 'rows': prod['grid'][1]}})
        return records

    def test_actual_production_panels_collapse_and_measurement(self):
        records = self.sequence()
        self.assertEqual([(r['production']['navigation_width'], r['production']['inspector_width'])
                          for r in records], [(280, 0)] * 5)
        self.assertTrue(all(r['production']['metrics']['mono_advance_exact'] for r in records))
        self.assertLess(records[3]['shell']['cols'], records[2]['shell']['cols'])
        narrow = g.expected(self.probe, 540, 423)
        self.assertEqual(narrow['navigation_width'], 0)
        self.assertGreater(narrow['grid'][0], records[3]['shell']['cols'])
        g.assert_sequence(records)

    def test_wrong_grid_shell_reset_or_missing_pixels_rejected(self):
        mutations = [lambda r: r[2]['shell'].update(cols=r[2]['shell']['cols'] + 1),
                     lambda r: r[2]['shell'].update(rows=r[2]['shell']['rows'] - 1),
                     lambda r: r[2]['shell'].update(pid=102),
                     lambda r: r[2]['shell'].update(tty='/dev/pts/98'),
                     lambda r: r[-1]['native']['client'].update(width=1275),
                     lambda r: r[-1]['native'].update(node_id=6),
                     lambda r: r[-1]['production']['metrics'].update(cell_w=9),
                     lambda r: r[3].update(native_shell_pixels_verified=False)]
        for change in mutations:
            with self.subTest(mutation=change):
                records = self.sequence(); change(records)
                with self.assertRaises(AssertionError): g.assert_sequence(records)
        with self.assertRaises(AssertionError): g.assert_sequence(self.sequence()[:-1])

    def native_fixture(self):
        output = {'active': True, 'name': 'HEADLESS-1', 'scale': 1.25, 'transform': 'normal',
                  'rect': {'x': 0, 'y': 0, 'width': 1024, 'height': 720},
                  'current_mode': {'width': 1280, 'height': 900}}
        node = {'id': 5, 'pid': 100, 'app_id': 'io.github.beamiter.frost', 'focused': True,
                'rect': {'x': 0, 'y': 25, 'width': 1024, 'height': 695},
                'window_rect': {'x': 2, 'y': 0, 'width': 1020, 'height': 693}}
        return [output], {'nodes': [node]}

    def test_actual_owned_geometry_rejects_bad_dpr_identity_and_ambiguity(self):
        outputs, tree = self.native_fixture()
        result = g.owned_geometry(outputs, tree, 100, 1.25)
        self.assertEqual(result['client']['width'], 1020)
        mutations = [lambda o,t: o[0].update(scale=1.5),
                     lambda o,t: o[0]['current_mode'].update(width=1270),
                     lambda o,t: t['nodes'][0].update(pid=101),
                     lambda o,t: t['nodes'][0].update(focused=False),
                     lambda o,t: t['nodes'].append(copy.deepcopy(t['nodes'][0]))]
        for change in mutations:
            o,t=copy.deepcopy((outputs,tree));change(o,t)
            with self.assertRaises(AssertionError): g.owned_geometry(o,t,100,1.25)

    def test_native_pixels_reject_fabricated_collapse_and_missing_panel(self):
        # Synthetic unit pixels only; native runs must capture the real PNG.
        width,height=849,573
        prod=g.expected(self.probe,width,height)
        native={'physical':[width,height], 'scale':1, 'client':{'x':0,'y':0,'width':width,'height':height},
                'output_rect':{'x':0,'y':0,'width':width,'height':height}}
        def ppm(nav,inspector):
            colors={name: [round(c * rgba[3] / 255 + 63 * (1 - rgba[3] / 255))
                          for c in rgba[:3]] for name, rgba in prod['palette'].items()}; data=bytearray()
            row=b''.join(bytes(colors['rail'] if x<prod['rail_width'] else colors['navigation'] if x<prod['rail_width']+nav
                               else colors['inspector'] if x>=width-inspector else [250,251,255])
                         for x in range(width))
            for _ in range(height):data+=row
            return f'P6\n{width} {height}\n255\n'.encode()+data
        g.verify_shell_pixels(ppm(280,0),native,prod,[63,63,63])
        with self.assertRaises(AssertionError):
            g.verify_shell_pixels(ppm(280,0),native,prod,[0,0,0])
        for pixels in [ppm(0,0),ppm(280,288),ppm(100,0)]:
            with self.assertRaises(AssertionError):g.verify_shell_pixels(pixels,native,prod,[63,63,63])


if __name__ == '__main__': unittest.main()
