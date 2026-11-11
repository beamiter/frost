#!/usr/bin/env python3
"""Real original-production mutants; stop at first exact full-state difference."""
import os,sys,time,subprocess,json,hashlib,gzip,signal,io,difflib,ctypes
# Become subreaper only for this isolated runner, so killed worker descendants
# are waitable here rather than leaving cleanup to an external init.
assert ctypes.CDLL(None,use_errno=True).prctl(36,1,0,0,0)==0
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2];E=Path(__file__).resolve().parent;R=ROOT/'recovery/resumed-family/frost-title-capture'
BIN=ROOT/'recovery/frost-color-target/debug/deps/frost-9eba07696053a4d1'
REG='#[cfg(test)]\nmod title_oracles;\n'
original=(E/'original-src-terminal.rs').read_text()
registered=original.replace('#[cfg(test)]\nmod parser_oracle_tests;\n','#[cfg(test)]\nmod parser_oracle_tests;\n'+REG)
observer=(E/'observer.original.rs').read_text();basehash=hashlib.sha256(original.encode()).hexdigest()
mutations=[
 ('limit200','const MAX_TERMINAL_TITLE_CHARS: usize = 256;','const MAX_TERMINAL_TITLE_CHARS: usize = 200;'),
 ('accept_replacement',"if title.contains('\\u{fffd}') {", "if false && title.contains('\\u{fffd}') {"),
 ('wrong_outbound','Self::sanitized_title(&self.icon_title)','self.icon_title.clone()'),
 ('peek_not_pop','self.title_stack.pop() else','self.title_stack.last().cloned() else'),
 ('ris_clears','    fn full_reset(&mut self) {','    fn full_reset(&mut self) {\n        self.window_title.clear(); self.icon_title.clear(); self.title_stack.clear();'),
 ('raw_id_alias','        self.process_input(input);\n    }','        self.process_input(input);\n        let id=self.grid.row_ids()[0]; self.grid.row_ids_mut()[1]=id;\n    }'),
 ('raw_id_shift','        self.process_input(input);\n    }','        self.process_input(input);\n        let id=self.grid.row_ids()[1].get().unwrap(); self.grid.row_ids_mut()[1]=RawRowId::new(id.saturating_add(1000000));\n    }'),
 ('duration8','metadata_duration_ms = raw.trim().parse::<u64>().ok();','metadata_duration_ms = raw.trim().parse::<u64>().ok().map(|n| n + 1);'),
]
class Follow(io.RawIOBase):
 def __init__(self,path,proc):self.f=open(path,'rb');self.proc=proc;self.started=time.monotonic()
 def readable(self):return True
 def read(self,n=-1):
  assert n>=0
  if n==0:return b''
  while True:
   b=self.f.read(n)
   if b or self.proc.poll() is not None:return b
   if time.monotonic()-self.started>1800:raise TimeoutError('control deadline')
   time.sleep(.02)
 def close(self):self.f.close();super().close()
def compare_until_difference(out,p,d):
 start=time.monotonic()
 while not out.exists():
  if p.poll() is not None:raise RuntimeError('worker failed before creating stream')
  assert time.monotonic()-start<60;time.sleep(.02)
 count=0
 with Follow(out,p) as growing,gzip.GzipFile(fileobj=growing) as actual,gzip.open(E/'original1/actual.jsonl.gz','rb') as expected:
  while True:
   a=actual.readline(4*1024*1024+1);b=expected.readline(4*1024*1024+1)
   assert len(a)<=4*1024*1024 and len(b)<=4*1024*1024
   if not a:raise AssertionError('mutant ended without a full-state difference')
   if a!=b:
    av=json.loads(a);bv=json.loads(b)
    lanes=[k for k in av[1] if av[1].get(k)!=bv[1].get(k)]
    with gzip.GzipFile(filename=str(d/'counterexample.json.gz'),mode='wb',mtime=0) as f:f.write(json.dumps({'actual':av,'expected':bv},separators=(',',':')).encode())
    return {'caught':True,'matched_records_before':count,'actual_record':av[0],'expected_record':bv[0],'different_lanes':lanes,'actual_record_sha256':hashlib.sha256(a).hexdigest(),'expected_record_sha256':hashlib.sha256(b).hexdigest()}
   count+=1
