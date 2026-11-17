#!/usr/bin/env python3
"""Synthetic admission ONLY. Requires separate review before execution.
Outer harness subreaps failed synthetic supervisors to clean fault-injected
children. It has no GNU provisioning or production lane launch capability.
"""
import ctypes
import hashlib
import json
import os
import pathlib
import resource
import signal
import subprocess
import sys
import tempfile
import time

HERE = pathlib.Path(__file__).resolve().parent

def children():
    return [int(x) for x in pathlib.Path(f'/proc/self/task/{os.getpid()}/children').read_text().split()]

def stop_owned(exclude=()):
    for pid in children():
        if pid in exclude:
            continue
        try:
            fd = os.pidfd_open(pid)
        except ProcessLookupError:
            continue
        try:
            if pid in children():
                try:
                    signal.pidfd_send_signal(fd, signal.SIGKILL)
                except ProcessLookupError:
                    pass
        finally:
            os.close(fd)

def reap_except(sentinel_pid):
    # Reap each direct child separately so sentinel Popen retains wait ownership.
    for pid in children():
        if pid != sentinel_pid:
            try:
                os.waitpid(pid, os.WNOHANG)
            except ChildProcessError:
                pass

def still_same(record):
    try:
        raw = pathlib.Path(f"/proc/{record['pid']}/stat").read_text()
    except FileNotFoundError:
        return False
    return raw[raw.rfind(')') + 2:].split()[19] == record['starttime']

