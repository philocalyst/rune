#!/usr/bin/env python3
"""WIP synthetic-only confinement compile/admission; no GNU or Rune command."""
import hashlib,json,os,pathlib,re,resource,shutil,signal,subprocess,time
S=pathlib.Path.cwd().resolve();C=S/'scripts/ci-confinement'
BASE_PIN='9d361bf9188e1cf8468d3807da67837b05b9c4d1a69aeaa3470344d4a53e92b5'
HELPERS_PIN='e04360ab12e467ad1131cf842abc59ff117c1b16f8b96e61e145b0924b74e2c1'
OVERLAY={'.github/workflows/wip-confinement.yml','docs/wip-confinement-scope.md'}|{'scripts/ci-confinement/'+n for n in ('run.py','probe.c','controller.py','supervisor.py','base-inventory.json','helper-inventory.json')}
OVERLAY.add('scripts/ci-exec-admission/helpers/cases.json')
OVERLAY.update(['.github/workflows/wip-exec-admission.yml', 'docs/wip-exec-admission-scope.md', 'scripts/ci-exec-admission/base-inventory.json', 'scripts/ci-exec-admission/helpers/README.md', 'scripts/ci-exec-admission/helpers/case.py', 'scripts/ci-exec-admission/helpers/config.h.example', 'scripts/ci-exec-admission/helpers/launcher.c', 'scripts/ci-exec-admission/helpers/manifest.json', 'scripts/ci-exec-admission/helpers/prepare_config.py', 'scripts/ci-exec-admission/helpers/suite.py', 'scripts/ci-exec-admission/helpers/supervisor.py', 'scripts/ci-exec-admission/helpers/target.c', 'scripts/ci-exec-admission/run.py'])
CASES=('positive','read-denied','write-denied','truncate-denied','socket-inet','socket-inet6','socket-unix','socket-abstract','socketpair','io-uring','ptrace','pidfd-getfd','inherited-fd-close','inherited-policy','unavailable-policy','inherited-seccomp')
OVERLAY.update(['.github/workflows/wip-gnu-payload.yml', 'docs/wip-gnu-payload-scope.md', 'scripts/ci-gnu-payload/authentication-receipt.json', 'scripts/ci-gnu-payload/base-inventory.json', 'scripts/ci-gnu-payload/debian-archive-keyring.gpg', 'scripts/ci-gnu-payload/extract_static.py', 'scripts/ci-gnu-payload/helper-inventory.json', 'scripts/ci-gnu-payload/packages.json', 'scripts/ci-gnu-payload/payload-inventory.json', 'scripts/ci-gnu-payload/run.py', 'scripts/ci-gnu-payload/verify_auth.py'])
OVERLAY.update(['.github/workflows/wip-gnu-closure.yml', 'docs/wip-gnu-closure-scope.md', 'scripts/ci-gnu-closure/base-inventory.json', 'scripts/ci-gnu-closure/helper-inventory.json', 'scripts/ci-gnu-closure/inspect.py', 'scripts/ci-gnu-closure/run.py'])
OVERLAY.update(['scripts/ci-gnu-closure/providers/archive-inventory.json', 'scripts/ci-gnu-closure/providers/debian-archive-keyring.gpg', 'scripts/ci-gnu-closure/providers/extract.py', 'scripts/ci-gnu-closure/providers/packages.json', 'scripts/ci-gnu-closure/providers/selected.json', 'scripts/ci-gnu-closure/providers/verify_auth.py'])
def sha(p):return hashlib.sha256(p.read_bytes()).hexdigest()
def verify(commit):
 assert sha(C/'base-inventory.json')==BASE_PIN and sha(C/'helper-inventory.json')==HELPERS_PIN
 base=json.loads((C/'base-inventory.json').read_text());helpers=json.loads((C/'helper-inventory.json').read_text())
 for name,h in base.items():
  p=S/name;assert p.is_file() and not p.is_symlink() and sha(p)==h,name
 for name,h in helpers.items():
  p=C/name;assert p.is_file() and not p.is_symlink() and sha(p)==h,name
 tracked=set(filter(None,subprocess.check_output(['git','ls-files','-z'],cwd=S).decode().split('\0')))
 assert tracked==set(base)|OVERLAY
 assert subprocess.check_output(['git','rev-parse','HEAD'],cwd=S,text=True).strip()==commit
 assert not subprocess.check_output(['git','status','--porcelain','--untracked-files=no'],cwd=S,text=True).strip()
