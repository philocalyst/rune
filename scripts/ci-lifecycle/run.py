#!/usr/bin/env python3
"""Synthetic lifecycle only. No GNU/Rune/build command or isolation claim."""
import hashlib
import json
import os
import pathlib
import re
import resource
import shutil
import subprocess
import sys

S=pathlib.Path.cwd().resolve()
C=S/'scripts/ci-lifecycle'
BASE_PIN='c8535b36a2d11a1775960cea80bc5029e5e7f088344136f0100db80b42101932'
HELPERS_PIN='8e1effaaa784d79764bec4594775f1bf0c01d05e418e26aab8264366e6f9fb4a'
OVERLAY={'.github/workflows/wip-lifecycle.yml','docs/wip-lifecycle-scope.md'}|{'scripts/ci-lifecycle/'+n for n in ['run.py','supervisor.py','preflight.py','fixture.py','fault_adapter.py','base-inventory.json','lifecycle-inventory.json']}
OVERLAY.add('scripts/ci-exec-admission/helpers/cases.json')
OVERLAY.update(['.github/workflows/wip-exec-admission.yml', 'docs/wip-exec-admission-scope.md', 'scripts/ci-exec-admission/base-inventory.json', 'scripts/ci-exec-admission/helpers/README.md', 'scripts/ci-exec-admission/helpers/case.py', 'scripts/ci-exec-admission/helpers/config.h.example', 'scripts/ci-exec-admission/helpers/launcher.c', 'scripts/ci-exec-admission/helpers/manifest.json', 'scripts/ci-exec-admission/helpers/prepare_config.py', 'scripts/ci-exec-admission/helpers/suite.py', 'scripts/ci-exec-admission/helpers/supervisor.py', 'scripts/ci-exec-admission/helpers/target.c', 'scripts/ci-exec-admission/run.py'])
OVERLAY.update(['.github/workflows/wip-confinement.yml', 'docs/wip-confinement-scope.md', 'scripts/ci-confinement/run.py', 'scripts/ci-confinement/probe.c', 'scripts/ci-confinement/controller.py', 'scripts/ci-confinement/supervisor.py', 'scripts/ci-confinement/base-inventory.json', 'scripts/ci-confinement/helper-inventory.json'])
EXPECTED_CASES=['normal','nonzero','group','doublefork','detached','crash','latefork','cancel','admission','cleanup']
OVERLAY.update(['.github/workflows/wip-gnu-payload.yml', 'docs/wip-gnu-payload-scope.md', 'scripts/ci-gnu-payload/authentication-receipt.json', 'scripts/ci-gnu-payload/base-inventory.json', 'scripts/ci-gnu-payload/debian-archive-keyring.gpg', 'scripts/ci-gnu-payload/extract_static.py', 'scripts/ci-gnu-payload/helper-inventory.json', 'scripts/ci-gnu-payload/packages.json', 'scripts/ci-gnu-payload/payload-inventory.json', 'scripts/ci-gnu-payload/run.py', 'scripts/ci-gnu-payload/verify_auth.py'])
OVERLAY.update(['.github/workflows/wip-gnu-closure.yml', 'docs/wip-gnu-closure-scope.md', 'scripts/ci-gnu-closure/base-inventory.json', 'scripts/ci-gnu-closure/helper-inventory.json', 'scripts/ci-gnu-closure/inspect.py', 'scripts/ci-gnu-closure/run.py'])
OVERLAY.update(['scripts/ci-gnu-closure/providers/archive-inventory.json', 'scripts/ci-gnu-closure/providers/debian-archive-keyring.gpg', 'scripts/ci-gnu-closure/providers/extract.py', 'scripts/ci-gnu-closure/providers/packages.json', 'scripts/ci-gnu-closure/providers/selected.json', 'scripts/ci-gnu-closure/providers/verify_auth.py'])
def sha(path):return hashlib.sha256(path.read_bytes()).hexdigest()
def verify(commit):
    assert sha(C/'base-inventory.json')==BASE_PIN
    assert sha(C/'lifecycle-inventory.json')==HELPERS_PIN
    base=json.loads((C/'base-inventory.json').read_text())
    helpers=json.loads((C/'lifecycle-inventory.json').read_text())
    for name,pin in base.items():
        p=S/name;assert p.is_file() and not p.is_symlink() and sha(p)==pin,name
    for name,pin in helpers.items():
        p=C/name;assert p.is_file() and not p.is_symlink() and sha(p)==pin,name
    tracked=set(filter(None,subprocess.check_output(['git','ls-files','-z'],cwd=S).decode().split('\0')))
    assert tracked==set(base)|OVERLAY,'tracked file set changed'
    assert subprocess.check_output(['git','rev-parse','HEAD'],cwd=S,text=True).strip()==commit
    assert not subprocess.check_output(['git','status','--porcelain','--untracked-files=no'],cwd=S,text=True).strip()

