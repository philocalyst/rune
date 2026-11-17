#!/usr/bin/env python3
"""Fixed synthetic self-confinement admission only; never GNU/Rune execution.
Invoke only under an independently reviewed external watchdog after source review.
The probe ELF must be built separately from pinned probe.c and supplied by path.
"""
import argparse
import contextlib
import hashlib
import json
import os
import pathlib
import resource
import signal
import stat
import sys
import tempfile
import time
sys.dont_write_bytecode = True
import supervisor

CASES = ('positive','read-denied','write-denied','truncate-denied',
         'socket-inet','socket-inet6','socket-unix','socket-abstract','socketpair',
         'io-uring','ptrace','pidfd-getfd','inherited-fd-close',
         'inherited-policy','unavailable-policy','inherited-seccomp')
CANARY = b'probe-canary\n'
def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()

@contextlib.contextmanager
def output_files(stdout, stderr):
    # Actual fd redirection captures the fixed C parent, not merely Python print.
    sys.stdout.flush();sys.stderr.flush()
    oldout=os.dup(1);olderr=os.dup(2)
    try:
        os.dup2(stdout.fileno(),1);os.dup2(stderr.fileno(),2)
        yield
    finally:
        os.dup2(oldout,1);os.dup2(olderr,2)
        os.close(oldout);os.close(olderr)

