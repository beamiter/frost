import pathlib,json,gzip,hashlib,subprocess,zstandard as z
root=pathlib.Path(__file__).resolve().parent;base=root.parents[1]/'frost-title-baseline'
corpus=json.loads((base/'corpus.json').read_bytes());meta=json.loads((base/'original1/verification.json').read_bytes())
path=root/'original1.level9.w23.run1.zst';frame=z.get_frame_parameters(path.read_bytes()[:64]);assert frame.window_size<=8388608 and frame.has_checksum and frame.content_size==meta['canonical_bytes']
# Independently derive the explicit record schedule from the corpus contract.
def ids():
 for seed in range(corpus['seed_count']):yield f'seed_{seed}'
 for c in corpus['cases']:
  n=len(bytes.fromhex(c['bytes_hex']));bulk=c['partition_policy']=='bulk'
  for seed in range(corpus['seed_count']):
   for schedule in range(6+(n-1 if c['partition_policy']=='all_two_way' else 0)):
    if schedule==0:ends=[n]
    elif schedule==1:ends=[*range(17 if bulk else 1,n,17 if bulk else 1),n]
    elif schedule>=6:ends=[schedule-5,n]
    else:
     state=schedule-1;pos=0;ends=[]
     while pos<n:
      state ^= (state << 13)&0xffffffffffffffff;state ^= state>>7;state ^= (state<<17)&0xffffffffffffffff
      pos=min(n,pos+1+state%(64 if bulk else 23));ends.append(pos)
    for feed in range(len(ends)):yield f'{c["id"]}_seed_{seed}_schedule_{schedule}_feed_{feed}'
def unique(pairs):
 d={}
 for k,v in pairs:
  assert k not in d,'duplicate json key';d[k]=v
 return d
h=hashlib.sha256();count=total=maximum=0;keys=None
proc=subprocess.Popen(['zstd','-q','-d','--memory=8MB','--stdout',str(path)],stdout=subprocess.PIPE)
with gzip.open(base/'original1/actual.jsonl.gz','rb') as reference:
 for expected in ids():
  b=proc.stdout.readline(4*1024*1024+1);other=reference.readline(4*1024*1024+1)
  assert b and len(b)<=4*1024*1024 and b.endswith(b'\n');assert b==other,'not byte-identical to original'
  total+=len(b);assert total<=meta['canonical_bytes'];h.update(b);maximum=max(maximum,len(b))
  actual=json.loads(b,object_pairs_hook=unique);assert len(actual)==2 and actual[0]==expected
  current=tuple(actual[1]);keys=current if keys is None else keys;assert current==keys and len(keys)==133
  count+=1
 assert proc.stdout.read(1)==b'' and reference.read(1)==b''
assert proc.wait()==0 and count==corpus['expected_record_count']==meta['records']
assert total==meta['canonical_bytes'] and h.hexdigest()==meta['canonical_sha256']
report={'records':count,'lanes':len(keys),'canonical_bytes':total,'max_record_bytes':maximum,'canonical_sha256':h.hexdigest(),'byte_equal_original_gzip':True,'exact_ids_order':True,'system_zstd_decode_memory_limit':'8MB','corpus_sha256':hashlib.sha256((base/'corpus.json').read_bytes()).hexdigest()}
(root/'independent-audit.json').write_text(json.dumps(report,indent=2)+'\n');print(json.dumps(report))