def main():
    # Small inherited per-process bounds, not aggregate resource isolation.
    resource.setrlimit(resource.RLIMIT_CORE,(0,0))
    resource.setrlimit(resource.RLIMIT_AS,(512*1024**2,512*1024**2))
    resource.setrlimit(resource.RLIMIT_CPU,(20,20))
    resource.setrlimit(resource.RLIMIT_FSIZE,(1024**2,1024**2))
    global_deadline = time.monotonic()+30
    signal.signal(signal.SIGCHLD, signal.SIG_DFL)
    if children():
        raise RuntimeError('preflight already owns children')
    libc = ctypes.CDLL(None, use_errno=True)
    if libc.prctl(36, 1, 0, 0, 0) != 0:
        raise RuntimeError('preflight subreaper denied')
    # Exclusive temporary namespace; retained on failure for diagnosis.
    root = pathlib.Path(tempfile.mkdtemp(prefix='rune-lifecycle-preflight-'))
    root.chmod(0o700)
    sentinel_dir = root / 'sentinel'; sentinel_dir.mkdir()
    sentinel = subprocess.Popen([sys.executable, str(HERE/'fixture.py'), 'sentinel', str(sentinel_dir)],
                                stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                stderr=subprocess.DEVNULL, close_fds=True)
    results = []
    try:
        for case in ('normal', 'nonzero', 'group', 'doublefork', 'detached', 'crash', 'latefork', 'cancel', 'admission', 'cleanup'):
            if time.monotonic() >= global_deadline:raise RuntimeError('preflight global wall limit')
            case_dir = root/case; case_dir.mkdir()
            fixture_mode = 'group' if case in ('admission','cleanup') else case
            fixture = [sys.executable, str(HERE/'fixture.py'), fixture_mode, str(case_dir)]
            if case in ('admission','cleanup','latefork'):
                command = [sys.executable, str(HERE/'fault_adapter.py'), 'boundary' if case=='latefork' else case] + fixture
            else:
                command = [sys.executable, str(HERE/'supervisor.py'), '--execution-seconds',
                           '2' if case == 'cancel' else '0.3', '--cleanup-seconds', '1', '--'] + fixture
            with (case_dir/'stdout').open('xb') as out, (case_dir/'stderr').open('xb') as err:
                proc = subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=out, stderr=err,
                                        close_fds=True, start_new_session=True)
                deadline = min(time.monotonic()+4, global_deadline)
                sent = False
                while proc.poll() is None and time.monotonic()<deadline:
                    if case == 'cancel' and not sent and (case_dir/'descendant.json').exists():
                        proc.send_signal(signal.SIGTERM); sent=True
                    if (case_dir/'stdout').stat().st_size + (case_dir/'stderr').stat().st_size > 1024**2:
                        proc.send_signal(signal.SIGTERM)
                        raise RuntimeError('preflight output bound')
                    time.sleep(0.01)
                if proc.poll() is None:
                    proc.send_signal(signal.SIGTERM)
                    try:
                        proc.wait(timeout=1)
                    except subprocess.TimeoutExpired:
                        proc.kill(); proc.wait()
                    raise RuntimeError('synthetic supervisor exceeded deadline')
                rc = proc.returncode
            # Fault-injected cleanup intentionally fails. Outer subreaper owns
            # any adopted children now; cleanup before next case, never pass it.
            clean_deadline=time.monotonic()+2
            if case == 'cleanup':
                while [p for p in children() if p != sentinel.pid] and time.monotonic()<clean_deadline:
                    stop_owned((sentinel.pid,));reap_except(sentinel.pid);time.sleep(0.01)
            assert not [p for p in children() if p != sentinel.pid], 'unreaped synthetic descendant'
            expected_records = set() if case=='admission' else {'root'}
            if case in ('group','doublefork','detached','crash','latefork','cancel','cleanup'):
                expected_records.add('descendant')
            if case=='doublefork':expected_records.add('intermediate')
            if case=='latefork':expected_records.add('late-descendant')
            records={f.stem:json.loads(f.read_text()) for f in case_dir.glob('*.json')}
            assert set(records)==expected_records, 'fixture did not reach exact required states'
            for record in records.values():
                assert not still_same(record), 'fixture identity still alive'
            if records:
                first=records['root']
                assert first['pgid']==first['pid']==first['sid']
            if 'descendant' in records:
                child=records['descendant']
                assert child['pid']!=first['pid'] and child['pgid']==child['pid']
                if case=='doublefork':assert child['sid']==child['pid']
                else:assert child['sid']==first['sid']
            if case=='doublefork':
                middle=records['intermediate']
                assert middle['pid'] not in (first['pid'],child['pid'])
                assert middle['pgid']==first['pgid'] and middle['sid']==first['sid']
            if case=='latefork':
                late=records['late-descendant']
                assert late['pid'] not in (first['pid'],child['pid'])
                assert late['pgid']==late['sid']==late['pid']
                assert (case_dir/'cleanup-trigger').read_text()=='first cleanup signal intercepted\n'

            assert sentinel.poll() is None, 'unrelated sentinel was harmed'
            if case == 'admission':
                assert rc != 0 and not (case_dir/'root.json').exists()
            else:
                lines=(case_dir/'stdout').read_text().splitlines()
                receipt=json.loads(lines[-1]);assert receipt['ok'] == (case == 'normal')
                assert (rc==0)==(case=='normal')
                assert receipt['empty'] == (case!='cleanup')
                expected_status={'normal':0,'nonzero':7,'group':-9,'doublefork':0,
                                 'detached':0,'crash':-9,'latefork':-9,'cancel':-9,'cleanup':None}
                expected_reason={'normal':'completed','nonzero':'child-failed',
                    'group':'timeout','doublefork':'surviving-descendant:completed',
                    'detached':'surviving-descendant:completed',
                    'crash':'surviving-descendant:child-failed','latefork':'timeout',
                    'cancel':'cancelled','cleanup':'cleanup-incomplete:cleanup-error:PermissionError'}
                assert receipt['root_status']==expected_status[case]
                assert receipt['reason']==expected_reason[case]
                assert receipt['root_pid']==records['root']['pid']
                assert receipt['root_starttime']==records['root']['starttime']
                assert receipt['deadline_met'], 'cleanup exceeded limit'
                if case == 'cancel':assert sent

            results.append({'case':case,'supervisor_exit':rc,'identities_gone':True,'sentinel_alive':True})
    finally:
        sentinel.terminate()
        try:sentinel.wait(timeout=1)
        except subprocess.TimeoutExpired:sentinel.kill();sentinel.wait()
        deadline=time.monotonic()+2
        while children() and time.monotonic()<deadline:
            stop_owned();reap_except(-1);time.sleep(0.01)
        if children():raise RuntimeError('preflight cleanup incomplete; disposable teardown required')
        try:os.waitpid(-1,os.WNOHANG)
        except ChildProcessError:pass
        else:raise RuntimeError('preflight did not reach ECHILD')
    result={'synthetic_only':True,'results':results,'directory':str(root),
            'source_sha256':{p.name:hashlib.sha256(p.read_bytes()).hexdigest() for p in HERE.glob('*.py')}}
    (root/'result.json').write_text(json.dumps(result,indent=2)+'\n')
    print(json.dumps(result))

if __name__=='__main__':main()
