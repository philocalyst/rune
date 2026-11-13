#!/usr/bin/env python3
"""Run upstream ERT files in isolated processes; never infer success from exit alone."""
import argparse
import json
import math
import os
import signal
import threading
from pathlib import Path
import subprocess
import tempfile
import time
import uuid

OUTCOMES = {'passed', 'failed', 'unimplemented', 'errored', 'skipped'}
PERMANENT_SKIPS = {'comp-tests.el', 'lcms-tests.el'}


def driver(path, marker):
    # Lisp strings do not support JSON's \uXXXX escapes.
    literal = '"' + str(path.resolve()).replace('\\', '\\\\').replace('"', '\\"').replace('\n', '\\n').replace('\r', '\\r').replace('\t', '\\t') + '"' 
    return r'''(require 'ert)
(defun rune-report (kind name)
  (princ (concat "\nMARKER\t" kind "\t" (prin1-to-string name) "\n")))
(defun rune-listener (event &rest args)
  (when (eq event 'run-started)
    (rune-report "selected" (length (ert--stats-tests (car args)))))
  (when (and (eq event 'run-ended) (nth 1 args))
    (rune-report "aborted" nil))
  (when (eq event 'test-ended)
    (let* ((test (nth 1 args)) (result (nth 2 args))
           (condition (and (ert-test-result-with-condition-p result)
                           (ert-test-result-with-condition-condition result)))
           (status (cond ((ert-test-passed-p result) "passed")
                         ((ert-test-skipped-p result) "skipped")
                         ((eq (car condition) 'rune-unimplemented) "unimplemented")
                         ((eq (car condition) 'ert-test-failed) "failed")
                         (t "errored"))))
      (rune-report status (ert-test-name test)))))
(condition-case err
    (progn (load FILE nil t t) (rune-report "loaded" nil)
           (ert-run-tests t 'rune-listener)
           (rune-report "done" nil))
  (error (rune-report "driver-error" err)))
'''.replace('MARKER', marker).replace('FILE', literal)


def parse_output(output, marker):
    tests, events = {}, set()
    for line in output.splitlines():
        fields = line.split('\t', 2)
        if len(fields) != 3 or fields[0] != marker:
            continue
        status, name = fields[1:]
        if status in OUTCOMES:
            if name in tests:
                raise ValueError('duplicate test result: ' + name)
            tests[name] = status
        else:
            if status not in {'loaded', 'selected', 'done', 'aborted', 'driver-error'}:
                raise ValueError('unknown protocol event: ' + status)
            if status == 'selected':
                if not name.isascii() or not name.isdecimal():
                    raise ValueError('invalid selected test count: ' + name)
                if any(event.startswith('selected:') for event in events):
                    raise ValueError('duplicate selected test count')
            elif status in events:
                raise ValueError('duplicate protocol event: ' + status)
            events.add(status + ':' + name if status == 'selected' else status)
    return tests, events


