#!/usr/bin/env python3
"""Trusted fixed-case adapter, always under external reviewed subreaper/watchdog.
No compiler, shell, GNU or arbitrary payload. Expects already qualified pins.
"""
import hashlib,json,os,pathlib,select,signal,socket,stat,subprocess,sys,time
MODES=['positive','read-denied','write-denied','truncate-denied','exec-denied','socket-inet','socket-inet6','socket-unix','socketpair','io-uring','ptrace','pidfd-getfd','inherited-fds','symlink-read','invalid-policy']
FORBIDDEN=set(MODES[5:12])
def sha(p):return hashlib.sha256(p.read_bytes()).hexdigest()
def identity(p):
 s=p.lstat();return (s.st_dev,s.st_ino,s.st_mode,s.st_uid,s.st_gid,s.st_nlink,s.st_size)
def main():
 assert len(sys.argv)==3;root=pathlib.Path(sys.argv[1]);mode=sys.argv[2];assert mode in MODES
 assert root.is_absolute() and root.resolve()==root and root.stat().st_uid==os.getuid() and stat.S_IMODE(root.stat().st_mode)==0o700
 # Wrapper must authenticate these records before adapter entry; hashes supplied
 # by an arbitrary caller are not trust. Freeze bytes AND inode metadata here.
 receipts={n:(identity(root/n),sha(root/n)) for n in ['config.h','config.json','build.json']}
 for n in receipts:
  st=(root/n).lstat();assert stat.S_ISREG(st.st_mode) and st.st_uid==os.getuid() and st.st_nlink==1 and not st.st_mode&0o022
 cfg=json.loads((root/'config.json').read_text());build=json.loads((root/'build.json').read_text())
 assert cfg['root']==str(root) and build['root']==str(root) and sha(root/'config.h')==cfg['config_sha256']==build['config_sha256']
 binaries={}
 for n in ['launcher','target','denied-target']:
  p=root/n;s=p.lstat();assert stat.S_ISREG(s.st_mode) and s.st_uid==os.getuid() and s.st_nlink==1 and stat.S_IMODE(s.st_mode)==0o500 and sha(p)==build['files'][n];binaries[n]=identity(p)
 assert sha(root/'target')==sha(root/'denied-target') and (root/'target').stat().st_dev==(root/'denied-target').stat().st_dev and (root/'target').stat().st_ino!=(root/'denied-target').stat().st_ino
 assert build['interpreter']==cfg['runtime'][0]['path'] and build['needed']==['libc.so.6']
 runtime={}
 for r in cfg['runtime']:
  p=pathlib.Path(r['path']);assert p.is_absolute() and p.resolve()==p and p.is_relative_to('/usr/lib')
  st=p.lstat();assert stat.S_ISREG(st.st_mode) and st.st_uid==0 and not st.st_mode&0o022 and sha(p)==r['sha256']
  assert (st.st_dev,st.st_ino)==(r['device'],r['inode']);runtime[str(p)]=(identity(p),r['sha256'])
 # Freeze lexical loader/libc alias chains, not only final canonical files.
 aliases={}
 for name in ['/lib64/ld-linux-x86-64.so.2','/lib/x86_64-linux-gnu/libc.so.6']:
  p=pathlib.Path(name);assert str(p.resolve()) in runtime
  aliases[name]=(identity(p),str(p.resolve()),os.readlink(p) if p.is_symlink() else None)
  for parent in p.parents:
   if parent==pathlib.Path('/'):continue
   aliases[str(parent)]=(identity(parent),str(parent.resolve()),os.readlink(parent) if parent.is_symlink() else None)
 canaries={}
 for p in [root/'allowed/input',root/'outside/canary']:
  st=p.lstat();assert stat.S_ISREG(st.st_mode) and st.st_uid==os.getuid() and st.st_nlink==1 and stat.S_IMODE(st.st_mode)==0o600 and p.read_bytes()==b'probe-canary\n'
  # Ordinary DAC permits read AND write before confinement; no byte mutation.
  fd=os.open(p,os.O_WRONLY|os.O_NOFOLLOW|os.O_CLOEXEC);assert os.fstat(fd).st_ino==st.st_ino;os.close(fd)
  canaries[str(p)]=identity(p)
 link=root/'allowed/outside-link';link_identity=identity(link);assert stat.S_ISLNK(link.lstat().st_mode)
 directories={str(p):identity(p) for p in [root,root/'allowed',root/'outside',root/'home',root/'tmp']}
 for name in directories:
  st=pathlib.Path(name).lstat();assert stat.S_ISDIR(st.st_mode) and st.st_uid==os.getuid() and stat.S_IMODE(st.st_mode)==0o700
 assert not (root/'allowed/output').exists();assert os.readlink(root/'allowed/outside-link')=='../outside/canary'
 # Reserve FD4 before making pipe; do not accidentally replace its read end.
 reservation=os.open('/dev/null',os.O_RDONLY);os.dup2(reservation,4)
 if reservation!=4:os.close(reservation)
 rd,wr=os.pipe2(os.O_CLOEXEC|os.O_NONBLOCK);assert rd!=4 and wr!=4
 os.dup2(wr,4,inheritable=True);os.close(wr)
 handles=[];injected=[]
 if mode=='inherited-fds':
  a,b=socket.socketpair();handles.extend([a,b]);directory=os.open(root/'outside',os.O_RDONLY|os.O_DIRECTORY);file=os.open(root/'outside/canary',os.O_RDONLY)
  for src,dst in [(a.fileno(),100),(directory,101),(file,102)]:
   os.dup2(src,dst,inheritable=True);s=os.fstat(dst);injected.append({'fd':dst,'device':s.st_dev,'inode':s.st_ino,'mode':s.st_mode})
  os.close(directory);os.close(file)
 signal.signal(signal.SIGCHLD,signal.SIG_DFL)
 out=root/(mode+'.out');err=root/(mode+'.err');start=time.monotonic();markers=b'';proc=None;owned=False
 try:
  with out.open('xb') as o,err.open('xb') as e:
   proc=subprocess.Popen([str(root/'launcher'),mode],stdin=subprocess.DEVNULL,stdout=o,stderr=e,cwd=root,env={'LANG':'C','LC_ALL':'C','PATH':'/usr/bin:/bin','HOME':str(root/'home'),'TMPDIR':str(root/'tmp')},pass_fds=(4,100,101,102) if injected else (4,),start_new_session=True);owned=True
   os.close(4)
   if injected:
    for fd in [100,101,102]:os.close(fd)
   while True:
    try:chunk=os.read(rd,32)
    except BlockingIOError:chunk=b''
    markers+=chunk;assert len(markers)<=16
    assert time.monotonic()-start<3,'case timeout'
    try:ended=os.waitid(os.P_PID,proc.pid,os.WEXITED|os.WNOHANG|os.WNOWAIT)
    except ChildProcessError:owned=False;raise
    if ended is not None:break
    select.select([rd],[],[],.01)
 finally:
  if proc is not None and owned:
   try:os.killpg(proc.pid,signal.SIGKILL)
   except ProcessLookupError:pass
   proc.wait()
 # Drain only after anchored leader cleanup. Parent pipe owns finite markers.
 while True:
  try:chunk=os.read(rd,32)
  except BlockingIOError:break
  if not chunk:break
  markers+=chunk;assert len(markers)<=16
 os.close(rd)
 for h in handles:h.close()
 expected=bytes([0xa1,0xa2,0xb0,0xb1,0xb2]) if mode=='inherited-fds' else (b'' if mode=='invalid-policy' else bytes([0xa1,0xa2,0xa3]))
 status=-signal.SIGSYS if mode in FORBIDDEN else (77 if mode=='invalid-policy' else 0)
 assert markers==expected and proc.returncode==status and out.stat().st_size==0 and err.stat().st_size==0,(markers.hex(),proc.returncode)
 for name,before in canaries.items():
  p=pathlib.Path(name);assert identity(p)==before and p.read_bytes()==b'probe-canary\n'
 assert identity(link)==link_identity and os.readlink(link)=='../outside/canary' and link.resolve()==root/'outside/canary'
 for name,before in directories.items():
  # Directory size can change with owned log/output entries; retain authority.
  now=identity(pathlib.Path(name));assert now[:6]==before[:6]
 for n,(before,digest) in receipts.items():assert identity(root/n)==before and sha(root/n)==digest
 for name,(before,digest) in runtime.items():
  p=pathlib.Path(name);assert identity(p)==before and sha(p)==digest and str(p.resolve())==name
 for name,(before,resolved,target) in aliases.items():
  p=pathlib.Path(name);assert identity(p)==before and str(p.resolve())==resolved and (os.readlink(p) if p.is_symlink() else None)==target
 if mode=='positive':
  p=root/'allowed/output';st=p.lstat();assert stat.S_ISREG(st.st_mode) and st.st_uid==os.getuid() and st.st_nlink==1 and stat.S_IMODE(st.st_mode)==0o600 and p.read_bytes()==b'probe-output\n'
 else:assert not (root/'allowed/output').exists()
 assert set(p.name for p in (root/'outside').iterdir())=={'canary'}
 assert set(p.name for p in (root/'allowed').iterdir())==({'input','outside-link','output'} if mode=='positive' else {'input','outside-link'})
 for n in ['launcher','target','denied-target']:assert identity(root/n)==binaries[n] and sha(root/n)==build['files'][n]
 print(json.dumps({'mode':mode,'markers':markers.hex(),'exit':proc.returncode,'injected':injected,'passed':True,'gnu_executed':False}))
if __name__=='__main__':main()
