#!/usr/bin/env python3
"""Render existing upstream ERT JSON as an honest per-file Markdown table."""
import argparse
import html
import json
from pathlib import Path

from run_upstream_ert import OUTCOMES, regressions

STATUSES = {'completed', 'permanently-skipped', 'could-not-load', 'runner-error',
            'no-tests', 'protocol-error', 'launch-error', 'output-limit',
            'timeout', 'cleanup-error'}


def read_report(path):
    def unique_object(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError('duplicate JSON key: ' + key)
            result[key] = value
        return result
    data = json.loads(path.read_text(encoding='utf-8'), object_pairs_hook=unique_object)
    if not isinstance(data, dict) or type(data.get('schema_version')) is not int or data['schema_version'] != 1:
        raise ValueError('expected ERT schema_version 1')
    if not isinstance(data.get('revision'), str) or not isinstance(data.get('files'), list):
        raise ValueError('expected revision string and files list')
    identities = set()
    for row in data['files']:
        if not isinstance(row, dict) or not isinstance(row.get('file'), str) or not row['file']:
            raise ValueError('expected nonempty file identity')
        if row['file'] in identities:
            raise ValueError('duplicate file identity: ' + row['file'])
        identities.add(row['file'])
        if not isinstance(row.get('status'), str) or row['status'] not in STATUSES:
            raise ValueError('unknown file status: ' + str(row.get('status')))
        tests = row.get('tests')
        if not isinstance(tests, dict) or any(not isinstance(k, str) or not k or
                not isinstance(v, str) or v not in OUTCOMES for k, v in tests.items()):
            raise ValueError('invalid test outcomes for: ' + row['file'])
        if row['status'] == 'completed' and not tests:
            raise ValueError('completed file without observed tests: ' + row['file'])
        if row['status'] == 'permanently-skipped' and tests:
            raise ValueError('permanently-skipped file with test results: ' + row['file'])
    return data


def escape(value):
    # No executable HTML, accidental table cells, links or Markdown emphasis.
    value = html.escape(str(value), quote=True)
    for char in ('\\', '`', '*', '_', '[', ']', '|'):
        value = value.replace(char, '\\' + char)
    return value.replace('\r', ' ').replace('\n', ' ')


def counts(row):
    tests = row['tests'] if row else {}
    return {outcome: sum(value == outcome for value in tests.values())
            for outcome in sorted(OUTCOMES)}


def render(current, previous=None):
    now = {r['file']: r for r in current['files']}
    before = {r['file']: r for r in previous['files']} if previous else {}
    losses = regressions(previous, current) if previous else []
    lines = ['# Upstream ERT measurement', '', 'Revision: ' + escape(current['revision']), '']
    if previous:
        lines += ['Baseline: ' + escape(previous['revision']), '']
    lines += ['Rates below describe **observed test records only**, not complete-suite coverage.',
              'A completed file means the runner finished, not that every test passed.',
              'Incomplete files and absent baseline files remain visible. A dash means no observed tests.', '',
              '| File | Run status | Passed | Failed | Unimplemented | Errored | Skipped | Observed pass rate | Previously passed now absent/nonpass |',
              '| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |']
    for file in sorted(now.keys() | before.keys()):
        row = now.get(file)
        c = counts(row)
        total = sum(c.values())
        rate = f"{100 * c['passed'] / total:.1f}% ({c['passed']}/{total})" if total else '—'
        lost = sum(r['file'] == file for r in losses)
        values = [escape(file), escape(row['status']) if row else 'missing-current',
                  str(c['passed']), str(c['failed']), str(c['unimplemented']), str(c['errored']),
                  str(c['skipped']), rate, str(lost) if previous else '—']
        lines.append('| ' + ' | '.join(values) + ' |')
    lines += ['', f'Files in current report: {len(now)}.',
              f"Completed files: {sum(row['status'] == 'completed' for row in now.values())}.",
              f"Permanently skipped files: {sum(row['status'] == 'permanently-skipped' for row in now.values())}."]
    if previous:
        lines += ['', f'Formerly-passing test regressions: {len(losses)}.']
        if losses:
            lines += ['', '| File | Test formerly passing |', '| --- | --- |']
            for loss in sorted(losses, key=lambda r: (r['file'], r['test'])):
                lines.append('| ' + escape(loss['file']) + ' | ' + escape(loss['test']) + ' |')
    return '\n'.join(lines) + '\n'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('report', type=Path)
    parser.add_argument('--baseline', type=Path)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    try:
        current = read_report(args.report)
        previous = read_report(args.baseline) if args.baseline else None
    except (OSError, ValueError) as exc:
        parser.error(str(exc))
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(render(current, previous), encoding='utf-8')


if __name__ == '__main__':
    main()
