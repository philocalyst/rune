#!/usr/bin/env python3
"""Hosted authenticated Debian data-only acquisition; NEVER GNU execution."""
import datetime,hashlib,json,os,pathlib,re,resource,shutil,signal,stat,subprocess,time
S=pathlib.Path.cwd().resolve();C=S/'scripts/ci-gnu-payload'
BASE_PIN='6ff8ea98a1cf10f2da8a549cb675499090ebfd79c6b0f7b010745283709a584f'
HELPERS_PIN='e840ae751aac854ba955643505fb9c740103a1c50b169381ce0006665532ac10'
OVERLAY=set(['.github/workflows/wip-gnu-payload.yml', 'docs/wip-gnu-payload-scope.md', 'scripts/ci-gnu-payload/authentication-receipt.json', 'scripts/ci-gnu-payload/base-inventory.json', 'scripts/ci-gnu-payload/debian-archive-keyring.gpg', 'scripts/ci-gnu-payload/extract_static.py', 'scripts/ci-gnu-payload/helper-inventory.json', 'scripts/ci-gnu-payload/packages.json', 'scripts/ci-gnu-payload/payload-inventory.json', 'scripts/ci-gnu-payload/run.py', 'scripts/ci-gnu-payload/verify_auth.py'])
OVERLAY.add('scripts/ci-exec-admission/helpers/cases.json')
OVERLAY.update(['.github/workflows/wip-exec-admission.yml', 'docs/wip-exec-admission-scope.md', 'scripts/ci-exec-admission/base-inventory.json', 'scripts/ci-exec-admission/helpers/README.md', 'scripts/ci-exec-admission/helpers/case.py', 'scripts/ci-exec-admission/helpers/config.h.example', 'scripts/ci-exec-admission/helpers/launcher.c', 'scripts/ci-exec-admission/helpers/manifest.json', 'scripts/ci-exec-admission/helpers/prepare_config.py', 'scripts/ci-exec-admission/helpers/suite.py', 'scripts/ci-exec-admission/helpers/supervisor.py', 'scripts/ci-exec-admission/helpers/target.c', 'scripts/ci-exec-admission/run.py'])
OVERLAY.update(['.github/workflows/wip-gnu-closure.yml', 'docs/wip-gnu-closure-scope.md', 'scripts/ci-gnu-closure/base-inventory.json', 'scripts/ci-gnu-closure/helper-inventory.json', 'scripts/ci-gnu-closure/inspect.py', 'scripts/ci-gnu-closure/run.py'])
OVERLAY.update(['scripts/ci-gnu-closure/providers/archive-inventory.json', 'scripts/ci-gnu-closure/providers/debian-archive-keyring.gpg', 'scripts/ci-gnu-closure/providers/extract.py', 'scripts/ci-gnu-closure/providers/packages.json', 'scripts/ci-gnu-closure/providers/selected.json', 'scripts/ci-gnu-closure/providers/verify_auth.py'])
RELEASE_PIN='2925561eb935754c05d261fbd7ace3ced4fd83e9507d6ee1226e3ef788f25d4a'
INDEX_PIN='b74b5bbca6f4701df6c5db54793a68449fb267ac10a9314e761b972ac63b3760'
def sha(p):
 h=hashlib.sha256()
 with p.open('rb') as f:
  while b:=f.read(1024*1024):h.update(b)
 return h.hexdigest()
def save(p,v):
 with p.open('x') as f:json.dump(v,f,indent=2);f.write('\n')
def verify(commit):
 assert sha(C/'base-inventory.json')==BASE_PIN and sha(C/'helper-inventory.json')==HELPERS_PIN
 base=json.loads((C/'base-inventory.json').read_text());helpers=json.loads((C/'helper-inventory.json').read_text())
 for root,items in ((S,base),(C,helpers)):
  for n,h in items.items():p=root/n;assert p.is_file() and not p.is_symlink() and sha(p)==h,n
 tracked=set(filter(None,subprocess.check_output(['git','ls-files','-z'],cwd=S).decode().split('\0')))
 assert tracked==set(base)|OVERLAY
 assert subprocess.check_output(['git','rev-parse','HEAD'],cwd=S,text=True).strip()==commit
 assert not subprocess.check_output(['git','status','--porcelain','--untracked-files=no'],cwd=S,text=True).strip()
def caps():
 for kind,limit in ((resource.RLIMIT_CORE,0),(resource.RLIMIT_AS,1024**3),(resource.RLIMIT_CPU,30),(resource.RLIMIT_FSIZE,256*1024**2)):resource.setrlimit(kind,(limit,limit))
