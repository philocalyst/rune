#!/usr/bin/env python3
"""Independent hosted-runner evidence only; no local-process resolution claim."""
import hashlib,json,os,pathlib,re,resource,shutil,signal,subprocess,time
S=pathlib.Path.cwd().resolve(); C=S/'scripts/ci-v11'
TESTS_PIN='a95e3b99ef850a7ca1c8452e938e3108b375a28f9c7042f1166a8e1568bcdad6'
SOURCE_PIN='45a8e02823165863105a9649589a8fc7f5259b54e00d337d8627f72553ab487a'
EXTRAS={'.github/workflows/wip-v11-native.yml','scripts/ci-v11/run.py','scripts/ci-v11/source-inventory.json','scripts/ci-v11/native-tests.json','docs/wip-v11-ci-scope.md'}
EXTRAS.update(['.github/workflows/wip-bootstrap.yml', 'scripts/ci-bootstrap/run.py', 'scripts/ci-bootstrap/check-sandbox.py', 'scripts/ci-bootstrap/checkpoint.el', 'docs/wip-bootstrap-scope.md'])
def sha(p):return hashlib.sha256(p.read_bytes()).hexdigest()
def save(p,v):p.write_text(json.dumps(v,indent=2)+'\n')
def verify():
 assert sha(C/'source-inventory.json')==SOURCE_PIN and sha(C/'native-tests.json')==TESTS_PIN
 inventory=json.loads((C/'source-inventory.json').read_text())
 tracked=subprocess.check_output(['git','ls-files','-z'],cwd=S).decode().split('\0');tracked={n for n in tracked if n}
 assert tracked==set(inventory)|EXTRAS,'unexpected tracked source set'
 for n,h in inventory.items():
  f=S/n;assert f.is_file() and not f.is_symlink() and sha(f)==h, 'source changed: '+n
 assert subprocess.check_output(['git','rev-parse','HEAD'],cwd=S,text=True).strip()==COMMIT
 assert not subprocess.check_output(['git','status','--porcelain','--untracked-files=no'],cwd=S,text=True).strip(),'tracked mutation'
def caps(filecap):
 resource.setrlimit(resource.RLIMIT_CORE,(0,0))
 resource.setrlimit(resource.RLIMIT_AS,(6*1024**3,6*1024**3))
 resource.setrlimit(resource.RLIMIT_FSIZE,(filecap,filecap))
def failure_diagnostics(out,err):
 # Cargo emits structured compiler errors on stdout, followed by many warnings
 # or artifact messages. Select errors rather than relying on a trailing slice.
 errors=[];remaining=4000
 for line in out.read_text(errors='replace').splitlines():
  try:item=json.loads(line)
  except json.JSONDecodeError:continue
  if not isinstance(item,dict) or item.get('reason')!='compiler-message':continue
  message=item.get('message',{})
  if not isinstance(message,dict) or message.get('level')!='error':continue
  rendered=message.get('rendered') or json.dumps(message,ensure_ascii=True)
  if not isinstance(rendered,str):continue
  part=rendered.encode('utf-8')[:remaining];errors.append(part.decode('utf-8',errors='replace'));remaining-=len(part)
  if remaining<=0:break
 stderr=err.read_bytes()
 if not errors:
  # Fallback for toolchain/Cargo non-JSON failures: center the first error,
  # otherwise retain the beginning rather than losing it to later warnings.
  match=re.search(rb'(?m)^error(?:\[E[0-9]+\])?:',stderr)
  start=max(0,match.start()-200) if match else 0
  errors=[stderr[start:start+4000].decode('utf-8',errors='replace')]
 return {'compiler_errors':errors,'stderr_tail':stderr[-2000:].decode(errors='replace'),'stdout_tail':out.read_bytes()[-2000:].decode(errors='replace')}
def run(label,cmd,seconds,logcap=16*1024**2,filecap=256*1024**2):
 out=E/(label+'.out');err=E/(label+'.err');start=time.monotonic();proc=None;stop=None
 try:
  with out.open('xb') as o,err.open('xb') as r:
   proc=subprocess.Popen(cmd,cwd=S,env=ENV,stdin=subprocess.DEVNULL,stdout=o,stderr=r,start_new_session=True,close_fds=True,preexec_fn=lambda:caps(filecap))
   while proc.poll() is None:
    if time.monotonic()-start>seconds:stop='wall'
    elif out.stat().st_size+err.stat().st_size>logcap:stop='sampled_log_cap'
    elif shutil.disk_usage(E).free<2*1024**3:stop='disk_floor'
    if stop:break
    time.sleep(.1)
   if stop:os.killpg(proc.pid,signal.SIGKILL)
   rc=proc.wait()
 finally:
  if proc is not None:
   try:os.killpg(proc.pid,signal.SIGKILL)
   except ProcessLookupError:pass
   proc.wait()
 elapsed=time.monotonic()-start
 record={'phase':label,'exit':rc,'stop':stop,'seconds':elapsed,'stdout_sha256':sha(out),'stderr_sha256':sha(err),'stdout_bytes':out.stat().st_size,'stderr_bytes':err.stat().st_size}
 save(E/(label+'.json'),record);print(json.dumps(record),flush=True)
 if rc or stop or elapsed>seconds or out.stat().st_size+err.stat().st_size>logcap or shutil.disk_usage(E).free<2*1024**3:
  # JSON encoding prevents multiline workflow-command interpretation.
  print(json.dumps(failure_diagnostics(out,err)),flush=True)
  raise RuntimeError('phase failed: '+label)
 return out.read_text(),err.read_text()
