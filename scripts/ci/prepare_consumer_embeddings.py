"""Prepare immutable Apache-2.0 MiniLM ONNX components for the offline package."""
import argparse,hashlib,json,shutil,tempfile,urllib.request
from pathlib import Path
REVISION='751bff37182d3f1213fa05d7196b954e230abad9'
FILES={'model.onnx':('onnx/model.onnx','759c3cd2b7fe7e93933ad23c4c9181b7396442a2ed746ec7c1d46192c469c46e'),'tokenizer.json':('tokenizer.json','da0e79933b9ed51798a3ae27893d3c5fa4a201126cef75586296df9b4d2c62a0')}
def prepare(output,cache=None):
    output.mkdir(parents=True,exist_ok=True);rows=[]
    for name,(relative,expected) in FILES.items():
        target=output/name;url=f'https://huggingface.co/Xenova/all-MiniLM-L6-v2/resolve/{REVISION}/{relative}'
        if not target.is_file() or hashlib.sha256(target.read_bytes()).hexdigest()!=expected:
            with tempfile.NamedTemporaryFile(dir=output,delete=False) as temporary:
                staged=Path(temporary.name)
                if cache and (cache/name).is_file() and hashlib.sha256((cache/name).read_bytes()).hexdigest()==expected:
                    with (cache/name).open('rb') as source:shutil.copyfileobj(source,temporary)
                else:
                    with urllib.request.urlopen(url,timeout=120) as response:
                        total=0
                        while block:=response.read(1024*1024):
                            total+=len(block)
                            if total>100*1024*1024:raise ValueError('Embedding component exceeds limit')
                            temporary.write(block)
            if hashlib.sha256(staged.read_bytes()).hexdigest()!=expected:staged.unlink();raise ValueError('Embedding hash mismatch: '+name)
            staged.replace(target)
        rows.append({'name':name,'sha256':expected,'bytes':target.stat().st_size,'url':url})
    root=Path(__file__).resolve().parents[2];shutil.copy2(root/'LICENSE',output/'LICENSE-APACHE-2.0.txt')
    (output/'NOTICE.txt').write_text(f'all-MiniLM-L6-v2: sentence-transformers. ONNX export: Xenova. Apache-2.0.\nSource: https://huggingface.co/Xenova/all-MiniLM-L6-v2/tree/{REVISION}\n',encoding='utf-8')
    (output/'embedding-lock.json').write_text(json.dumps({'schema_version':1,'revision':REVISION,'license':'Apache-2.0','files':rows},indent=2)+'\n',encoding='utf-8')
if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path,required=True);p.add_argument('--cache',type=Path);a=p.parse_args();prepare(a.output,a.cache)