def phase(label,command,seconds,E,env):
 start=time.monotonic();proc=None;owned=False;stop=None;out=E/(label+'.out');err=E/(label+'.err')
 try:
  with out.open('xb') as stdout,err.open('xb') as stderr:
   proc=subprocess.Popen(['/usr/bin/timeout','--signal=TERM','--kill-after=3s',str(seconds-3)+'s',*command],cwd=E,env=env,stdin=subprocess.DEVNULL,stdout=stdout,stderr=stderr,start_new_session=True,close_fds=True,preexec_fn=caps);owned=True
   while True:
    try:ended=os.waitid(os.P_PID,proc.pid,os.WEXITED|os.WNOHANG|os.WNOWAIT)
    except ChildProcessError:owned=False;raise RuntimeError('wait ownership lost; teardown required')
    if ended is not None:break
    if time.monotonic()-start>=seconds:stop='wall'
    elif out.stat().st_size+err.stat().st_size>1024**2:stop='sampled logs'
    elif shutil.disk_usage(E).free<512*1024**2:stop='disk floor'
    if stop:break
    time.sleep(.02)
 finally:
  if proc is not None and owned:
   try:os.killpg(proc.pid,signal.SIGKILL)
   except ProcessLookupError:pass
   proc.wait()
 elapsed=time.monotonic()-start
 assert proc is not None
 record={'phase':label,'exit':proc.returncode,'stop':stop,'seconds':elapsed,'stdout_sha256':sha(out),'stderr_sha256':sha(err)};save(E/(label+'.json'),record);print(json.dumps(record),flush=True)
 if proc.returncode or stop or elapsed>seconds or out.stat().st_size+err.stat().st_size>1024**2 or shutil.disk_usage(E).free<512*1024**2:
  print(json.dumps({'stdout':out.read_bytes()[-2000:].decode(errors='replace'),'stderr':err.read_bytes()[-4000:].decode(errors='replace')}),flush=True);raise RuntimeError('phase failed; no retry')
 return out.read_text()
def download(label,url,destination,size,digest,E,env):
 assert not destination.exists()
 phase(label,['/usr/bin/curl','--silent','--show-error','--fail','--location','--proto','=https','--proto-redir','=https','--max-redirs','3','--max-time','40','--max-filesize',str(size),url,'--output',str(destination)],50,E,env)
 st=destination.lstat();assert stat.S_ISREG(st.st_mode) and st.st_uid==os.getuid() and st.st_nlink==1 and st.st_size==size and sha(destination)==digest
 destination.chmod(0o400)