def main():
 global COMMIT,E,ENV
 assert os.environ.get('GITHUB_ACTIONS')=='true' and os.environ.get('GITHUB_REPOSITORY')=='philocalyst/rune'
 assert os.environ.get('GITHUB_EVENT_NAME')=='push' and os.environ.get('GITHUB_REF')=='refs/heads/work/rune-backtrace-untested-20261117'
 COMMIT=os.environ['EXPECTED_COMMIT'];assert re.fullmatch('[0-9a-f]{40}',COMMIT) and COMMIT==os.environ['GITHUB_SHA']
 verify()
 root=pathlib.Path(os.environ['RUNNER_TEMP']).resolve();E=root/('rune-v11-'+os.environ['GITHUB_RUN_ID']+'-'+os.environ['GITHUB_RUN_ATTEMPT']);E.mkdir(exist_ok=False)
 home=E/'home';home.mkdir(); target=E/'target'
 rustup=pathlib.Path(shutil.which('rustup')).resolve();assert rustup.is_file()
 # Explicit allowlist: no GITHUB_TOKEN, ACTIONS_RUNTIME_TOKEN, cloud credentials,
 # host Cargo config, caches, SSH agent or user environment reaches build/tests.
 ENV={'HOME':str(home),'CARGO_HOME':str(E/'cargo'),'RUSTUP_HOME':str(E/'rustup'),'PATH':str(rustup.parent)+':/usr/bin:/bin','LANG':'C.UTF-8','LC_ALL':'C.UTF-8','TZ':'UTC','CARGO_TARGET_DIR':str(target),'CARGO_BUILD_JOBS':'2','CARGO_INCREMENTAL':'0','RUNE_ELPROP_BACKEND':'batch','RUST_BACKTRACE':'0','GIT_TERMINAL_PROMPT':'0','RUSTUP_DIST_SERVER':'https://static.rust-lang.org','RUSTUP_UPDATE_ROOT':'https://static.rust-lang.org/rustup'}
 assert shutil.disk_usage(E).free>=8*1024**3,'disk admission'
 run('toolchain',[str(rustup),'toolchain','install','1.96.0','--profile','minimal','--no-self-update'],180)
 tool=E/'rustup/toolchains/1.96.0-x86_64-unknown-linux-gnu/bin';cargo=tool/'cargo';rustc=tool/'rustc';assert cargo.is_file() and rustc.is_file()
 ENV['PATH']=str(tool)+':/usr/bin:/bin'
 versions,_=run('versions',[str(rustc),'-Vv'],10);assert versions.startswith('rustc 1.96.0 ')
 cargo_version,_=run('cargo-version',[str(cargo),'-Vv'],10);assert cargo_version.startswith('cargo 1.96.0 ')
 save(E/'provenance.json',{'commit':COMMIT,'source_inventory_sha256':SOURCE_PIN,'rustc':versions,'cargo':cargo_version,'rustc_sha256':sha(rustc),'cargo_sha256':sha(cargo),'rustup_sha256':sha(rustup),'indexmap_git_revision':'5ceff134a843d7afc26bb63db48f927ea80883bc','scope':'native20 not GNU139 or Doom startup'})
 # Cold checkout needs locked network fetch including pinned public indexmap git.
 run('fetch',[str(cargo),'fetch','--locked','--target','x86_64-unknown-linux-gnu'],180)
 verify()
 output,_=run('build',[str(cargo),'test','--locked','--offline','--no-default-features','--bin','rune','--no-run','-j2','--message-format=json-render-diagnostics'],600)
 artifacts=[]
 for line in output.splitlines():
  try:item=json.loads(line)
  except json.JSONDecodeError:continue
  if item.get('reason')=='compiler-artifact' and item.get('target',{}).get('name')=='rune' and item.get('profile',{}).get('test') and item.get('executable'):
   assert pathlib.Path(item['target']['src_path']).resolve()==S/'src/main.rs'
   artifacts.append(pathlib.Path(item['executable']).resolve())
 assert len(artifacts)==1;binary=artifacts[0];assert binary.is_relative_to(target.resolve()) and binary.is_file()
 binary_pin=sha(binary);assert binary.open('rb').read(4)==b'\x7fELF';verify()
 selected=json.loads((C/'native-tests.json').read_text());assert len(selected)==len(set(selected))==20
 listed,_=run('list',[str(binary),'--list'],20,1024**2,1024**2)
 discovered=[line.removesuffix(': test') for line in listed.splitlines() if line.endswith(': test')]
 assert all(discovered.count(n)==1 for n in selected)
 deadline=time.monotonic()+720
 for i,name in enumerate(selected):
  assert sha(binary)==binary_pin and time.monotonic()<deadline
  remaining=min(120,deadline-time.monotonic());assert remaining>0
  out,err=run('test-'+str(i),[str(binary),name,'--exact','--test-threads=1'],remaining,1024**2,1024**2)
  assert re.search(r'test result: ok\. 1 passed; 0 failed; 0 ignored;',out),'unexpected test accounting'
  assert time.monotonic()<=deadline
 verify();assert sha(binary)==binary_pin
 result={'qualified_native_subset':True,'passed':20,'commit':COMMIT,'artifact_sha256':binary_pin,'source_inventory_sha256':SOURCE_PIN,'tests':selected,'gnu_reference_tested':False,'doom_startup_tested':False,'old_local_process':'UNKNOWN/unaffected'}
 save(E/'result.json',result);print(json.dumps(result),flush=True)
 if os.environ.get('GITHUB_STEP_SUMMARY'):
  pathlib.Path(os.environ['GITHUB_STEP_SUMMARY']).write_text('V11 native subset only:20 passed; commit '+COMMIT+'; ELF SHA256 '+binary_pin+'. Not GNU139 or Doom startup.\n')
if __name__=='__main__':main()
