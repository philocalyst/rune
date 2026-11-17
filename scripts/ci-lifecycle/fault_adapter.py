#!/usr/bin/env python3
"""Synthetic-only adapter; NEVER part of GNU lane command path."""
import os
import pathlib
import time
import signal
import sys
import supervisor

mode = sys.argv[1]
if mode == 'admission':
    def denied(*_args):
        raise PermissionError('synthetic pidfd admission denial')
    supervisor.os.pidfd_open = denied
elif mode == 'cleanup':
    def denied(*_args):
        raise PermissionError('synthetic cleanup denial')
    supervisor.signal.pidfd_send_signal = denied
elif mode == 'boundary':
    real_signal = supervisor.signal.pidfd_send_signal
    directory = pathlib.Path(sys.argv[-1])
    triggered = [False]
    def boundary(*args):
        if not triggered[0]:
            triggered[0] = True
            (directory/'cleanup-trigger').write_text('first cleanup signal intercepted\n')
            deadline = time.monotonic()+1
            while not (directory/'late-descendant.json').exists():
                if time.monotonic() >= deadline:
                    raise RuntimeError('synthetic latefork handshake timed out')
                time.sleep(0.005)
        return real_signal(*args)
    supervisor.signal.pidfd_send_signal = boundary
else:
    raise ValueError('unknown fault mode')
# A separate process imports the reviewed helper. No production fault toggle.
sys.argv = [sys.argv[0], '--execution-seconds', '0.2', '--cleanup-seconds', '2', '--'] + sys.argv[2:]
sys.exit(supervisor.main())
