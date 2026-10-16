#!/usr/bin/env python3
"""Synthetic X11 selection owner for latency characterization, not a clipboard reader."""
import argparse
import json
from pathlib import Path
import time
from Xlib import X, Xatom, display
from Xlib.protocol import event

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--delay', type=float, required=True)
parser.add_argument('--ready', type=Path, required=True)
parser.add_argument('--log', type=Path, required=True)
args = parser.parse_args()
if not 0 <= args.delay <= 2.5:
    parser.error('delay must remain within the fixture budget of 0..2.5 seconds')
d = display.Display()
window = d.screen().root.create_window(0, 0, 1, 1, 0, X.CopyFromParent)
clipboard = d.intern_atom('CLIPBOARD')
targets = d.intern_atom('TARGETS')
utf8 = d.intern_atom('UTF8_STRING')
window.set_selection_owner(clipboard, X.CurrentTime)
d.sync()
if d.get_selection_owner(clipboard).id != window.id:
    raise RuntimeError('Synthetic selection ownership was not acquired')
args.ready.write_text(str(window.id))
sequence = 0
with args.log.open('a', buffering=1) as log:
    while True:
        request = d.next_event()
        if request.type != X.SelectionRequest:
            continue
        sequence += 1
        started = time.monotonic()
        log.write(json.dumps({'phase': 'request', 'request_id': sequence, 'text_transfer': request.target in (utf8, Xatom.STRING), 'target_atom': request.target, 'monotonic': started}) + '\n')
        property_atom = request.property or request.target
        accepted = request.selection == clipboard
        if accepted and request.target == targets:
            request.requestor.change_property(property_atom, Xatom.ATOM, 32, [targets, utf8, Xatom.STRING])
        elif accepted and request.target in (utf8, Xatom.STRING):
            time.sleep(args.delay)
            # Empty synthetic paste leaves the command line unchanged.
            request.requestor.change_property(property_atom, request.target, 8, b'')
        else:
            accepted = False
        reply = event.SelectionNotify(time=request.time, requestor=request.requestor,
                                      selection=request.selection, target=request.target,
                                      property=property_atom if accepted else X.NONE)
        request.requestor.send_event(reply, propagate=False)
        d.flush()
        log.write(json.dumps({'phase': 'reply', 'request_id': sequence, 'text_transfer': request.target in (utf8, Xatom.STRING), 'target_atom': request.target, 'accepted': accepted,
                              'started_monotonic': started, 'completed_monotonic': time.monotonic(),
                              'payload_bytes': 0}) + '\n')