def capture(args, cwd, timeout, limit=4 * 1024 * 1024):
    """Drain stdout concurrently with a hard in-memory cap; own a POSIX group."""
    try:
        proc = subprocess.Popen(args, cwd=cwd, stdout=subprocess.PIPE,
                                stderr=subprocess.STDOUT, start_new_session=(os.name == 'posix'))
    except OSError as exc:
        return str(exc), None, 'launch-error'
    chunks = bytearray()
    exceeded = threading.Event()

    def drain():
        while True:
            chunk = proc.stdout.read(65536)
            if not chunk:
                return
            remaining = limit - len(chunks)
            chunks.extend(chunk[:remaining])
            if len(chunk) > remaining:
                exceeded.set()
                return

    reader = threading.Thread(target=drain, daemon=True)
    reader.start()
    deadline, forced = time.monotonic() + timeout, None
    while proc.poll() is None:
        if exceeded.is_set():
            forced = 'output-limit'
            break
        if time.monotonic() >= deadline:
            forced = 'timeout'
            break
        time.sleep(.01)
    # Clean up descendants even when the interpreter exits normally. Never wait
    # indefinitely for inherited stdout handles or grandchildren.
    if os.name == 'posix':
        try:
            os.killpg(proc.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
    elif proc.poll() is None:
        proc.kill()
    proc.wait(timeout=5)
    reader.join(timeout=1)
    if reader.is_alive():
        forced = forced or 'cleanup-error'
    elif exceeded.is_set():
        forced = forced or 'output-limit'
    if not reader.is_alive():
        proc.stdout.close()
    return bytes(chunks).decode('utf-8', errors='replace'), proc.returncode, forced


def run_file(path, command, timeout, flavor='rune', cwd=None):
    if path.name in PERMANENT_SKIPS:
        return {'file': str(path), 'status': 'permanently-skipped', 'tests': {}}
    marker = 'RUNE-ERT-' + uuid.uuid4().hex
    started = time.monotonic()
    with tempfile.TemporaryDirectory(prefix='rune-ert-') as tmp:
        script = Path(tmp) / 'driver.el'
        script.write_text(driver(path, marker), encoding='utf-8')
        args = list(command) + (['-Q', '--batch'] if flavor == 'emacs' else [])
        args += ['--load', str(script)]
        output, code, forced = capture(args, cwd, timeout)
    try:
        tests, events = parse_output(output, marker)
        status = forced or ('could-not-load' if 'loaded' not in events else
                            'runner-error' if code != 0 or 'done' not in events
                            or 'driver-error' in events or 'aborted' in events else
                            'no-tests' if not tests else
                            'protocol-error' if 'selected:' + str(len(tests)) not in events else 'completed')
    except ValueError as exc:
        tests, status = {}, 'protocol-error'
        output += '\n' + str(exc)
    return {'file': str(path), 'status': status, 'tests': tests,
            'counts': {outcome: list(tests.values()).count(outcome) for outcome in sorted(OUTCOMES)},
            'pass_rate': (list(tests.values()).count('passed') / len(tests)) if tests else None,
            'returncode': code, 'seconds': time.monotonic() - started,
            'output': output}


def regressions(previous, current):
    """A missing file/result is a regression too, not an improved pass rate."""
    now = {row['file']: row for row in current['files']}
    losses = []
    for row in previous['files']:
        for name, status in row['tests'].items():
            if status == 'passed' and now.get(row['file'], {}).get('tests', {}).get(name) != 'passed':
                losses.append({'file': row['file'], 'test': name})
    return losses


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('files', nargs='+', type=Path)
    parser.add_argument('--binary', default='./target/release/rune')
    parser.add_argument('--arg', action='append', default=[], help='extra interpreter argument')
    parser.add_argument('--flavor', choices=['rune', 'emacs'], default='rune')
    parser.add_argument('--timeout', type=float, default=60)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--baseline', type=Path)
    parser.add_argument('--revision', default='unknown', help='commit being measured')
    args = parser.parse_args()
    if not math.isfinite(args.timeout) or args.timeout <= 0:
        parser.error('--timeout must be finite and positive')
    files = sorted({p.resolve() for item in args.files for p in
                    (item.rglob('*-tests.el') if item.is_dir() else [item])})
    if not files or any(not p.is_file() for p in files):
        parser.error('provide existing test files or nonempty test directories')
    # Relative identities survive checkout-directory changes between CI commits.
    root = Path.cwd()
    rows = []
    for path in files:
        row = run_file(path, [args.binary, *args.arg], args.timeout, args.flavor)
        row['file'] = str(path.relative_to(root)) if path.is_relative_to(root) else str(path)
        rows.append(row)
    report = {'schema_version': 1, 'revision': args.revision, 'files': rows}
    report['regressions'] = regressions(json.loads(args.baseline.read_text()), report) if args.baseline else []
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + '\n')
    return int(bool(report['regressions']) or any(
        r['status'] not in {'completed', 'permanently-skipped'} or
        any(v in {'failed', 'unimplemented', 'errored'} for v in r['tests'].values()) for r in rows))


if __name__ == '__main__':
    raise SystemExit(main())
