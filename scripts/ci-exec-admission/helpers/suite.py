#!/usr/bin/env python3
"""Fixed hosted synthetic exec admission only. Source-only until reviewed.
External hard watchdog <=450s and disposable runner teardown remain mandatory.
"""
import hashlib,importlib.util,json,os,pathlib,re,resource,shutil,signal,stat,sys,time
sys.dont_write_bytecode=True
HERE=pathlib.Path(__file__).resolve().parent
SUPERVISOR='7555676495d56228986f0bd2cbdde0e62a566306937300dac691dbe70d2728a0'
MODES=['positive','read-denied','write-denied','truncate-denied','exec-denied','socket-inet','socket-inet6','socket-unix','socketpair','io-uring','ptrace','pidfd-getfd','inherited-fds','symlink-read','invalid-policy']
def sha(p):return hashlib.sha256(p.read_bytes()).hexdigest()
def need(ok,msg):
 if not ok:raise RuntimeError(msg)
def save(p,v):
 with p.open('x') as f:json.dump(v,f,indent=2);f.write('\n')
 p.chmod(0o400)
def main():
 need(len(sys.argv)==2,'fresh existing empty suite directory')
 need(os.environ.get('GITHUB_ACTIONS')=='true' and os.environ.get('GITHUB_REPOSITORY')=='philocalyst/rune' and os.environ.get('GITHUB_EVENT_NAME')=='push' and os.environ.get('GITHUB_REF')=='refs/heads/work/rune-backtrace-untested-20261117','hosted WIP only')
 root=pathlib.Path(sys.argv[1]);need(root.is_absolute() and root.resolve()==root and root.is_dir() and not list(root.iterdir()),'fresh canonical root')
 st=root.stat();need(st.st_uid==os.getuid() and stat.S_IMODE(st.st_mode)==0o700,'private root');need(shutil.disk_usage(root).free>=1024**3,'disk admission')
 # Wrapper independently pins manifest itself and exact commit before invoking.
 manifest=json.loads((HERE/'manifest.json').read_text())
 def verify_sources():
  need({p.name for p in HERE.iterdir()}==set(manifest)|{'manifest.json'},'unexpected source siblings')
  for name,digest in manifest.items():
   p=HERE/name;need(p.is_file() and not p.is_symlink() and sha(p)==digest,'source changed')
 verify_sources();need(sha(HERE/'supervisor.py')==SUPERVISOR,'supervisor pin')
 spec=importlib.util.spec_from_file_location('fixed_supervisor',HERE/'supervisor.py');supervisor=importlib.util.module_from_spec(spec);spec.loader.exec_module(supervisor)
 cancelled=[]
 def request(signum,_frame):cancelled.append(signum)
 signal.signal(signal.SIGTERM,request);signal.signal(signal.SIGINT,request);signal.signal(signal.SIGCHLD,signal.SIG_DFL)
 for kind,limit in [(resource.RLIMIT_CORE,0),(resource.RLIMIT_AS,1024**3),(resource.RLIMIT_CPU,180),(resource.RLIMIT_FSIZE,2*1024**2)]:resource.setrlimit(kind,(limit,limit))
 for name in ['home','tmp','logs']:(root/name).mkdir(mode=0o700)
 os.environ.clear();os.environ.update({'PATH':'/usr/bin:/bin','HOME':str(root/'home'),'TMPDIR':str(root/'tmp'),'LANG':'C','LC_ALL':'C','TZ':'UTC','PYTHONDONTWRITEBYTECODE':'1','DEBUGINFOD_URLS':''})
 tools={}
 for name in ['cc','python3','readelf']:
  alias=pathlib.Path('/usr/bin')/name;canonical=alias.resolve();st=canonical.stat();need(stat.S_ISREG(st.st_mode) and st.st_uid==0 and not st.st_mode&0o022,'untrusted host tool')
  tools[name]={'alias':str(alias),'path':str(canonical),'sha256':sha(canonical),'device':st.st_dev,'inode':st.st_ino}
 deadline=time.monotonic()+420;sequence=0
 def phase(label,command,seconds):
  nonlocal sequence
  need(not cancelled and time.monotonic()<deadline,'cancelled/deadline');sequence+=1
  out=root/'logs'/f'{sequence:03}-{label}.out';err=root/'logs'/f'{sequence:03}-{label}.err'
  saved=[os.dup(1),os.dup(2)]
  try:
   with out.open('xb') as o,err.open('xb') as e:
    os.dup2(o.fileno(),1);os.dup2(e.fileno(),2)
    result=supervisor.supervise(command,min(seconds,max(.1,deadline-time.monotonic()-2)),2,cancellation_state=cancelled)
  finally:
   os.dup2(saved[0],1);os.dup2(saved[1],2)
   for fd in saved:os.close(fd)
  save(root/'logs'/f'{sequence:03}-{label}.json',result)
  need(not cancelled and time.monotonic()<deadline and result.get('ok') is True and result.get('empty') is True,'phase failed: '+label)
  need(out.stat().st_size+err.stat().st_size<=1024**2 and shutil.disk_usage(root).free>=512*1024**2,'output/disk bound')
  return out.read_text(),err.read_text()
 for name in tools:tools[name]['version']=phase(name+'-version',[tools[name]['path'],'--version'],5)[0][:4096]
 need(phase('cc-target',[tools['cc']['path'],'-dumpmachine'],5)[0].strip()=='x86_64-linux-gnu','compiler target')
 compiler_helpers={}
 for name in ['cc1','as','ld']:
  text,err=phase('cc-helper-'+name,[tools['cc']['path'],'-print-prog-name='+name],5);need(not err,'compiler helper diagnostics')
  value=text.strip();candidate=pathlib.Path(value) if pathlib.Path(value).is_absolute() else pathlib.Path('/usr/bin')/value
  p=candidate.resolve();st=p.stat();need(p.is_relative_to('/usr') and stat.S_ISREG(st.st_mode) and st.st_uid==0 and not st.st_mode&0o022,'compiler helper provenance');compiler_helpers[name]={'path':str(p),'sha256':sha(p)}
 headers={}
 for name in ['linux/audit.h','linux/filter.h','linux/landlock.h','linux/seccomp.h','x86_64-linux-gnu/asm/prctl.h','x86_64-linux-gnu/asm/unistd_64.h']:
  p=pathlib.Path('/usr/include')/name;need(p.is_file(),'policy header missing');headers[str(p)]=sha(p)
 def inspect(binary,interpreter):
  text,err=phase('readelf',[tools['readelf']['path'],'-W','-h','-l','-d',str(binary)],5)
  need(not err and re.search(r'Class:\s+ELF64\b',text) and re.search(r'Machine:\s+Advanced Micro Devices X86-64',text),'ELF ABI')
  needed=re.findall(r'\(NEEDED\).*?\[([^\]]+)\]',text);need(needed==['libc.so.6'],'unexpected target library')
  names=re.findall(r'\[Requesting program interpreter: ([^\]]+)\]',text);need(len(names)==1 and str(pathlib.Path(names[0]).resolve())==interpreter,'matched interpreter')
  need(not re.search(r'\((?:RPATH|RUNPATH|AUDIT|DEPAUDIT|FILTER|AUXILIARY)\)',text),'unexpected loader search tag')
  return {'interpreter':interpreter,'needed':needed,'readelf_sha256':hashlib.sha256(text.encode()).hexdigest()}
 receipts=[]
 for mode in MODES:
  verify_sources();case=root/mode;case.mkdir(mode=0o700)
  phase('prepare',[tools['python3']['path'],'-I',str(HERE/'prepare_config.py'),str(case)],5)
  cfg=json.loads((case/'config.json').read_text());need(cfg['root']==str(case) and cfg['config_sha256']==sha(case/'config.h'),'generated config mismatch')
  need(cfg['source_hashes']=={n:sha(HERE/n) for n in ['launcher.c','target.c']},'generated source mismatch')
  for r in cfg['runtime']:
   p=pathlib.Path(r['path']);need(p.resolve()==p and sha(p)==r['sha256'] and (p.stat().st_dev,p.stat().st_ino)==(r['device'],r['inode']),'runtime changed')
  for name in ['target','launcher']:
   phase('compile-'+name,[tools['cc']['path'],'-std=c11','-O2','-Wall','-Wextra','-I',str(case),str(HERE/(name+'.c')),'-o',str(case/name)],20)
   (case/name).chmod(0o500)
  # No invocation of either generated ELF until static identity checks finish.
  target_info=inspect(case/'target',cfg['runtime'][0]['path']);inspect(case/'launcher',cfg['runtime'][0]['path'])
  with (case/'target').open('rb') as src,(case/'denied-target').open('xb') as dst:shutil.copyfileobj(src,dst)
  (case/'denied-target').chmod(0o500)
  build={'root':str(case),'config_sha256':sha(case/'config.h'),'files':{n:sha(case/n) for n in ['target','launcher','denied-target']},**target_info,'source_hashes':cfg['source_hashes'],'tools':tools}
  save(case/'build.json',build)
  out,err=phase('case-'+mode,[tools['python3']['path'],'-I',str(HERE/'case.py'),str(case),mode],5)
  need(not err,'unexpected case diagnostics');r=json.loads(out);need(r['passed'] is True and r['gnu_executed'] is False and r['mode']==mode,'case receipt');receipts.append(r)
  expected_siblings={'allowed','outside','home','tmp','config.h','config.json','target','launcher','denied-target','build.json',mode+'.out',mode+'.err'}
  need({p.name for p in case.iterdir()}==expected_siblings,'unexpected case siblings')
  need(not list((case/'home').iterdir()) and not list((case/'tmp').iterdir()),'unexpected home/tmp output')
  need(sha(case/'build.json')==hashlib.sha256((json.dumps(build,indent=2)+'\n').encode()).hexdigest(),'build receipt changed')
  for n,h in build['files'].items():need(sha(case/n)==h,'ELF changed')
  for runtime in cfg['runtime']:need(sha(pathlib.Path(runtime['path']))==runtime['sha256'],'runtime changed')
 verify_sources()
 for t in compiler_helpers.values():need(sha(pathlib.Path(t['path']))==t['sha256'],'compiler helper changed')
 for name,digest in headers.items():need(sha(pathlib.Path(name))==digest,'policy header changed')
 for t in tools.values():
  p=pathlib.Path(t['path']);need(pathlib.Path(t['alias']).resolve()==p and sha(p)==t['sha256'] and (p.stat().st_dev,p.stat().st_ino)==(t['device'],t['inode']),'tool changed')
 need({p.name for p in root.iterdir()}==set(MODES)|{'home','tmp','logs'},'unexpected suite siblings')
 need(not list((root/'home').iterdir()) and not list((root/'tmp').iterdir()),'unexpected suite home/tmp')
 need(len(list((root/'logs').iterdir()))==sequence*3,'unexpected phase logs')
 need([r['mode'] for r in receipts]==MODES and not cancelled and time.monotonic()<deadline,'suite incomplete')
 # All children ECHILD before final signal transition; late cancellation exits.
 signal.signal(signal.SIGTERM,signal.SIG_DFL);signal.signal(signal.SIGINT,signal.SIG_DFL)
 need(not cancelled,'late cancellation')
 result={'fixed_postexec_mechanism_cases':receipts,'count':15,'gnu_executed':False,'gnu_runtime_qualified':False,'doom_executed':False,'tools':tools,'compiler_helpers':compiler_helpers,'policy_headers':headers,'old_local_process':'UNKNOWN/unaffected'}
 save(root/'result.json',result);print(json.dumps(result),flush=True)
if __name__=='__main__':main()
