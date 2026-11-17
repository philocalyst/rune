#!/usr/bin/env python3
"""Fixed benign lifecycle fixtures; no oracle, shell, sockets or network."""
import json
import os
import pathlib
import signal
import sys
import time

mode, destination = sys.argv[1:]
root = pathlib.Path(destination)
def record(label):
    # proc stat starttime gives a checkable identity, not merely a reusable PID.
    raw = pathlib.Path('/proc/self/stat').read_text()
    starttime = raw[raw.rfind(')') + 2:].split()[19]
    path = root / (label + '.json')
    temporary = path.with_suffix('.pending')
    with temporary.open('x') as stream:
        json.dump({'pid': os.getpid(), 'starttime': starttime,
                   'pgid': os.getpgrp(), 'sid': os.getsid(0)}, stream)
    os.rename(temporary, path)

def pause():
    # Finite fallback even if harness fails. Supervisor should stop it first.
    time.sleep(10)

if mode == 'normal':
    record('root')
    sys.exit(0)
if mode == 'nonzero':
    record('root')
    sys.exit(7)
if mode == 'sentinel':
    record('sentinel')
    time.sleep(60)
    sys.exit(0)
if mode in ('group', 'cancel', 'crash', 'doublefork', 'latefork', 'detached'):
    record('root')
    readfd, writefd = os.pipe()
    pid = os.fork()
    if pid == 0:
        os.close(readfd)
        if mode == 'doublefork':
            record('intermediate')
            second = os.fork()
            if second:
                os._exit(0)
            os.setsid()
        else:
            os.setpgid(0, 0)
        record('descendant')
        os.write(writefd, b'R')
        os.close(writefd)
        if mode == 'detached':
            os.close(1); os.close(2)
        if mode == 'latefork':
            # Test-only adapter releases this only after cleanup signalling
            # begins; handshake makes the fork occur inside cleanup.
            deadline = time.monotonic()+3
            while not (root/'cleanup-trigger').exists():
                if time.monotonic() >= deadline:os._exit(91)
                time.sleep(0.005)
            late = os.fork()
            if late == 0:
                os.setsid()
                record('late-descendant')
                pause()
                os._exit(0)
        pause()
        os._exit(0)
    os.close(writefd)
    assert os.read(readfd, 1) == b'R'
    os.close(readfd)
    if mode in ('doublefork', 'detached'):
        os._exit(0)
    if mode == 'crash':
        os.kill(os.getpid(), signal.SIGKILL)
    pause()
    sys.exit(0)
raise RuntimeError('unknown fixed fixture')