def caps():
 for kind,limit in ((resource.RLIMIT_CORE,0),(resource.RLIMIT_AS,1024**3),(resource.RLIMIT_CPU,60),(resource.RLIMIT_FSIZE,2*1024**2)):
  resource.setrlimit(kind,(limit,limit))
def phase(name,command,seconds,E,env):
 proc=None;stop=None;start=time.monotonic();out=E/(name+'.out');err=E/(name+'.err')
 try:
  with out.open('xb') as stdout,err.open('xb') as stderr:
   proc=subprocess.Popen(command,cwd=E,env=env,stdin=subprocess.DEVNULL,stdout=stdout,stderr=stderr,start_new_session=True,close_fds=True,preexec_fn=caps)
   # Observe exit without reaping: the owned leader PID/PGID cannot be
   # recycled before finally signals the group and proc.wait releases it.
   while True:
    if os.waitid(os.P_PID,proc.pid,os.WEXITED|os.WNOHANG|os.WNOWAIT) is not None:break
    if time.monotonic()-start>=seconds:stop='deadline'
    elif out.stat().st_size+err.stat().st_size>1024**2:stop='sampled output'
    elif shutil.disk_usage(E).free<512*1024**2:stop='disk floor'
    if stop:break
    time.sleep(.01)
 finally:
  if proc is not None:
   try:os.killpg(proc.pid,signal.SIGKILL)
   except ProcessLookupError:pass
   proc.wait()
 elapsed=time.monotonic()-start
 if proc.returncode or stop or elapsed>seconds or out.stat().st_size+err.stat().st_size>1024**2 or shutil.disk_usage(E).free<512*1024**2:
  print(json.dumps({'phase':name,'exit':proc.returncode,'stop':stop,'stdout_tail':out.read_bytes()[-2000:].decode(errors='replace'),'stderr_tail':err.read_bytes()[-4000:].decode(errors='replace')}),flush=True)
  raise RuntimeError('synthetic phase failed; disposable teardown required')
 return out.read_text(),err.read_text()
