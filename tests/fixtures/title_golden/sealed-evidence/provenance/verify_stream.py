#!/usr/bin/env python3
"""Bounded exact full-value JSONL verification; hashes are transport evidence only."""
import argparse,gzip,hashlib,json
from pathlib import Path
p=argparse.ArgumentParser();p.add_argument('actual');p.add_argument('--corpus',required=True);p.add_argument('--reference');p.add_argument('--max-record',type=int,default=4*1024*1024);p.add_argument('--max-total',type=int,default=16*1024**3);p.add_argument('--report',required=True);a=p.parse_args()
c=json.loads(Path(a.corpus).read_bytes());M=(1<<64)-1
def ends(n,s,bulk=False):
 if s==0:return [n]
 if s==1:return list(range(17 if bulk else 1,n,17 if bulk else 1))+[n]
 if s>5:return [s-5,n]
 x=s-1;at=0;out=[]
 while at<n:
  x^=(x<<13)&M;x^=x>>7;x^=(x<<17)&M
  at=min(n,at+1+x%(64 if bulk else 23));out.append(at)
 return out
def ids():
 for seed in range(4):yield f'seed_{seed}'
 for case in c['cases']:
  n=len(bytes.fromhex(case['bytes_hex']));policy=case['partition_policy']
  for seed in range(4):
   for schedule in range(6+(n-1 if policy=='all_two_way' else 0)):
    for feed,_ in enumerate(ends(n,schedule,policy=='bulk')):yield f'{case["id"]}_seed_{seed}_schedule_{schedule}_feed_{feed}'
def compressed_sha(path):
 h=hashlib.sha256()
 with open(path,'rb') as f:
  for b in iter(lambda:f.read(1024*1024),b''):h.update(b)
 return h.hexdigest()
def read(f):
 b=f.readline(a.max_record+1)
 assert len(b)<=a.max_record,'record exceeds fixed limit'
 assert not b or b.endswith(b'\n'),'unterminated record'
 return b
count=total=maximum=0;digest=hashlib.sha256();keys=None
ref=gzip.open(a.reference,'rb') if a.reference else None
try:
 with gzip.open(a.actual,'rb') as f:
  for expected_id in ids():
   b=read(f);assert b,f'missing {expected_id}'
   total+=len(b);assert total<=a.max_total,'decompressed total exceeds bound'
   maximum=max(maximum,len(b));digest.update(b)
   actual=json.loads(b);assert isinstance(actual,list) and len(actual)==2
   assert actual[0]==expected_id,(count,actual[0],expected_id)
   assert isinstance(actual[1],dict)
   if keys is None:keys=list(actual[1])
   assert list(actual[1])==keys,(expected_id,'lane keys changed')
   if ref:
    other=read(ref)
    if b!=other:
     expected=json.loads(other)
     lanes=[k for k in actual[1] if actual[1].get(k)!=expected[1].get(k)]
     raise AssertionError((expected_id,'full-value mismatch',lanes[:10]))
   count+=1
  assert not read(f),'extra records'
  if ref:assert not read(ref),'extra reference records'
finally:
 if ref:ref.close()
assert count==c['expected_record_count'],(count,c['expected_record_count'])
report=dict(records=count,lanes=len(keys or []),canonical_bytes=total,max_record_bytes=maximum,canonical_sha256=digest.hexdigest(),gzip_bytes=Path(a.actual).stat().st_size,gzip_sha256=compressed_sha(a.actual),corpus_sha256=hashlib.sha256(Path(a.corpus).read_bytes()).hexdigest(),exact_reference_equal=bool(a.reference),max_record_bound=a.max_record,max_total_bound=a.max_total)
Path(a.report).write_text(json.dumps(report,indent=2)+'\n');print(json.dumps(report))
