#!/usr/bin/env python3
"""Exact signed-archive-derived shared files only. No package code runs."""
import hashlib,json,os,pathlib,stat,subprocess,sys,tarfile
C=pathlib.Path(__file__).resolve().parent
PINS={'selected.json': 'bb0bc59e2342c5613e16423772cbe2bf7cb27db07958928806dd114cb88d2c9b', 'archive-inventory.json': 'ac0b26a7096c78e228a80e4898b61d8a129586de7ce85a5019fbbad5e1d5c622', 'packages.json': 'ddc986ce8f6808bcb2776377f120348fefcfccb52de355b87419cbb1dab4e0c0'}

def need(ok,msg):
 if not ok:raise RuntimeError(msg)
def sha(p):
 h=hashlib.sha256()
 with p.open('rb') as f:
  while b:=f.read(1024**2):h.update(b)
 return h.hexdigest()
def private(p):
 st=p.lstat();need(p.is_absolute() and p.resolve()==p and stat.S_ISDIR(st.st_mode) and st.st_uid==os.getuid() and stat.S_IMODE(st.st_mode)==0o700,'private canonical directory')
class Bounded:
 def __init__(self,f):self.f=f;self.total=0
 def read(self,n=-1):
  need(0<=n<=64*1024**2,'bounded stream read');b=self.f.read(n);self.total+=len(b);need(self.total<=64*1024**2,'archive stream exceeds bound');return b

def main():
 need(len(sys.argv)==3,'ARCHIVE_DIR FRESH_PROVIDER_DIR');archives=pathlib.Path(sys.argv[1]);out=pathlib.Path(sys.argv[2]);private(archives);private(out.parent);out.mkdir(mode=0o700);private(out)
 for n,h in PINS.items():need(sha(C/n)==h,'metadata pin')
 packages=json.loads((C/'packages.json').read_text());inventory=json.loads((C/'archive-inventory.json').read_text());selected=json.loads((C/'selected.json').read_text());need(len(packages)==len(selected)==2,'exact provider pair')
 for package in packages:
  p=archives/package['filename'];st=p.lstat();need(stat.S_ISREG(st.st_mode) and st.st_uid==os.getuid() and st.st_nlink==1 and st.st_size==package['size'] and sha(p)==package['sha256'],'archive identity')
  rows=inventory[package['package']];wanted=[r for r in selected if r['package']==package['package']];need(len(wanted)==1,'selected package membership');wanted=wanted[0];seen=[];copied=0
  err=out/(package['package']+'.extract.stderr')
  with err.open('xb') as error:
   child=subprocess.Popen(['/usr/bin/dpkg-deb','--fsys-tarfile',str(p)],stdin=subprocess.DEVNULL,stdout=subprocess.PIPE,stderr=error,env={'PATH':'/usr/bin:/bin','LANG':'C','LC_ALL':'C'},close_fds=True)
   try:
    stream=Bounded(child.stdout)
    with tarfile.open(fileobj=stream,mode='r|') as tar:
     for member in tar:
      row={'path':member.name,'type':member.type.decode('ascii'),'size':member.size,'mode':oct(member.mode),'link':member.linkname}
      need(len(seen)<len(rows) and row=={k:rows[len(seen)][k] for k in row},'archive member mismatch');seen.append(row)
      parts=pathlib.PurePosixPath(member.name).parts;need(not member.name.startswith('/') and '..' not in parts and member.type in (tarfile.REGTYPE,tarfile.DIRTYPE,tarfile.SYMTYPE),'unsafe archive structure')
      if member.name!=wanted['path']:continue
      need(member.isfile() and member.size==wanted['size'] and copied==0,'selected member mismatch')
      target=out/wanted['soname'][0];need(target.name in ('libgccjit.so.0','libtree-sitter.so.0'),'fixed output name');h=hashlib.sha256();total=0
      with target.open('xb') as f:
       source=tar.extractfile(member)
       while b:=source.read(1024**2):total+=len(b);need(total<=wanted['size'],'member excess');h.update(b);f.write(b)
      target.chmod(0o400);need(total==wanted['size'] and h.hexdigest()==wanted['sha256'],'selected bytes');copied+=1
    while stream.read(1024**2):pass
    need(child.wait(timeout=10)==0 and err.stat().st_size==0 and seen==[{k:r[k] for k in ('path','type','size','mode','link')} for r in rows] and copied==1,'extract completion')
   finally:
    if child.returncode is None:child.kill();child.wait(timeout=5)
    child.stdout.close()
  need(sha(p)==package['sha256'],'archive changed');err.unlink()
 for r in selected:
  p=out/r['soname'][0];st=p.lstat();need(stat.S_ISREG(st.st_mode) and st.st_uid==os.getuid() and st.st_nlink==1 and stat.S_IMODE(st.st_mode)==0o400 and st.st_size==r['size'] and sha(p)==r['sha256'],'provider integrity')
 need({p.name for p in out.iterdir()}=={'libgccjit.so.0','libtree-sitter.so.0'},'provider fileset')
 for n,h in PINS.items():need(sha(C/n)==h,'metadata changed')
 print(json.dumps({'inert_providers':2,'package_code_executed':False}),flush=True)
if __name__=='__main__':main()