def main():
    assert os.environ.get('GITHUB_ACTIONS')=='true'
    assert os.environ.get('GITHUB_REPOSITORY')=='philocalyst/rune'
    assert os.environ.get('GITHUB_EVENT_NAME')=='push'
    assert os.environ.get('GITHUB_REF')=='refs/heads/work/rune-backtrace-untested-20261117'
    commit=os.environ['EXPECTED_COMMIT']
    assert re.fullmatch('[0-9a-f]{40}',commit) and commit==os.environ['GITHUB_SHA']
    verify(commit)
    # Saved before caps, but no child receives the ambient environment.
    runner_temp=pathlib.Path(os.environ['RUNNER_TEMP']).resolve()
    tag=os.environ['GITHUB_RUN_ID']+'-'+os.environ['GITHUB_RUN_ATTEMPT']
    assert re.fullmatch('[0-9]+-[0-9]+',tag)
    E=runner_temp/('rune-lifecycle-'+tag);E.mkdir(mode=0o700,exist_ok=False)
    for name in ('home','tmp','config','cache','data'):(E/name).mkdir(mode=0o700)
    env={'PATH':'/usr/bin:/bin','HOME':str(E/'home'),'TMPDIR':str(E/'tmp'),
         'XDG_CONFIG_HOME':str(E/'config'),'XDG_CACHE_HOME':str(E/'cache'),
         'XDG_DATA_HOME':str(E/'data'),'LANG':'C.UTF-8','LC_ALL':'C.UTF-8',
         'TZ':'UTC','PYTHONDONTWRITEBYTECODE':'1'}
    python=pathlib.Path('/usr/bin/python3').resolve()
    timeout=pathlib.Path('/usr/bin/timeout').resolve()
    assert python.is_file() and timeout.is_file()
    assert shutil.disk_usage(E).free>=1024**3
    for kind,limit in [(resource.RLIMIT_CORE,0),(resource.RLIMIT_AS,512*1024**2),
                       (resource.RLIMIT_CPU,30),(resource.RLIMIT_FSIZE,1024**2)]:
        resource.setrlimit(kind,(limit,limit))
    versions={}
    for name,path,args in [('python',python,['--version']),('timeout',timeout,['--version'])]:
        observed=subprocess.run([str(path)]+args,env=env,cwd=E,stdin=subprocess.DEVNULL,
                                stdout=subprocess.PIPE,stderr=subprocess.STDOUT,timeout=3,check=True)
        assert len(observed.stdout)<=16384
        versions[name]={'path':str(path),'sha256':sha(path),'version':observed.stdout.decode()}
    assert 'GNU coreutils' in versions['timeout']['version']
    before={str(p.relative_to(S)):sha(p) for p in [S/n for n in OVERLAY]}
    command=[str(timeout),'--signal=TERM','--kill-after=3s','37s',str(python),str(C/'preflight.py')]
    try:
        with (E/'stdout').open('xb') as out,(E/'stderr').open('xb') as err:
            result=subprocess.run(command,cwd=E,env=env,stdin=subprocess.DEVNULL,
                                  stdout=out,stderr=err,timeout=43,check=False)
    except subprocess.TimeoutExpired:
        # Never launch another case/lane after controller/watchdog failure.
        raise RuntimeError('outer lifecycle watchdog failed; disposable teardown required')
    out=(E/'stdout').read_bytes();err=(E/'stderr').read_bytes()
    if result.returncode or len(out)+len(err)>1024**2:
        print(json.dumps({'qualified':False,'exit':result.returncode,
                          'stdout_tail':out[-2000:].decode(errors='replace'),
                          'stderr_tail':err[-4000:].decode(errors='replace')}),flush=True)
        raise RuntimeError('synthetic lifecycle admission failed')
    candidates=list((E/'tmp').glob('rune-lifecycle-preflight-*'))
    assert len(candidates)==1 and candidates[0].is_dir() and not candidates[0].is_symlink()
    receipt_file=candidates[0]/'result.json'
    assert receipt_file.is_file() and not receipt_file.is_symlink()
    receipt=json.loads(receipt_file.read_text())
    assert receipt['synthetic_only'] is True and receipt['directory']==str(candidates[0])
    assert [r['case'] for r in receipt['results']]==EXPECTED_CASES
    assert all(r['identities_gone'] is True and r['sentinel_alive'] is True for r in receipt['results'])
    assert all(r['supervisor_exit']==(0 if r['case']=='normal' else 1) for r in receipt['results'])
    assert receipt['source_sha256']=={p.name:sha(p) for p in C.glob('*.py')}
    verify(commit)
    assert before=={str(p.relative_to(S)):sha(p) for p in [S/n for n in OVERLAY]}
    summary={'synthetic_lifecycle_passed':True,'commit':commit,'cases':EXPECTED_CASES,
             'base_inventory_sha256':BASE_PIN,'helper_inventory_sha256':HELPERS_PIN,
             'tools':versions,'stdout_sha256':sha(E/'stdout'),'stderr_sha256':sha(E/'stderr'),
             'gnu_tested':False,'doom_tested':False,'old_local_process':'UNKNOWN/unaffected'}
    (E/'verified-result.json').write_text(json.dumps(summary,indent=2)+'\n')
    print(json.dumps(summary),flush=True)
    if os.environ.get('GITHUB_STEP_SUMMARY'):
        pathlib.Path(os.environ['GITHUB_STEP_SUMMARY']).write_text('Synthetic lifecycle10 only passed at '+commit+'. No GNU or Doom execution.\n')
if __name__=='__main__':main()
