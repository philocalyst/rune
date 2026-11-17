#!/usr/bin/env python3
"""Same-job authenticated payload then static exact-host ELF diagnostic only."""
import datetime,hashlib,importlib.util,json,os,pathlib,re,stat,subprocess,sys
sys.dont_write_bytecode=True
S=pathlib.Path.cwd().resolve();C=S/'scripts/ci-gnu-closure'
BASE_PIN='6d5c5e93140950aaa4682a44b3424d43d78377bacdc4f5bd248c4ebe6125671c'
HELPERS_PIN='c4acbbeaab711199f12b6a47bda830cfcea79f943a03b65f45bc299f2be56e4f'
OVERLAY=set(['.github/workflows/wip-gnu-closure.yml', 'docs/wip-gnu-closure-scope.md', 'scripts/ci-gnu-closure/base-inventory.json', 'scripts/ci-gnu-closure/helper-inventory.json', 'scripts/ci-gnu-closure/inspect.py', 'scripts/ci-gnu-closure/run.py'])
OVERLAY.add('scripts/ci-exec-admission/helpers/cases.json')
OVERLAY.update(['.github/workflows/wip-exec-admission.yml', 'docs/wip-exec-admission-scope.md', 'scripts/ci-exec-admission/base-inventory.json', 'scripts/ci-exec-admission/helpers/README.md', 'scripts/ci-exec-admission/helpers/case.py', 'scripts/ci-exec-admission/helpers/config.h.example', 'scripts/ci-exec-admission/helpers/launcher.c', 'scripts/ci-exec-admission/helpers/manifest.json', 'scripts/ci-exec-admission/helpers/prepare_config.py', 'scripts/ci-exec-admission/helpers/suite.py', 'scripts/ci-exec-admission/helpers/supervisor.py', 'scripts/ci-exec-admission/helpers/target.c', 'scripts/ci-exec-admission/run.py'])
OVERLAY.update(['scripts/ci-gnu-closure/providers/archive-inventory.json', 'scripts/ci-gnu-closure/providers/debian-archive-keyring.gpg', 'scripts/ci-gnu-closure/providers/extract.py', 'scripts/ci-gnu-closure/providers/packages.json', 'scripts/ci-gnu-closure/providers/selected.json', 'scripts/ci-gnu-closure/providers/verify_auth.py'])
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
 P=C/'providers'
 for n in ('provider-metadata','provider-archives'):(E/n).mkdir(mode=0o700)
 g.download('provider-release','https://deb.debian.org/debian/dists/bookworm/InRelease',E/'provider-metadata/InRelease',151075,'77737fa4b34f2693e982cc9ee35736816c35a7778fc2d326cc1bbf5b301fe1aa',E,env)
 g.download('provider-index','https://deb.debian.org/debian/dists/bookworm/main/binary-amd64/by-hash/SHA256/9e0b5aabb2465b3d2e7a7fe27f9913846277833f7a2826e7767acccff5b588c5',E/'provider-metadata/Packages.xz',8790396,'9e0b5aabb2465b3d2e7a7fe27f9913846277833f7a2826e7767acccff5b588c5',E,env)
 g.phase('provider-authenticate',['/usr/bin/python3','-I',str(P/'verify_auth.py'),str(E/'provider-metadata'),str(E/'provider-authentication.json')],30,E,env)
 receipt=E/'provider-authentication.json';st=receipt.lstat();assert stat.S_ISREG(st.st_mode) and st.st_uid==os.getuid() and st.st_nlink==1 and stat.S_IMODE(st.st_mode)==0o600
 authentication=json.loads(receipt.read_text());assert authentication['authenticated'] is True and authentication['package_code_executed'] is False and authentication['signed_valid_until'] is None
 assert authentication['inrelease_sha256']=='77737fa4b34f2693e982cc9ee35736816c35a7778fc2d326cc1bbf5b301fe1aa' and authentication['packages_xz_sha256']=='9e0b5aabb2465b3d2e7a7fe27f9913846277833f7a2826e7767acccff5b588c5'
 receipt_pin=sha(receipt)
 for package in json.loads((P/'packages.json').read_text()):g.download('provider-'+package['package'],'https://deb.debian.org/debian/'+package['archive_path'],E/'provider-archives'/package['filename'],package['size'],package['sha256'],E,env)
 g.phase('provider-extract',['/usr/bin/python3','-I',str(P/'extract.py'),str(E/'provider-archives'),str(E/'providers')],40,E,env)
 def check_providers():
  root=E/'providers';st=root.lstat();assert root.resolve()==root and stat.S_ISDIR(st.st_mode) and st.st_uid==os.getuid() and stat.S_IMODE(st.st_mode)==0o700
  selected=json.loads((P/'selected.json').read_text());assert {p.name for p in root.iterdir()}=={v['soname'][0] for v in selected}
  for v in selected:
   p=root/v['soname'][0];st=p.lstat();assert stat.S_ISREG(st.st_mode) and st.st_uid==os.getuid() and st.st_nlink==1 and stat.S_IMODE(st.st_mode)==0o400 and st.st_size==v['size'] and sha(p)==v['sha256']
  now=datetime.datetime.now(datetime.timezone.utc);assert datetime.datetime.fromisoformat(authentication['review_not_before'])<=now<datetime.datetime.fromisoformat(authentication['review_not_after'])
  assert now<datetime.datetime.fromisoformat(json.loads((E/'authentication.json').read_text())['valid_until'])
  assert sha(receipt)==receipt_pin
  for v in json.loads((E/'result.json').read_text())['tools'].values():assert sha(pathlib.Path(v['path']))==v['sha256']
 check_providers();verify(commit)
 g.phase('static-closure',['/usr/bin/python3','-I',str(C/'inspect.py'),str(E/'payload'),str(E/'static-closure'),str(S/'scripts/ci-gnu-payload/payload-inventory.json'),str(E/'providers'),str(P/'selected.json')],120,E,env)
 check_providers()
 p=E/'static-closure/closure.json';r=json.loads(p.read_text());assert r['static_candidate_complete'] is True and r['gnu_executed'] is False and r['runtime_qualified'] is False and r['eln_roots']==149 and r['elf_files']>=150 and len(r['pdumps'])==1
 verify(commit)
 result={'static_candidate_complete':True,'provider_authentication_sha256':receipt_pin,'provider_signed_valid_until':None,'provider_local_review_not_after':authentication['review_not_after'],'commit':commit,'closure_sha256':sha(p),'elf_files':r['elf_files'],'eln_roots':149,'gnu_executed':False,'runtime_qualified':False,'gnu16_tested':False,'doom_tested':False,'image_version':os.environ.get('ImageVersion'),'image_os':os.environ.get('ImageOS')}
 with (E/'static-result.json').open('x') as f:json.dump(result,f,indent=2);f.write('\n')
 print(json.dumps(result),flush=True)
 if os.environ.get('GITHUB_STEP_SUMMARY'):pathlib.Path(os.environ['GITHUB_STEP_SUMMARY']).write_text('Static ELF dependency candidate only; GNU/loader/ELNs were not executed. Runtime/dlopen/NSS/Doom unqualified. Commit '+commit+'\n')
if __name__=='__main__':main()
