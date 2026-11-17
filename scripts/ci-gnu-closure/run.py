#!/usr/bin/env python3
"""Same-job authenticated payload then static exact-host ELF diagnostic only."""
import datetime,hashlib,importlib.util,json,os,pathlib,re,stat,subprocess,sys
sys.dont_write_bytecode=True
S=pathlib.Path.cwd().resolve();C=S/'scripts/ci-gnu-closure'
BASE_PIN='cfaaafac7e54dfc6b807fc73f9b30b47c70275bb87b48f402860968a3408045a'
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
 def export_evidence():
  import time
  deadline=time.monotonic()+20
  tag=os.environ['GITHUB_RUN_ID']+'-'+os.environ['GITHUB_RUN_ATTEMPT'];assert re.fullmatch('[0-9]+-[0-9]+',tag)
  temp=pathlib.Path(os.environ['RUNNER_TEMP']).resolve();destination=temp/('rune-closure-evidence-'+tag)
  destination.mkdir(mode=0o700);assert destination.resolve()==destination
  sources={'closure.json':E/'static-closure/closure.json','static-result.json':E/'static-result.json','payload-result.json':E/'result.json','authentication.json':E/'authentication.json','provider-authentication.json':E/'provider-authentication.json'}
  def bounded_hash(path):
   fd=os.open(path,os.O_RDONLY|os.O_NOFOLLOW|os.O_CLOEXEC|os.O_NONBLOCK)
   try:
    st=os.fstat(fd);assert stat.S_ISREG(st.st_mode) and st.st_size<=32*1024**2
    h=hashlib.sha256();size=0
    while True:
     assert time.monotonic()<deadline
     data=os.read(fd,65536)
     if not data:break
     size+=len(data);assert size<=32*1024**2;h.update(data)
    assert size==st.st_size;return h.hexdigest()
   finally:os.close(fd)
  before={};inventory={};total=0
  for name,source in sources.items():
   assert time.monotonic()<deadline and source.resolve()==source and source.is_relative_to(E)
   fd=os.open(source,os.O_RDONLY|os.O_NOFOLLOW|os.O_CLOEXEC|os.O_NONBLOCK)
   try:
    st=os.fstat(fd);assert stat.S_ISREG(st.st_mode) and st.st_uid==os.getuid() and st.st_nlink==1 and st.st_size<=32*1024**2
    identity=(st.st_dev,st.st_ino,st.st_size,st.st_mode,st.st_uid,st.st_nlink,st.st_mtime_ns,st.st_ctime_ns);before[name]=identity
    digest=hashlib.sha256();size=0
    with (destination/name).open('xb') as out:
     while True:
      assert time.monotonic()<deadline
      chunk=os.read(fd,65536)
      if not chunk:break
      size+=len(chunk);total+=len(chunk);assert size<=32*1024**2 and total<=40*1024**2
      digest.update(chunk);out.write(chunk)
    now=os.fstat(fd);assert identity==(now.st_dev,now.st_ino,now.st_size,now.st_mode,now.st_uid,now.st_nlink,now.st_mtime_ns,now.st_ctime_ns) and size==st.st_size
    (destination/name).chmod(0o400);inventory[name]={'size':size,'sha256':digest.hexdigest()}
   finally:os.close(fd)
  assert inventory['closure.json']['sha256']==result['closure_sha256'] and inventory['provider-authentication.json']['sha256']==receipt_pin
  provenance={'commit':commit,'run_id':os.environ['GITHUB_RUN_ID'],'run_attempt':os.environ['GITHUB_RUN_ATTEMPT'],'image_os':os.environ.get('ImageOS'),'image_version':os.environ.get('ImageVersion'),'closure_helper_manifest_sha256':sha(C/'helper-inventory.json'),'payload_manifest_sha256':sha(S/'scripts/ci-gnu-payload/payload-inventory.json'),'provider_selection_sha256':sha(P/'selected.json'),'scope':'same-job data-only static closure evidence; not runtime admission','gnu_executed':False,'runtime_qualified':False,'includes_package_or_elf_bytes':False}
  with (destination/'provenance.json').open('x') as out:json.dump(provenance,out,indent=2);out.write('\n')
  (destination/'provenance.json').chmod(0o400);inventory['provenance.json']={'size':(destination/'provenance.json').stat().st_size,'sha256':sha(destination/'provenance.json')}
  for name,source in sources.items():
   assert time.monotonic()<deadline
   now=source.lstat();assert before[name]==(now.st_dev,now.st_ino,now.st_size,now.st_mode,now.st_uid,now.st_nlink,now.st_mtime_ns,now.st_ctime_ns)
   assert bounded_hash(source)==inventory[name]['sha256']==bounded_hash(destination/name)
  with (destination/'manifest.json').open('x') as out:json.dump({'commit':commit,'run_id':os.environ['GITHUB_RUN_ID'],'run_attempt':os.environ['GITHUB_RUN_ATTEMPT'],'files':inventory,'gnu_executed':False,'runtime_qualified':False},out,indent=2);out.write('\n')
  (destination/'manifest.json').chmod(0o400)
  assert {x.name for x in destination.iterdir()}==set(inventory)|{'manifest.json'} and time.monotonic()<deadline
  check_providers();verify(commit)
  print(json.dumps({'closure_evidence_directory':str(destination),'manifest_sha256':sha(destination/'manifest.json'),'files':len(inventory)+1,'copied_source_bytes':total,'gnu_executed':False,'runtime_qualified':False}),flush=True)
 export_evidence()
 if os.environ.get('GITHUB_STEP_SUMMARY'):pathlib.Path(os.environ['GITHUB_STEP_SUMMARY']).write_text('Static ELF dependency candidate only; GNU/loader/ELNs were not executed. Runtime/dlopen/NSS/Doom unqualified. Commit '+commit+'\n')
if __name__=='__main__':main()