def main():
    parser=argparse.ArgumentParser()
    parser.add_argument('--probe',required=True)
    parser.add_argument('--probe-sha256',required=True)
    args=parser.parse_args()
    probe=pathlib.Path(args.probe)
    assert probe.is_absolute() and probe.resolve()==probe and probe.is_file() and not probe.is_symlink()
    assert digest(probe)==args.probe_sha256 and probe.open('rb').read(4)==b'\x7fELF'
    here=pathlib.Path(__file__).resolve().parent
    sources={p.name:digest(p) for p in (here/'probe.c',here/'controller.py',here/'supervisor.py')}
    # Caller must supply fresh private HOME/TMPDIR; never inherit a desktop home.
    tmp=pathlib.Path(os.environ['TMPDIR']).resolve();home=pathlib.Path(os.environ['HOME']).resolve()
    assert tmp.is_dir() and home.is_dir() and tmp.stat().st_uid==os.getuid() and home.stat().st_uid==os.getuid()
    assert tmp.stat().st_mode&0o077==0 and home.stat().st_mode&0o077==0
    os.environ.clear();os.environ.update({'PATH':'/usr/bin:/bin','HOME':str(home),'TMPDIR':str(tmp),'LANG':'C','LC_ALL':'C','TZ':'UTC','PYTHONDONTWRITEBYTECODE':'1'})
    for key,limit in [(resource.RLIMIT_CORE,0),(resource.RLIMIT_AS,512*1024**2),(resource.RLIMIT_CPU,20),(resource.RLIMIT_FSIZE,1024**2)]:
        resource.setrlimit(key,(limit,limit))
    root=pathlib.Path(tempfile.mkdtemp(prefix='rune-confinement-',dir=tmp));root.chmod(0o700)
    deadline=time.monotonic()+30
    receipts=[]
    cancelled=[]
    def request(signum, _frame):cancelled.append(signum)
    signal.signal(signal.SIGTERM,request);signal.signal(signal.SIGINT,request)
    for case in CASES:
        assert not cancelled, 'controller cancelled'
        remaining=deadline-time.monotonic();assert remaining>1,'suite deadline'
        directory=root/case;directory.mkdir(mode=0o700)
        allow=directory/'allow';deny=directory/'deny'
        for p in (allow,deny):p.mkdir(mode=0o700)
        input_file=allow/'input';canary_file=deny/'canary'
        identities={}
        for p in (input_file,canary_file):
            with p.open('xb') as stream:stream.write(CANARY)
            p.chmod(0o600)
            st=p.lstat();assert stat.S_ISREG(st.st_mode) and st.st_uid==os.getuid()
            identities[str(p)]=(st.st_dev,st.st_ino)
            # Parent proves ordinary read AND write access before restrictions.
            with p.open('rb') as stream:assert stream.read()==CANARY
            fd=os.open(p,os.O_WRONLY|os.O_NOFOLLOW|os.O_CLOEXEC);os.close(fd)
        with (directory/'stdout').open('xb') as out,(directory/'stderr').open('xb') as err:
            with output_files(out,err):
                lifecycle=supervisor.supervise([str(probe),case,str(allow),str(deny)],min(3,remaining-1),1,cancellation_state=cancelled)
        # Lifecycle receipt is an in-process return, never parsed from child stdout.
        assert not cancelled, 'controller cancelled'
        assert lifecycle['ok'] is True and lifecycle['empty'] is True and lifecycle['root_status']==0,lifecycle
        out=(directory/'stdout').read_bytes();err=(directory/'stderr').read_bytes()
        assert len(out)+len(err)<=65536,'combined synthetic output bound'
        assert not err,'unexpected probe diagnostics'
        lines=out.splitlines();assert len(lines)==1
        # Only the pinned C parent emits this protocol, after observing a separate
        # readiness pipe and kernel wait status. Confined child never emits JSON.
        report=json.loads(lines[0])
        assert set(report)=={'mode','abi','policy_ready','ready_count','exit','signal','timedout','passed'}
        assert report['mode']==case and report['passed'] is True
        assert report['abi']>=3,'required Landlock unavailable'
        assert report['timedout'] is False
        if case=='unavailable-policy':
            assert report['policy_ready'] is False and report['ready_count']==0
            assert report['exit']==77 and report['signal']==0
        else:
            assert report['policy_ready'] is True
            assert report['ready_count']==(2 if case in ('inherited-policy','inherited-seccomp') else 1)
            forbidden={'socket-inet','socket-inet6','socket-unix','socket-abstract',
                       'socketpair','io-uring','ptrace','pidfd-getfd'}
            if case in forbidden:
                assert report['exit']==-1 and report['signal']==signal.SIGSYS
            else:
                assert report['exit']==0 and report['signal']==0
        for p in (input_file,canary_file):
            st=p.lstat()
            assert stat.S_ISREG(st.st_mode) and st.st_uid==os.getuid()
            assert (st.st_dev,st.st_ino)==identities[str(p)]
            assert p.read_bytes()==CANARY
        assert {p.name for p in deny.iterdir()}=={'canary'}
        assert {p.name for p in allow.iterdir()}==({'input','output'} if case=='positive' else {'input'})
        if case=='positive':
            output=allow/'output';st=output.lstat()
            assert stat.S_ISREG(st.st_mode) and st.st_uid==os.getuid()
            assert output.read_bytes()==b'probe-output\n'
        assert digest(probe)==args.probe_sha256
        assert sources=={p.name:digest(p) for p in (here/'probe.c',here/'controller.py',here/'supervisor.py')}
        record={'case':case,'parent_probe':report,'lifecycle':lifecycle,
                'stdout_sha256':digest(directory/'stdout'),'stderr_sha256':digest(directory/'stderr')}
        (directory/'verified.json').write_text(json.dumps(record,indent=2)+'\n')
        receipts.append(record)
    # All supervised children reached ECHILD. Stop swallowing completion-time
    # signals: earlier signals remain sticky; later signals terminate nonzero.
    signal.signal(signal.SIGTERM,signal.SIG_DFL)
    signal.signal(signal.SIGINT,signal.SIG_DFL)
    assert time.monotonic()<=deadline and not cancelled
    result={'synthetic_confinement_only':True,'probe_sha256':args.probe_sha256,
            'source_sha256':sources,'cases':receipts,'gnu_tested':False,'rune_tested':False,
            'doom_tested':False,'old_local_process':'UNKNOWN/unaffected'}
    (root/'verified-result.json').write_text(json.dumps(result,indent=2)+'\n')
    assert not cancelled, 'controller cancelled before completion'
    print(json.dumps({'synthetic_confinement_passed':True,'cases':len(receipts),'evidence':str(root),
                      'gnu_tested':False,'rune_tested':False,'doom_tested':False}),flush=True)
if __name__=='__main__':main()
