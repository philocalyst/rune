#!/usr/bin/env python3
"""Exact-commit hosted fixed synthetic exec preflight; no GNU or Doom execution.
Fixed five-group source reviewed admission; never an arbitrary command runner.
"""
import hashlib,importlib.util,json,os,pathlib,re,stat,subprocess,sys
sys.dont_write_bytecode=True
S=pathlib.Path.cwd().resolve();C=S/'scripts/ci-exec-admission';H=C/'helpers'
BASE_PIN='b5d0c4be7ca318824d104af1f020a8f3f994f6180e31a2aeaacdb477aaadea71'
HELPERS_PIN='acb26afbc19bc56893c5a3d720d10c8c613157f67c62cbdf4cad39a527d340de'
GROUP_NAMES=['baseline','scalar_exec','scalar_other','process_alias','metadata']
GROUP_COUNTS=[15,8,8,9,9]
def expected(mode):
 if mode=='hardlink-preadmission':return ('',78)
 if mode=='invalid-policy':return ('',77)
 if mode=='inherited-fds':return ('a1a2b0b1b2',0)
 ordinary={'positive','read-denied','write-denied','truncate-denied','exec-denied','symlink-read','symlink-write','symlink-truncate'}
 return ('a1a2a3',0 if mode in ordinary else -31)
def need(ok,msg):
 if not ok:raise RuntimeError(msg)
def sha(p):return hashlib.sha256(p.read_bytes()).hexdigest()
def verify(commit):
 need(re.fullmatch('[0-9a-f]{64}',BASE_PIN) is not None,'unfrozen publication')
 need(sha(C/'base-inventory.json')==BASE_PIN and sha(H/'manifest.json')==HELPERS_PIN,'manifest pin')
 base=json.loads((C/'base-inventory.json').read_text());helpers=json.loads((H/'manifest.json').read_text())
 for root,items in [(S,base),(H,helpers)]:
  for n,digest in items.items():
   p=root/n;need(p.is_file() and not p.is_symlink() and sha(p)==digest,'source identity')
 need({p.name for p in H.iterdir()}==set(helpers)|{'manifest.json'},'unexpected helper siblings')
 overlay={'.github/workflows/wip-exec-admission.yml','docs/wip-exec-admission-scope.md','scripts/ci-exec-admission/run.py','scripts/ci-exec-admission/base-inventory.json','scripts/ci-exec-admission/helpers/manifest.json'}|{'scripts/ci-exec-admission/helpers/'+n for n in helpers}
 actual=set(filter(None,subprocess.check_output(['/usr/bin/git','ls-files','-z'],cwd=S).decode().split('\0')))
 need(actual==set(base)|overlay,'tracked fileset')
 need(subprocess.check_output(['/usr/bin/git','rev-parse','HEAD'],cwd=S,text=True).strip()==commit,'commit mismatch')
 need(not subprocess.check_output(['/usr/bin/git','status','--porcelain','--untracked-files=all'],cwd=S,text=True).strip(),'dirty/unexpected checkout')
def main():
 need(os.environ.get('GITHUB_ACTIONS')=='true' and os.environ.get('GITHUB_REPOSITORY')=='philocalyst/rune' and os.environ.get('GITHUB_EVENT_NAME')=='push' and os.environ.get('GITHUB_REF')=='refs/heads/work/rune-backtrace-untested-20261117','exact WIP push only')
 group=os.environ['ADMISSION_GROUP'];need(group in GROUP_NAMES,'fixed group only')
 commit=os.environ['EXPECTED_COMMIT'];need(re.fullmatch('[0-9a-f]{40}',commit) is not None and commit==os.environ['GITHUB_SHA'],'event commit');verify(commit)
 groups=json.loads((H/'cases.json').read_text());need(list(groups)==GROUP_NAMES and [len(groups[x]) for x in GROUP_NAMES]==GROUP_COUNTS and len(set(sum(groups.values(),[])))==49,'fixed49 roster')
 MODES=groups[group]
 tag=os.environ['GITHUB_RUN_ID']+'-'+os.environ['GITHUB_RUN_ATTEMPT'];need(re.fullmatch('[0-9]+-[0-9]+',tag) is not None,'run identity')
 root=pathlib.Path(os.environ['RUNNER_TEMP']).resolve()/('rune-exec-admission-'+tag+'-'+group);root.mkdir(mode=0o700)
 need(root.stat().st_uid==os.getuid() and stat.S_IMODE(root.stat().st_mode)==0o700,'private staging')
 # Suite owns fixed trusted builds and direct lifecycle receipts. No genericcmd.
 spec=importlib.util.spec_from_file_location('fixed_exec_suite',H/'suite.py');suite=importlib.util.module_from_spec(spec);spec.loader.exec_module(suite)
 oldargv=sys.argv;sys.argv=[str(H/'suite.py'),str(root),group]
 try:suite.main()
 finally:sys.argv=oldargv
 verify(commit)
 resultpath=root/'result.json';st=resultpath.lstat();need(stat.S_ISREG(st.st_mode) and st.st_uid==os.getuid() and st.st_nlink==1 and stat.S_IMODE(st.st_mode)==0o400,'result identity')
 r=json.loads(resultpath.read_text());need(r['group']==group and r['count']==len(MODES) and [x['mode'] for x in r['fixed_postexec_mechanism_cases']]==MODES,'exact completed case set')
 need(all(x['passed'] is True and x['gnu_executed'] is False for x in r['fixed_postexec_mechanism_cases']),'case outcomes')
 for item in r['fixed_postexec_mechanism_cases']:
  need((item['markers'],item['exit'])==expected(item['mode']),'exact marker/status')
  need([x['fd'] for x in item['injected']]==([100,101,102] if item['mode']=='inherited-fds' else []),'exact injected descriptors')
 need(r['gnu_executed'] is False and r['gnu_runtime_qualified'] is False and r['doom_executed'] is False,'scope flags')
 print(json.dumps({'commit':commit,'group':group,'fixed_postexec_mechanism_cases':len(MODES),'result_sha256':sha(resultpath),'gnu_executed':False,'gnu_runtime_qualified':False,'doom_executed':False,'old_local_process':'UNKNOWN/unaffected'}),flush=True)
if __name__=='__main__':main()
