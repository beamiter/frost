"""Compile the actual ForceClose arm with isolated state fixtures, not native input."""
import hashlib
from pathlib import Path
import subprocess
import tempfile

source = Path(__file__).resolve().parents[1] / 'src/eva_companion.rs'
text = source.read_text()
start = text.index('            Event::ForceClose(revision) => {')
start = text.index('{', start) + 1
end, depth = start, 1
while depth:
    depth += (text[end] == '{') - (text[end] == '}')
    end += 1
body = text[start:end - 1]
code = '''use std::time::Instant;
struct Gate { closing: bool, close_paused_for_work: bool, close_ui_revision: u64, force_close: bool }
impl Gate {
 fn new() -> Self { Self { closing:true, close_paused_for_work:false, close_ui_revision:1, force_close:false } }
 fn apply(&mut self, revision:u64) {
''' + body + '''}}
#[test] fn current_callback_is_accepted() { let mut g=Gate::new();g.apply(1);assert!(g.force_close); }
#[test] fn replaced_revision_is_rejected() { let mut g=Gate::new();g.close_ui_revision=3;g.apply(1);assert!(!g.force_close); }
#[test] fn paused_current_revision_is_rejected() { let mut g=Gate::new();g.close_paused_for_work=true;g.close_ui_revision=2;g.apply(2);assert!(!g.force_close); }
#[test] fn cancelled_close_is_rejected() { let mut g=Gate::new();g.closing=false;g.close_ui_revision=2;g.apply(1);assert!(!g.force_close); }
'''
print('ForceClose arm sha256:', hashlib.sha256(body.encode()).hexdigest(), flush=True)
with tempfile.TemporaryDirectory(prefix='frost-close-gate-') as temp:
    root = Path(temp)
    (root / 'gate.rs').write_text(code)
    subprocess.run(['rustc', '--test', str(root / 'gate.rs'), '-o', str(root / 'gate')], check=True)
    subprocess.run([str(root / 'gate'), '--nocapture'], check=True)
