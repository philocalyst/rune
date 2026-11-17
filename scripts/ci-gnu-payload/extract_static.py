#!/usr/bin/env python3
"""Fixed authenticated Debian payload extraction, NOT installation or execution.
Requires an independently bounded trusted-host supervisor. No network or package
program is invoked. Runtime dependency/launcher qualification is separate.
"""
import datetime,hashlib,json,os,pathlib,stat,subprocess,sys,tarfile
HERE=pathlib.Path(__file__).resolve().parent
PINS={'packages.json':'eb1f62d2d894419f8792a52f1b41db7fecf0b761bcb24ac52a987be767797441','payload-inventory.json':'09b5580652e007149f550fbb02feb0350dcc2274ebe05ff1605125230c16e7fd','authentication-receipt.json':'15f5b5d0fcacac79a1f3a61406f9de99003fa8e0fd7249240c130941d4a17d97'}
def sha(path):
 h=hashlib.sha256()
 with path.open('rb') as f:
  while chunk:=f.read(1024*1024):h.update(chunk)
 return h.hexdigest()
def main():
 assert len(sys.argv)==3,'archive-directory fresh-output-directory'
 for n,h in PINS.items():assert sha(HERE/n)==h
 packages=json.loads((HERE/'packages.json').read_text());expected=json.loads((HERE/'payload-inventory.json').read_text());auth=json.loads((HERE/'authentication-receipt.json').read_text())
 now=datetime.datetime.now(datetime.timezone.utc)
 assert datetime.datetime.fromisoformat(auth['release_date'])<=now<datetime.datetime.fromisoformat(auth['valid_until']),'signed metadata expired/not yet valid; re-review required'
 source=pathlib.Path(sys.argv[1]);dest=pathlib.Path(sys.argv[2]);assert source.is_absolute() and dest.is_absolute()
 assert source.resolve()==source and dest.parent.resolve()==dest.parent and not dest.exists() and not dest.is_symlink()
 assert dest.parent.stat().st_uid==os.getuid()
 for v in packages:
  signed=[r for r in auth['packages'] if r['Package']==v['package']];assert len(signed)==1;r=signed[0]
  assert r['Version']==v['version'] and r['Architecture']==v['architecture'] and r['SHA256']==v['sha256'] and int(r['Size'])==v['size']
  p=source/v['filename'];st=p.lstat();assert stat.S_ISREG(st.st_mode) and st.st_uid==os.getuid() and st.st_nlink==1 and st.st_size==v['size'] and sha(p)==v['sha256']
 dest.mkdir(mode=0o700);links=[];files=[];total=0
 for v in packages:
  rows={r['path']:r for r in expected[v['package']]};assert len(rows)==len(expected[v['package']]);seen=set()
  # Only the archive data stream. Never --install, scripts, triggers, ldd,
  # loader invocation, or any program extracted from a package.
  proc=subprocess.Popen(['/usr/bin/dpkg-deb','--fsys-tarfile',str(source/v['filename'])],stdin=subprocess.DEVNULL,stdout=subprocess.PIPE,env={'PATH':'/usr/bin:/bin','LANG':'C','LC_ALL':'C'},close_fds=True)
  try:
   with tarfile.open(fileobj=proc.stdout,mode='r|') as archive:
    for m in archive:
     n=str(pathlib.PurePosixPath(m.name));assert not n.startswith('/') and '..' not in pathlib.PurePosixPath(n).parts and n not in seen;seen.add(n)
     r=rows[n];assert m.size==r['size'] and oct(m.mode)==r['mode'] and m.type.decode()==r['type'] and m.linkname==r['link']
     p=dest/n
     if m.isdir():
      p.mkdir(mode=0o700,parents=True,exist_ok=True);assert p.is_dir() and not p.is_symlink()
     elif m.isfile():
      total+=m.size;assert total<=256*1024**2 and m.size<=100*1024**2
      p.parent.mkdir(mode=0o700,parents=True,exist_ok=True)
      h=hashlib.sha256();count=0
      with archive.extractfile(m) as reader,p.open('xb') as writer:
       while chunk:=reader.read(1024*1024):writer.write(chunk);h.update(chunk);count+=len(chunk)
      assert count==m.size and h.hexdigest()==r['sha256'];p.chmod(0o400);files.append(n)
     elif m.issym():
      assert not pathlib.PurePosixPath(m.linkname).is_absolute()
      target=(p.parent/m.linkname).resolve();assert target.is_relative_to(dest)
      links.append((p,m.linkname))
     else:raise AssertionError('unexpected special/hardlink entry')
   assert proc.wait(timeout=5)==0
  except BaseException:
   # No polling/reaping precedes kill; dpkg-deb is our sole direct child.
   proc.kill();proc.wait(timeout=5);raise
  assert seen==set(rows)
 # Delayed links ensure no archive writes traverse a newly created symlink.
 for p,target in links:p.parent.mkdir(mode=0o700,parents=True,exist_ok=True);p.symlink_to(target)
 for v in packages:assert sha(source/v['filename'])==v['sha256']
 # Leave ALL extracted files non-executable. A later reviewed launcher stage
 # copies only the pinned GNU target; this extractor cannot run any of them.
 print(json.dumps({'payload_extracted':True,'files':len(files),'symlinks':len(links),'bytes':total,'reference_executed':False,'runtime_closure_complete':False,'valid_until':auth['valid_until']}))
if __name__=='__main__':main()
