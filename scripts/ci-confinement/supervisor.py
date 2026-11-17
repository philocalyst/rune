#!/usr/bin/env python3
"""Linux lifecycle-only supervisor. Not a security sandbox. Review-only candidate."""
import argparse
import ctypes
import errno
import json
import os
import signal
import subprocess
import sys
import time


def supervise(command, execution_seconds, cleanup_seconds, cancellation_state=None):
    if sys.platform != 'linux':
        raise RuntimeError('Linux required')
    if not (0 < execution_seconds <= 115 and 0 < cleanup_seconds <= 5):
        raise ValueError('invalid bounded limits')
    # Optional shared sticky state for sequential calls in one controller.
    cancelled = [] if cancellation_state is None else cancellation_state
    def request(signum, _frame):
        cancelled.append(signum)
    signal.signal(signal.SIGTERM, request)
    signal.signal(signal.SIGINT, request)
    # Python installs default disposition without SA_NOCLDWAIT.
    signal.signal(signal.SIGCHLD, signal.SIG_DFL)
    try:
        os.waitpid(-1, os.WNOHANG)
    except ChildProcessError:
        pass
    else:
        raise RuntimeError('supervisor already owns children')
    libc = ctypes.CDLL(None, use_errno=True)
    libc.prctl.argtypes = [ctypes.c_int, ctypes.c_ulong, ctypes.c_ulong,
                          ctypes.c_ulong, ctypes.c_ulong]
    libc.prctl.restype = ctypes.c_int
    if libc.prctl(36, 1, 0, 0, 0) != 0:
        raise OSError(ctypes.get_errno(), 'SET_CHILD_SUBREAPER')
    value = ctypes.c_int()
    if libc.prctl(37, ctypes.addressof(value), 0, 0, 0) != 0 or value.value != 1:
        raise RuntimeError('GET_CHILD_SUBREAPER verification failed')
    children_file = f'/proc/self/task/{os.getpid()}/children'
    def children():
        with open(children_file, encoding='ascii') as stream:
            return [int(p) for p in stream.read().split()]
    if children():
        raise RuntimeError('unexpected initial children')
    # Confirm APIs before launch; self pidfd is never signalled.
    fd = os.pidfd_open(os.getpid(), 0)
    os.close(fd)
    if not callable(getattr(signal, 'pidfd_send_signal', None)):
        raise RuntimeError('pidfd signalling unavailable')
    if cancelled:
        return {'ok': False, 'reason': 'cancelled-before-launch', 'empty': True}
    root_pid = None
    root_handle = None
    root_starttime = None
    root_status = None
    proc = None
    observed = set()
    reason = None
    empty = False
    contaminated = False
    started = time.monotonic()
    execution_deadline = started + execution_seconds
    # Only this routine reaps; never use Popen poll/wait/communicate.
    def reap(deadline):
        nonlocal root_status
        # Bound even a sustained stream of exited descendants. Returning
        # False means completion was NOT established; caller handles deadline.
        reaped = 0
        while True:
            if time.monotonic() >= deadline or reaped >= 1024:
                return False
            try:
                pid, status = os.waitpid(-1, os.WNOHANG)
            except ChildProcessError:
                return True
            if pid == 0:
                return False
            reaped += 1
            observed.add(pid)
            if pid == root_pid:
                root_status = os.waitstatus_to_exitcode(status)
                proc.returncode = root_status
    def kill_direct():
        for pid in children():
            observed.add(pid)
            try:
                handle = os.pidfd_open(pid, 0)
            except ProcessLookupError:
                continue
            try:
                # No reaping between enumeration/open/recheck/signal: a dead
                # child remains a zombie, so its PID cannot be reused here.
                if pid in children():
                    try:
                        signal.pidfd_send_signal(handle, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
            finally:
                os.close(handle)
    try:
        if cancelled:
            reason = 'cancelled-before-launch'
            return {'ok': False, 'reason': reason, 'empty': True, 'cancelled': cancelled}
        proc = subprocess.Popen(command, stdin=subprocess.DEVNULL,
                                close_fds=True, start_new_session=True)
        root_pid = proc.pid
        observed.add(root_pid)
        # Original child is unreaped here, so identity cannot be recycled.
        root_handle = os.pidfd_open(root_pid, 0)
        with open(f'/proc/{root_pid}/stat', encoding='ascii') as stream:
            stat = stream.read()
        root_starttime = stat[stat.rfind(')') + 2:].split()[19]
        while True:
            empty = reap(execution_deadline)
            if cancelled:
                reason = 'cancelled'
                break
            if time.monotonic() >= execution_deadline:
                reason = 'timeout'
                break
            if root_status is not None:
                reason = 'completed' if root_status == 0 else 'child-failed'
                contaminated = not empty
                break
            time.sleep(0.01)
    except BaseException as exc:
        reason = 'supervisor-error:' + type(exc).__name__
    finally:
        # The cancellation handler never raises across Popen ownership setup.
        cleanup_deadline = min(time.monotonic() + cleanup_seconds,
                               started + execution_seconds + cleanup_seconds)
        try:
            if root_handle is not None and root_status is None:
                try:
                    signal.pidfd_send_signal(root_handle, signal.SIGKILL)
                except ProcessLookupError:
                    pass
            while True:
                if time.monotonic() >= cleanup_deadline:
                    break
                kill_direct()
                empty = reap(cleanup_deadline)
                if empty:
                    if children():
                        raise RuntimeError('ECHILD/list disagreement')
                    break
                if time.monotonic() >= cleanup_deadline:
                    break
                time.sleep(0.01)
        except BaseException as exc:
            empty = False
            reason = 'cleanup-error:' + type(exc).__name__
        finally:
            if root_handle is not None:
                os.close(root_handle)
    deadline_met = time.monotonic() <= cleanup_deadline
    if not deadline_met:
        reason = 'cleanup-deadline-exceeded:' + str(reason)
    if cancelled:
        reason = 'cancelled'
    if not empty:
        reason = 'cleanup-incomplete:' + str(reason)
    if contaminated:
        reason = 'surviving-descendant:' + str(reason)
    return {'ok': reason == 'completed' and empty and not contaminated and not cancelled and deadline_met,
            'reason': reason, 'empty': empty, 'root_pid': root_pid,
            'root_status': root_status, 'root_starttime': root_starttime,
            'deadline_met': deadline_met, 'observed_pids': sorted(observed),
            'cancelled': cancelled, 'seconds': time.monotonic() - started}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--execution-seconds', type=float, default=115)
    parser.add_argument('--cleanup-seconds', type=float, default=5)
    parser.add_argument('command', nargs=argparse.REMAINDER)
    args = parser.parse_args()
    command = args.command
    if command[:1] == ['--']:
        command = command[1:]
    if not command:
        parser.error('command required')
    result = supervise(command, args.execution_seconds, args.cleanup_seconds)
    # Caller must separately pin source/command, enforce output/rlimits and
    # verify receipt. JSON output alone is not an authenticated CI receipt.
    print(json.dumps(result), flush=True)
    return 0 if result['ok'] else 1


if __name__ == '__main__':
    sys.exit(main())
