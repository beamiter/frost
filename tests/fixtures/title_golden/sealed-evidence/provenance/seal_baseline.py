#!/usr/bin/env python3
"""Seal only after restored full original replay; never generates state."""
from pathlib import Path
import json,hashlib,subprocess,shutil
E=Path(__file__).resolve().parent;ROOT=E.parents[1];R=ROOT/'recovery/resumed-family/frost-title-capture';T=ROOT/'recovery/frost-title-transport-eval/full'
def digest(p):return hashlib.sha256(p.read_bytes()).hexdigest()
reports=[json.loads((E/d/'verification.json').read_text()) for d in ['original1','original2','restored3_retry1']]
for k in ['records','lanes','canonical_bytes','canonical_sha256','max_record_bytes','gzip_bytes','gzip_sha256','corpus_sha256']:
 assert len({r[k] for r in reports})==1,k
assert all(r['exact_reference_equal'] for r in reports[1:])
controls=json.loads((E/'controls-results.json').read_text());assert {c['name'] for c in controls}=={'limit200','accept_replacement','wrong_outbound','peek_not_pop','ris_clears','raw_id_alias','raw_id_shift','duration8'}
assert len(controls)==8 and all(c['caught'] and c['different_lanes'] for c in controls)
pre=json.loads((E/'pre-capture-lock.json').read_text())
for f,sha in pre['files'].items():assert digest(E/f)==sha,f
lock=json.loads((E/'source-lock.json').read_text());reg='#[cfg(test)]\nmod title_oracles;\n'
assert hashlib.sha256((R/'src/terminal.rs').read_text().replace(reg,'').encode()).hexdigest()==lock['files']['src/terminal.rs']
assert hashlib.sha256((R/'src/kitty_graphics.rs').read_text().split('// TITLE_ORACLE_TEST_HELPER_BEGIN')[0].encode()).hexdigest()==lock['files']['src/kitty_graphics.rs']
for f in ['Cargo.toml','Cargo.lock']:assert digest(R/f)==lock['files'][f]
assert (R/'src/terminal/title_oracles.rs').read_bytes()==(E/'observer.original.rs').read_bytes()
assert subprocess.check_output(['git','rev-parse','HEAD'],cwd=R,text=True).strip()==lock['published_sha']
assert subprocess.check_output(['git','diff','--name-only'],cwd=R,text=True).splitlines()==['src/kitty_graphics.rs','src/terminal.rs']
conversion=json.loads((T/'conversion.json').read_text());audit=json.loads((T/'independent-audit.json').read_text())
assert conversion['deterministic'] and audit['byte_equal_original_gzip'] and audit['exact_ids_order']
v=reports[0];z=T/'original1.level9.w23.run1.zst';zsha=digest(z)
assert all(r['sha256']==zsha and r['canonical_sha256']==v['canonical_sha256'] for r in conversion['results'])
assert audit['canonical_sha256']==v['canonical_sha256']
manifest=json.loads((E/'manifest.draft.json').read_text());manifest['status']='sealed_original_production_after_8_mutants_and_restored_full_replay'
manifest['transport']={'format':'zstd-single-frame','file':'frost-title-original.jsonl.zst','bytes':z.stat().st_size,'sha256':zsha,'window_log_max':23,'checksum':True,'content_size':v['canonical_bytes'],'compression_parameters':conversion['parameters']}
manifest['original_runs']=3;manifest['interrupted_runs']=1;manifest['restored_run_directory']='restored3_retry1';manifest['negative_controls']=8;manifest['controls_sha256']=digest(E/'controls-results.json')
out=E/'sealed';out.mkdir(exist_ok=False)
shutil.copyfile(z,out/'frost-title-original.jsonl.zst');(out/'manifest.json').write_text(json.dumps(manifest,indent=2)+'\n')
files=['source-lock.json','core-source-lock.json','pre-capture-lock.json','source-restored.json','observer.original.rs','kitty-helper.original.rs','registration.diff','corpus.json','controls-results.json','run_controls.py','verify_stream.py','seed-preflight.log','build-locked.log','build-restored.log','seal_baseline.py']
files += [p.name for p in E.glob('original-*') if p.is_file()]
files += [p.name for p in E.glob('core-original-*') if p.is_file()]
for d in ['small1','small2','original1','original2','restored3','restored3_retry1']:
 for n in ['driver.log','worker.stdout','worker.stderr','worker-list.txt','worker-list.stderr','verification.json','interruption.json']:
  if (E/d/n).exists():files.append(f'{d}/{n}')
