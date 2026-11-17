#!/usr/bin/env python3
"""Same-job authenticated payload then static exact-host ELF diagnostic only."""
import hashlib,importlib.util,json,os,pathlib,re,subprocess,sys
sys.dont_write_bytecode=True
S=pathlib.Path.cwd().resolve();C=S/'scripts/ci-gnu-closure'
BASE_PIN='64ae7df31ea37b52a8197c26954285143e2672c45ac5078b438f42f4ff5a46c0'
HELPERS_PIN='10634869bed038c1718141829379a276a04a6e56f56efa62a60198d965956610'
OVERLAY=set(['.github/workflows/wip-gnu-closure.yml', 'docs/wip-gnu-closure-scope.md', 'scripts/ci-gnu-closure/base-inventory.json', 'scripts/ci-gnu-closure/helper-inventory.json', 'scripts/ci-gnu-closure/inspect.py', 'scripts/ci-gnu-closure/run.py'])
def sha(p):
 h=hashlib.sha256()
 with p.open('rb') as f:
  while b:=f.read(1024**2):h.update(b)
 return h.hexdigest()
def verify(commit):
 assert sha(C/'base-inventory.json')==BASE_PIN and sha(C/'helper-inventory.json')==HELPERS_PIN
 base=json.loads((C/'base-inventory.json').read_text());helpers=json.loads((C/'helper-inventory.json').read_text())
 for root,items in ((S,base),(C,helpers)):
  for n,h in items.items():p=root/n;assert p.is_file() and not p.is_symlink() and sha(p)==h,n
 assert set(filter(None,subprocess.check_output(['git','ls-files','-z']).decode().split('\0')))==set(base)|OVERLAY
 assert subprocess.check_output(['git','rev-parse','HEAD'],text=True).strip()==commit
 assert not subprocess.check_output(['git','status','--porcelain','--untracked-files=no'],text=True).strip()
def main():
 assert os.environ.get('GITHUB_ACTIONS')=='true' and os.environ.get('GITHUB_REPOSITORY')=='philocalyst/rune'
 assert os.environ.get('GITHUB_EVENT_NAME')=='push' and os.environ.get('GITHUB_REF')=='refs/heads/work/rune-backtrace-untested-20261117'
 commit=os.environ['EXPECTED_COMMIT'];assert re.fullmatch('[0-9a-f]{40}',commit) and commit==os.environ['GITHUB_SHA'];verify(commit)
 spec=importlib.util.spec_from_file_location('payload',S/'scripts/ci-gnu-payload/run.py');g=importlib.util.module_from_spec(spec);spec.loader.exec_module(g)
 E=g.main() # Same verified data-only stage; never execute an extracted file.
 env={'PATH':'/usr/bin:/bin','HOME':str(E/'home'),'TMPDIR':str(E/'tmp'),'LANG':'C','LC_ALL':'C','TZ':'UTC','PYTHONDONTWRITEBYTECODE':'1','DEBUGINFOD_URLS':''}
 g.phase('static-closure',['/usr/bin/python3','-I',str(C/'inspect.py'),str(E/'payload'),str(E/'static-closure'),str(S/'scripts/ci-gnu-payload/payload-inventory.json')],120,E,env)
 p=E/'static-closure/closure.json';r=json.loads(p.read_text());assert r['static_candidate_complete'] is True and r['gnu_executed'] is False and r['runtime_qualified'] is False and r['eln_roots']==149 and r['elf_files']>=150 and len(r['pdumps'])==1
 verify(commit)
 result={'static_candidate_complete':True,'commit':commit,'closure_sha256':sha(p),'elf_files':r['elf_files'],'eln_roots':149,'gnu_executed':False,'runtime_qualified':False,'gnu16_tested':False,'doom_tested':False,'image_version':os.environ.get('ImageVersion'),'image_os':os.environ.get('ImageOS')}
 with (E/'static-result.json').open('x') as f:json.dump(result,f,indent=2);f.write('\n')
 print(json.dumps(result),flush=True)
 if os.environ.get('GITHUB_STEP_SUMMARY'):pathlib.Path(os.environ['GITHUB_STEP_SUMMARY']).write_text('Static ELF dependency candidate only; GNU/loader/ELNs were not executed. Runtime/dlopen/NSS/Doom unqualified. Commit '+commit+'\n')
if __name__=='__main__':main()
