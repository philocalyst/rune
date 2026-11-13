#!/usr/bin/env python3
"""Run reproducible bounded primitive-form probes, without a semantic oracle."""
import argparse
from collections import Counter
import json
import math
from pathlib import Path
import random

from run_stdin_differential import literal, run_batch

FAMILIES = ('arithmetic', 'division', 'car', 'cdr', 'nth', 'length', 'aref', 'substring', 'make-vector')
NUMBERS = (-1073741824, -1, 0, 1, 2, 1073741824)
OBJECTS = ('nil', 't', '0', '-1', '""', '"aλ"', "'(1 2)", "'(1 . 2)", '[1 2]')
TEXTS = ('', 'a', 'λ', 'aλ', 'é界')
INDICES = (-3, -1, 0, 1, 2, 3, 8)


def generate(seed, count):
    rng, cases = random.Random(seed), []
    for index in range(count):
        family = rng.choice(FAMILIES)
        if family == 'arithmetic':
            form = f'({rng.choice(("+", "-", "*"))} {rng.choice(NUMBERS)} {rng.choice(NUMBERS)})'
        elif family == 'division':
            form = f'(/ {rng.choice(NUMBERS)} {rng.choice((-1, 0, 1, 2))})'
        elif family in {'car', 'cdr', 'length'}:
            form = f'({family} {rng.choice(OBJECTS)})'
        elif family == 'nth':
            form = f'(nth {rng.choice(INDICES)} {rng.choice(OBJECTS)})'
        elif family == 'aref':
            form = f'(aref {rng.choice(OBJECTS)} {rng.choice(INDICES)})'
        elif family == 'substring':
            form = f'(substring {literal(rng.choice(TEXTS))} {rng.choice(INDICES)} {rng.choice(INDICES)})'
        else:
            # Deliberately bound allocations: no giant vector-length generator.
            form = f'(make-vector {rng.choice((-1, 0, 1, 2, 8))} {rng.choice(OBJECTS)})'
        cases.append({'id': f'{index:05d}-{family}', 'family': family, 'form': form})
    return cases


def probe(cases, command, timeout):
    runtime = run_batch(cases, command, 'rune', timeout)
    observations = []
    for index, case in enumerate(cases):
        value = runtime['results'].get(index)
        status = 'not-observed' if value is None else 'value' if value.startswith('(value ') else 'condition'
        observations.append(dict(case, status=status, observation=value))
    return {'runtime': runtime, 'observations': observations,
            'counts': dict(sorted(Counter(row['status'] for row in observations).items())),
            'runtime_finding': runtime['status'] != 'completed',
            'first_unobserved_candidate': next((row['id'] for row in observations if row['status'] == 'not-observed'), None)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', default='./target/release/rune')
    parser.add_argument('--arg', action='append', default=[])
    parser.add_argument('--seed', type=int, default=0)
    parser.add_argument('--count', type=int, default=200)
    parser.add_argument('--timeout', type=float, default=15)
    parser.add_argument('--revision', required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    if not 1 <= args.count <= 5000:
        parser.error('--count must be between 1 and 5000')
    if not math.isfinite(args.timeout) or args.timeout <= 0:
        parser.error('--timeout must be finite and positive')
    cases = generate(args.seed, args.count)
    result = probe(cases, [args.binary, *args.arg], args.timeout)
    data = {'schema_version': 1, 'measurement': 'bounded-primitive-form-probes',
            'revision': args.revision, 'seed': args.seed, 'count': args.count,
            'replay_cases': [{'id': case['id'], 'form': case['form']} for case in cases],
            'limitations': ['No semantic oracle; conditions are ordinary observations, not failures.',
                            'Raw-core shared batch state; function availability is not verified.',
                            'First unobserved case is a candidate, not proven crash attribution.',
                            'Small fixed template families; no full type coverage, shrinking or M0 no-abort gate.'],
            'result': result}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(data, indent=2) + '\n', encoding='utf-8')
    return int(result['runtime_finding'])


if __name__ == '__main__':
    raise SystemExit(main())