def main():
 signal.signal(signal.SIGCHLD,signal.SIG_DFL)
 assert hasattr(os,'waitid') and hasattr(os,'WNOWAIT'),'unreaped leader observation required'
 assert os.environ.get('GITHUB_ACTIONS')=='true' and os.environ.get('GITHUB_REPOSITORY')=='philocalyst/rune'
 assert os.environ.get('GITHUB_EVENT_NAME')=='push' and os.environ.get('GITHUB_REF')=='refs/heads/work/rune-backtrace-untested-20261117'
 commit=os.environ['EXPECTED_COMMIT'];assert re.fullmatch('[0-9a-f]{40}',commit) and commit==os.environ['GITHUB_SHA'];verify(commit)
 tag=os.environ['GITHUB_RUN_ID']+'-'+os.environ['GITHUB_RUN_ATTEMPT'];assert re.fullmatch('[0-9]+-[0-9]+',tag)
 E=pathlib.Path(os.environ['RUNNER_TEMP']).resolve()/('rune-confinement-ci-'+tag);E.mkdir(mode=0o700,exist_ok=False)
 for n in ('home','tmp','config','cache','data'):(E/n).mkdir(mode=0o700)
 env={'PATH':'/usr/bin:/bin','HOME':str(E/'home'),'TMPDIR':str(E/'tmp'),'XDG_CONFIG_HOME':str(E/'config'),'XDG_CACHE_HOME':str(E/'cache'),'XDG_DATA_HOME':str(E/'data'),'LANG':'C','LC_ALL':'C','TZ':'UTC','PYTHONDONTWRITEBYTECODE':'1'}
 assert shutil.disk_usage(E).free>=1024**3
 compiler=pathlib.Path('/usr/bin/cc').resolve();python=pathlib.Path('/usr/bin/python3').resolve();timeout=pathlib.Path('/usr/bin/timeout').resolve()
 tools={}
 for name,path in (('cc',compiler),('python',python),('timeout',timeout)):
  assert path.is_file();version,_=phase(name+'-version',[str(path),'--version'],5,E,env);tools[name]={'path':str(path),'sha256':sha(path),'version':version[:16384]}
 assert 'GNU coreutils' in tools['timeout']['version']
 target,_=phase('cc-target',[str(compiler),'-dumpmachine'],5,E,env);assert target.strip()=='x86_64-linux-gnu'
 headers=[pathlib.Path(n) for n in ['/usr/include/linux/landlock.h','/usr/include/linux/seccomp.h','/usr/include/linux/filter.h','/usr/include/linux/audit.h','/usr/include/x86_64-linux-gnu/asm/unistd_64.h']]
 assert all(p.is_file() for p in headers);header_pins={str(p):sha(p) for p in headers}
 pins={n:sha(S/n) for n in OVERLAY}
 binary=E/'probe';command=[str(compiler),'-std=c11','-O2','-Wall','-Wextra',str(C/'probe.c'),'-o',str(binary)]
 phase('compile',command,60,E,env)
 assert binary.is_file() and not binary.is_symlink() and binary.open('rb').read(4)==b'\x7fELF';bp=sha(binary);verify(commit)
 provenance={'commit':commit,'compile_command':command,'tools':tools,'probe_sha256':bp,'source_sha256':sha(C/'probe.c'),'base_pin':BASE_PIN,'helpers_pin':HELPERS_PIN,'compiler_target':target.strip(),'policy_header_sha256':header_pins,'image':os.environ.get('ImageVersion'),'scope':'synthetic16 only'}
 (E/'compile-provenance.json').write_text(json.dumps(provenance,indent=2)+'\n')
 # Independent watchdog: TERM37s then KILL after3s; controller fallback43s.
 # Controller internally budgets30s and uses reviewed subreaper cleanup.
 phase('confinement',[str(timeout),'--signal=TERM','--kill-after=3s','37s',str(python),str(C/'controller.py'),'--probe',str(binary),'--probe-sha256',bp],43,E,env)
 candidates=list((E/'tmp').glob('rune-confinement-*'));assert len(candidates)==1
 directory=candidates[0];assert directory.is_dir() and not directory.is_symlink()
 p=directory/'verified-result.json';assert p.is_file() and not p.is_symlink();result=json.loads(p.read_text())
 assert result['synthetic_confinement_only'] is True and result['probe_sha256']==bp
 assert result['source_sha256']=={n:sha(C/n) for n in ('probe.c','controller.py','supervisor.py')}
 assert [x['case'] for x in result['cases']]==list(CASES)
 assert all(x['lifecycle']['ok'] is True and x['lifecycle']['empty'] is True and x['lifecycle']['root_status']==0 and x['parent_probe']['passed'] is True for x in result['cases'])
 assert all(result[n] is False for n in ('gnu_tested','rune_tested','doom_tested'))
 assert sha(binary)==bp and pins=={n:sha(S/n) for n in OVERLAY};verify(commit)
 assert header_pins=={str(p):sha(p) for p in headers}
 assert all(sha(pathlib.Path(t['path']))==t['sha256'] for t in tools.values())
 summary={'synthetic_confinement_passed':True,'commit':commit,'cases':list(CASES),'probe_sha256':bp,'tools':tools,'helper_inventory_sha256':HELPERS_PIN,'gnu_tested':False,'rune_tested':False,'doom_tested':False,'old_local_process':'UNKNOWN/unaffected'}
 (E/'qualified-result.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps(summary),flush=True)
 if os.environ.get('GITHUB_STEP_SUMMARY'):pathlib.Path(os.environ['GITHUB_STEP_SUMMARY']).write_text('Synthetic confinement16 only passed at '+commit+'. No GNU/Rune/Doom execution.\n')
if __name__=='__main__':main()