for c in controls:
 d='control-'+c['name']
 for n in ['production.diff','observer-control.diff','counterexample.json.gz','counterexample-records.jsonl.gz','result.json','driver.log','worker.stdout','worker.stderr','build.log']:
  if (E/d/n).exists():files.append(f'{d}/{n}')
for f in files:
 dst=out/'provenance'/f;dst.parent.mkdir(parents=True,exist_ok=True);shutil.copyfile(E/f,dst)
for n in ['conversion.json','independent-audit.json','convert.py','audit.py']:
 shutil.copyfile(T/n,out/'provenance'/('transport-'+n))
readme='''# Sealed original Frost title baseline

Actual source: ada5ae0cb2ff14b5b1177128aec3595c534f3a27, existing core598f158.
No candidate engine/consumer took part. Only recorded cfg(test) observation was
registered; Cargo/lock and all production bodies were unchanged for original runs.

An initial restored3 attempt was interrupted by an executor key change and its
partial output is not evidence of completion. A new unique restored3_retry1
provided the restored run. Interruption metadata is preserved.

Three fresh-process complete outputs are byte-identical: two originals before
mutations and a third after exact source/observer restoration. Each carries
53,992 ordered records and133 full-value lanes (all129 TerminalState fields plus
four retained-frame/alias lanes). Canonical JSONL is3,775,893,628bytes with maximum
record92,682bytes. It is losslessly represented by one3,501,727byte zstd frame,
independently decoded against original gzip and all record IDs/schema. Original
gzip fingerprints remain in reports; redundant multi-hundred-MB transport copies
are intentionally not bundled. No state is replaced by a fingerprint.

Eight real production mutations were caught at first full-state difference:
limit200, replacement-admission bypass, wrong outbound sanitizer, peek vs pop,
RIS clearing titles, RawRowId alias, single ID shift, and explicit duration7→8.
Each has source diff, exact SHA, full counterexample values/raw JSONL pair,
expected/actual record SHA and lane list. The duration control changes only the
seed admissibility assertion7→8 in addition to the source hash guard so the actual
wrong value reaches the unchanged observer. No expected record was changed.
Stopped worker groups were killed and reaped under a dedicated Linux subreaper.
Intentionally incomplete mutant gzip streams are not retained as baselines.

RawRowIds are NEVER rewritten: sole fresh ignored worker assertions establish
allocator1/6, and original output retains live/stale identity and aliases. Time
normalization only maps finished_at_ms presence and Instant presence; explicit
durations and lifecycle metadata are preserved. Two seed Arc views stay alive
through every feed and are fully observed along with current-cache alias booleans.
No snapshot invokes a mutating renderer getter. Seeds use an explicitly recorded
renderer cadence; primaryseed2 asserts an effective collapse while altseed3
asserts identity fallback. Endpoints use production projected-selection mapping.
The Kitty helper keeps complete state and asserts at most one Pending node.

Historical capture wrapper is source-locked evidence, not a publication-ready
replay wrapper. Final integration still needs bounded Rust zstd replay, strengthened
fresh subprocess lifecycle, original full test, candidate full test and review.
The original helper retains its drafting comment as part of immutable provenance.
This seal does NOT claim consumer migration or release completion.
'''
(out/'README.md').write_text(readme)
inv={str(p.relative_to(out)):{'bytes':p.stat().st_size,'sha256':digest(p)} for p in sorted(out.rglob('*')) if p.is_file()}
(out/'artifact-inventory.json').write_text(json.dumps(inv,indent=2)+'\n')
(out/'seal.json').write_text(json.dumps({'status':'sealed','manifest_sha256':digest(out/'manifest.json'),'artifact_inventory_sha256':digest(out/'artifact-inventory.json'),'production_source_exact_restored':True,'observer_exact_restored':True,'full_original_runs':3,'production_controls_caught':8,'candidate_used':False},indent=2)+'\n')
print(json.dumps({'path':str(out),'files':len(inv)+2,'zstd_sha256':zsha,'seal_sha256':digest(out/'seal.json')}))