results=json.loads((E/'controls-results.json').read_text()) if (E/'controls-results.json').exists() else []
try:
 for name,old,new in mutations:
  if len(sys.argv)>1 and name not in sys.argv[1:]:continue
  d=E/('control-'+name);d.mkdir(exist_ok=False)
  assert original.count(old)==1,(name,original.count(old))
  mutated=original.replace(old,new);sha=hashlib.sha256(mutated.encode()).hexdigest()
  src=mutated.replace('#[cfg(test)]\nmod parser_oracle_tests;\n','#[cfg(test)]\nmod parser_oracle_tests;\n'+REG)
  ob=observer.replace(basehash,sha)
  # Keep the snapshot code identical: permit the deliberately wrong explicit
  # duration only in seed admissibility, so its unnormalized value reaches oracle.
  if name=='duration8':ob=ob.replace('z.duration_ms == Some(7)','z.duration_ms == Some(8)')
  (R/'src/terminal.rs').write_text(src);(R/'src/terminal/title_oracles.rs').write_text(ob)
  (d/'production.diff').write_text(''.join(difflib.unified_diff(original.splitlines(True),mutated.splitlines(True),fromfile='original-production',tofile=name)))
  (d/'observer-control.diff').write_text(''.join(difflib.unified_diff(observer.splitlines(True),ob.splitlines(True),fromfile='locked-observer',tofile='control-guard')))
  with (d/'build.log').open('wb') as log:
   cp=subprocess.run(['bash','-lc',f'source "{ROOT}/recovery/toolchain/env.sh"; CARGO_TARGET_DIR="{ROOT}/recovery/frost-color-target" CARGO_BUILD_JOBS=1 cargo test --locked --no-run'],cwd=R,stdout=log,stderr=subprocess.STDOUT)
  assert cp.returncode==0,(name,'compile failed')
  env=os.environ.copy();env['FROST_TITLE_RUN_DIR']=str(d);env['FROST_TITLE_INPUT_CORPUS']=str(E/'corpus.json')
  with (d/'driver.log').open('wb') as log:
   p=subprocess.Popen([str(BIN),'--ignored','--exact','terminal::title_oracles::capture_driver','--nocapture','--test-threads=1'],env=env,stdout=log,stderr=subprocess.STDOUT,start_new_session=True)
   try:result=compare_until_difference(d/'actual.jsonl.gz',p,d)
   finally:
    if p.poll() is None:os.killpg(p.pid,signal.SIGTERM)
    p.wait()
    # All members of this fresh process group were terminated; reap adopted
    # grandchildren after the direct driver is reaped (Linux subreaper above).
    while True:
     try:os.waitpid(-p.pid,0)
     except ChildProcessError:break
  partial=d/'actual.jsonl.gz'
  result.update(name=name,production_sha256=sha,worker_exit=p.returncode,stream_intentionally_stopped_at_first_difference=True,partial_stream_bytes=partial.stat().st_size,counterexample_sha256=hashlib.sha256((d/'counterexample.json.gz').read_bytes()).hexdigest())
  # This deliberately incomplete generated stream is not a baseline; retain the
  # complete counterexample pair+source diffs, not redundant partial gigabytes.
  partial.unlink();result['partial_stream_retained']=False
  (d/'result.json').write_text(json.dumps(result,indent=2)+'\n');results.append(result)
  (E/'controls-results.json').write_text(json.dumps(results,indent=2)+'\n');print(json.dumps(result),flush=True)
  # Restore both production and observer between independent mutations.
  (R/'src/terminal.rs').write_text(registered);(R/'src/terminal/title_oracles.rs').write_text(observer)
finally:
 (R/'src/terminal.rs').write_text(registered);(R/'src/terminal/title_oracles.rs').write_text(observer)
