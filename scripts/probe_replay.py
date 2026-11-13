#!/usr/bin/env python3
"""Replay one saved primitive-probe case in a fresh, bounded Rune process."""
import argparse
import json
import math
from pathlib import Path

from run_stdin_differential import run_batch


def read_candidate(path, case_id=None):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError('duplicate JSON key: ' + key)
            result[key] = value
        return result
    report = json.loads(path.read_text(encoding='utf-8'), object_pairs_hook=unique)
    if not isinstance(report, dict) or type(report.get('schema_version')) is not int or report['schema_version'] != 1 or report.get('measurement') != 'bounded-primitive-form-probes':
        raise ValueError('expected schema-v1 bounded primitive-probe report')
    if not isinstance(report.get('revision'), str):
        raise ValueError('expected original revision label')
    cases = report.get('replay_cases')
    if not isinstance(cases, list) or not 1 <= len(cases) <= 5000:
        raise ValueError('expected concrete replay_cases list')
    identities = set()
    for case in cases:
        if not isinstance(case, dict) or set(case) != {'id', 'form'} or any(
                not isinstance(case[key], str) or not case[key].strip() for key in ('id', 'form')):
            raise ValueError('invalid saved replay case')
        if case['id'] in identities:
            raise ValueError('duplicate saved case id: ' + case['id'])
        identities.add(case['id'])
        if any(char in case['form'] for char in ('\n', '\r', ';', '\x00')):
            raise ValueError('saved case violates single-line semicolon-free bridge bounds')
    result = report.get('result')
    if not isinstance(result, dict) or not isinstance(result.get('runtime'), dict) or not isinstance(result['runtime'].get('status'), str):
        raise ValueError('expected original runtime status')
    if case_id is None:
        case_id = result.get('first_unobserved_candidate')
        if not isinstance(case_id, str) or not case_id:
            raise ValueError('no saved candidate; select an explicit --case-id')
    if not isinstance(case_id, str) or case_id not in identities:
        raise ValueError('selected case id not present in saved replay cases')
    selected = next(case for case in cases if case['id'] == case_id)
    return selected, report['revision'], result['runtime']['status']


def replay(case, command, timeout):
    runtime = run_batch([case], command, 'rune', timeout)
    failed = runtime['status'] != 'completed'
    return {'runtime': runtime, 'isolated_runtime_failure': failed,
            'observation': runtime['results'].get(0),
            'assessment': 'isolated-runtime-failure-observed' if failed else 'isolated-runtime-completed'}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('report', type=Path)
    parser.add_argument('--case-id')
    parser.add_argument('--binary', default='./target/release/rune')
    parser.add_argument('--arg', action='append', default=[])
    parser.add_argument('--timeout', type=float, default=10)
    parser.add_argument('--revision', required=True, help='runtime revision being replayed, not inferred from source report')
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    if not math.isfinite(args.timeout) or args.timeout <= 0:
        parser.error('--timeout must be finite and positive')
    try:
        case, original_revision, original_status = read_candidate(args.report, args.case_id)
    except (OSError, ValueError) as exc:
        parser.error(str(exc))
    result = replay(case, [args.binary, *args.arg], args.timeout)
    data = {'schema_version': 1, 'measurement': 'isolated-primitive-probe-replay',
            'revision': args.revision, 'original_revision': original_revision,
            'original_runtime_status': original_status, 'case': case,
            'limitations': ['Fresh raw-core process; no original batch prefix/state is replayed.',
                            'Isolated failure does not establish same cause as original failure.',
                            'Completed value/condition is not semantic correctness or proof original failure was spurious.',
                            'No shrinking, oracle, source provenance verification or automatic bug-fix claim.'],
            'result': result}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(data, indent=2) + '\n', encoding='utf-8')
    return int(result['isolated_runtime_failure'])


if __name__ == '__main__':
    raise SystemExit(main())
