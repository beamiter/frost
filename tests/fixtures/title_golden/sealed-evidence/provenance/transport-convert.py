import gzip,zstandard as z,pathlib,json,hashlib,time
root=pathlib.Path(__file__).resolve().parent
source=root.parents[1]/'frost-title-baseline/original1/actual.jsonl.gz'
meta=json.loads((source.parent/'verification.json').read_text()); results=[]
for run in [1,2]:
 path=root/f'original1.level9.w23.run{run}.zst';h=hashlib.sha256();n=0;start=time.monotonic()
 cp=z.ZstdCompressionParameters.from_level(9,window_log=23,threads=0,write_checksum=1,write_content_size=1)
 with gzip.open(source,'rb') as f,path.open('wb') as out,z.ZstdCompressor(compression_params=cp).stream_writer(out,size=meta['canonical_bytes'],closefd=False) as writer:
  while block:=f.read(65536):
   writer.write(block);h.update(block);n+=len(block)
 assert n==meta['canonical_bytes'] and h.hexdigest()==meta['canonical_sha256']
 packed=path.read_bytes();frame=z.get_frame_parameters(packed);assert frame.window_size==8388608 and frame.has_checksum
 results.append({'path':str(path),'compressed_bytes':len(packed),'sha256':hashlib.sha256(packed).hexdigest(),'window_size':frame.window_size,'canonical_bytes':n,'canonical_sha256':h.hexdigest(),'seconds':time.monotonic()-start})
assert results[0]['sha256']==results[1]['sha256']
(root/'conversion.json').write_text(json.dumps({'parameters':{'level':9,'window_log':23,'threads':0,'checksum':True,'content_size':meta['canonical_bytes'],'input_chunk':65536},'results':results,'deterministic':True},indent=2)+'\n')
print(json.dumps(results))