def main():
 signal.signal(signal.SIGCHLD,signal.SIG_DFL);assert hasattr(os,'waitid') and hasattr(os,'WNOWAIT')
 assert os.environ.get('GITHUB_ACTIONS')=='true' and os.environ.get('GITHUB_REPOSITORY')=='philocalyst/rune'
 assert os.environ.get('GITHUB_EVENT_NAME')=='push' and os.environ.get('GITHUB_REF')=='refs/heads/work/rune-backtrace-untested-20261117'
 commit=os.environ['EXPECTED_COMMIT'];assert re.fullmatch('[0-9a-f]{40}',commit) and commit==os.environ['GITHUB_SHA'];verify(commit)
 tag=os.environ['GITHUB_RUN_ID']+'-'+os.environ['GITHUB_RUN_ATTEMPT'];assert re.fullmatch('[0-9]+-[0-9]+',tag)
 E=pathlib.Path(os.environ['RUNNER_TEMP']).resolve()/('rune-gnu-payload-'+tag);E.mkdir(mode=0o700,exist_ok=False)
 assert E.stat().st_uid==os.getuid() and stat.S_IMODE(E.stat().st_mode)==0o700
 for n in ('home','tmp','metadata','archives'):(E/n).mkdir(mode=0o700)
 env={'PATH':'/usr/bin:/bin','HOME':str(E/'home'),'TMPDIR':str(E/'tmp'),'LANG':'C','LC_ALL':'C','TZ':'UTC','PYTHONDONTWRITEBYTECODE':'1'}
 assert shutil.disk_usage(E).free>=1024**3
 tools={}
 for n in ('python3','curl','dpkg-deb','gpgv','readelf','timeout'):
  p=pathlib.Path('/usr/bin')/n;real=p.resolve();assert real.is_file();v=phase(n+'-version',[str(p),'--version'],10,E,env);tools[n]={'path':str(real),'sha256':sha(real),'version':v[:4096]}
 assert 'GNU coreutils' in tools['timeout']['version']
 download('release','https://deb.debian.org/debian/dists/bookworm-backports/InRelease',E/'metadata/InRelease',59432,RELEASE_PIN,E,env)
 download('index','https://deb.debian.org/debian/dists/bookworm-backports/main/binary-amd64/by-hash/SHA256/'+INDEX_PIN,E/'metadata/Packages.xz',300424,INDEX_PIN,E,env)
 # Fresh signature/chain/time verification, not just a copied success receipt.
 phase('authenticate',['/usr/bin/python3','-I',str(C/'verify_auth.py'),str(E/'metadata'),str(E/'authentication.json')],30,E,env)
 receipt_path=E/'authentication.json';st=receipt_path.lstat();assert stat.S_ISREG(st.st_mode) and st.st_uid==os.getuid() and st.st_nlink==1 and stat.S_IMODE(st.st_mode)==0o600
 receipt=json.loads(receipt_path.read_text());assert receipt['authenticated'] is True and receipt['package_code_executed'] is False
 assert receipt['inrelease_sha256']==RELEASE_PIN and receipt['packages_xz_sha256']==INDEX_PIN
 packages=json.loads((C/'packages.json').read_text())
 for v in packages:download(v['package'],'https://deb.debian.org/debian/pool/main/e/emacs/'+v['filename'],E/'archives'/v['filename'],v['size'],v['sha256'],E,env)
 phase('extract',['/usr/bin/python3','-I',str(C/'extract_static.py'),str(E/'archives'),str(E/'payload')],60,E,env)
 expected={};links={};dirs=set()
 for rows in json.loads((C/'payload-inventory.json').read_text()).values():
  for r in rows:
   if r['type']=='0':assert r['path'] not in expected;expected[r['path']]=r
   elif r['type']=='2':assert r['path'] not in links;links[r['path']]=r['link']
   elif r['type']=='5':dirs.add(r['path'])
   else:raise AssertionError('unexpected type')
 actual={str(p.relative_to(E/'payload')) for p in (E/'payload').rglob('*') if not p.is_dir() or p.is_symlink()}
 assert actual==set(expected)|set(links)
 actual_dirs={str(p.relative_to(E/'payload')) for p in (E/'payload').rglob('*') if p.is_dir() and not p.is_symlink()}
 assert actual_dirs==dirs-{'.'}
 for n,r in expected.items():
  p=E/'payload'/n;st=p.lstat();assert stat.S_ISREG(st.st_mode) and st.st_uid==os.getuid() and stat.S_IMODE(st.st_mode)==0o400 and st.st_nlink==1 and st.st_size==r['size'] and sha(p)==r['sha256']
 for n,target in links.items():
  p=E/'payload'/n;assert p.is_symlink() and os.readlink(p)==target and p.resolve().is_relative_to(E/'payload')
 for p in (E/'payload').rglob('*'):
  if p.is_dir() and not p.is_symlink():assert stat.S_IMODE(p.stat().st_mode)==0o700 and p.stat().st_uid==os.getuid()
 # Static ELF parser only. The extracted GNU file remains non-executable.
 elf=E/'payload/usr/bin/emacs-nox';metadata=phase('elf',['/usr/bin/readelf','-l','-d','--version-info',str(elf)],15,E,env)
 assert '/lib64/ld-linux-x86-64.so.2' in metadata
 verify(commit)
 for n,v in tools.items():assert sha(pathlib.Path(v['path']))==v['sha256']
 now=datetime.datetime.now(datetime.timezone.utc);assert now<datetime.datetime.fromisoformat(receipt['valid_until'])
 result={'authenticated_payload_extracted':True,'commit':commit,'package_version':'1:30.1+1-6~bpo12+1','files':len(expected),'symlinks':len(links),'gnu_elf_sha256':sha(elf),'inrelease_sha256':RELEASE_PIN,'tools':tools,'gnu_executed':False,'runtime_closure_complete':False,'gnu16_tested':False,'doom_tested':False,'old_local_process':'UNKNOWN/unaffected'}
 save(E/'result.json',result);print(json.dumps(result),flush=True)
 if os.environ.get('GITHUB_STEP_SUMMARY'):pathlib.Path(os.environ['GITHUB_STEP_SUMMARY']).write_text('Authenticated Debian GNU30.1 payload data only. No GNU executable, reference tests or Doom startup ran. Commit '+commit+'\n')
 return E
if __name__=='__main__':main()
