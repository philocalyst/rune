#!/usr/bin/env python3
"""Measure declared primitive names, not implementation or semantic coverage."""
import argparse
from collections import defaultdict
import json
from pathlib import Path
import re


# This is a bounded lexical scanner, not a Rust/C parser. Preserve offsets so
# diagnostics and declarations can always point back to the source.
def tokens(source, language):
    result = []
    i = 0
    while i < len(source):
        if source[i].isspace():
            i += 1
            continue
        if source.startswith('//', i):
            end = source.find('\n', i)
            i = len(source) if end < 0 else end
            continue
        if source.startswith('/*', i):
            start, depth = i, 1
            i += 2
            while i < len(source) and depth:
                if language == 'rust' and source.startswith('/*', i):
                    depth += 1
                    i += 2
                elif source.startswith('*/', i):
                    depth -= 1
                    i += 2
                else:
                    i += 1
            if depth:
                raise ValueError(f'unterminated comment at offset {start}')
            continue
        raw = re.match(r'(?:br|r)(#{0,255})"', source[i:])
        if raw:
            end = source.find('"' + raw[1], i + len(raw[0]))
            if end < 0:
                raise ValueError(f'unterminated raw string at offset {i}')
            end += 1 + len(raw[1])
        elif source[i] == '"':
            match = re.match(r'"(?:[^"\\]|\\[\s\S])*"', source[i:])
            if not match:
                raise ValueError(f'unterminated string at offset {i}')
            end = i + len(match[0])
        elif source[i] == "'" and (match := re.match(r"'(?:[^'\\\n]|\\(?:[^\n]|x[0-9a-fA-F]{2}))'", source[i:])):
            end = i + len(match[0])
        else:
            match = re.match(r'[A-Za-z_][A-Za-z_0-9]*', source[i:])
            end = i + len(match[0]) if match else i + 1
        result.append((source[i:end], i))
        i = end
    return result


def literal(token):
    # Primitive names in both trees use plain ASCII string literals. Do not
    # guess escape rules or concatenate string expressions across languages.
    if not re.fullmatch(r'"[A-Za-z0-9_+*/<>=!?%:~.^@&$-]+"', token):
        raise ValueError('unsupported primitive name literal: ' + token)
    return token[1:-1]


def declarations(source, language):
    ts = tokens(source, language)
    rows = []
    for i, (token, offset) in enumerate(ts):
        name = None
        if language == 'c' and token == 'DEFUN' and i + 1 < len(ts) and ts[i + 1][0] == '(':
            if i + 2 >= len(ts):
                raise ValueError('truncated DEFUN')
            # Ignore the macro definition itself: #define DEFUN(lname, ...).
            line = source[source.rfind('\n', 0, offset) + 1:offset]
            if re.match(r'\s*#\s*define\s*$', line):
                continue
            name = literal(ts[i + 2][0])
            if i + 3 >= len(ts) or ts[i + 3][0] != ',':
                raise ValueError('unsupported DEFUN name expression')
        elif language == 'rust' and [t[0] for t in ts[i:i + 3]] == ['#', '[', 'defun']:
            end, depth = i + 3, 1
            while end < len(ts) and depth:
                depth += (ts[end][0] == '[') - (ts[end][0] == ']')
                end += 1
            if depth:
                raise ValueError('unterminated defun attribute')
            attrs = [t[0] for t in ts[i + 3:end - 1]]
            overrides = [j for j, value in enumerate(attrs) if value == 'name']
            if overrides:
                if len(overrides) != 1 or attrs[overrides[0] + 1:overrides[0] + 2] != ['=']:
                    raise ValueError('unsupported defun name attribute')
                try:
                    name = literal(attrs[overrides[0] + 2])
                    if attrs[overrides[0] + 3] not in {',', ')'}:
                        raise ValueError('unsupported defun name expression')
                except IndexError as exc:
                    raise ValueError('missing defun name') from exc
            # Attributes/visibility may intervene, but never cross a body or
            # statement: doing so could attribute a declaration to another fn.
            fn = end
            while fn < len(ts) and ts[fn][0] not in {'fn', '{', ';', '}'}:
                fn += 1
            if fn + 1 >= len(ts) or ts[fn][0] != 'fn' or not re.fullmatch(r'[A-Za-z_][A-Za-z_0-9]*', ts[fn + 1][0]):
                raise ValueError('defun attribute without supported function declaration')
            if name is None:
                name = ts[fn + 1][0].replace('_', '-')
        if name is not None:
            rows.append({'name': name, 'line': source.count('\n', 0, offset) + 1})
    return rows


def scan(root, language):
    suffix = '*.c' if language == 'c' else '*.rs'
    roots = [root / 'src'] if language == 'c' else [root / 'src', root / 'crates']
    if not roots[0].is_dir():
        raise ValueError(f'expected source directory: {roots[0]}')
    files = sorted({p for base in roots if base.is_dir() for p in base.rglob(suffix)})
    if not files:
        raise ValueError(f'no {suffix} files beneath {root}')
    rows = []
    for path in files:
        try:
            found = declarations(path.read_text(encoding='utf-8'), language)
        except (ValueError, UnicodeError) as exc:
            raise ValueError(f'{path}: {exc}') from exc
        rows.extend(dict(row, file=path.relative_to(root).as_posix()) for row in found)
    if not rows:
        raise ValueError(f'no primitive declarations found beneath {root}')
    return rows


def report(gnu, rune, gnu_revision, rune_revision):
    gnames, rnames = {r['name'] for r in gnu}, {r['name'] for r in rune}
    groups = defaultdict(set)
    for row in gnu:
        groups[row['file']].add(row['name'])
    def counts(names):
        present = names & rnames
        return {'declared': len(names), 'present': len(present),
                'declared_coverage': len(present) / len(names) if names else None,
                'present_names': sorted(present), 'missing_names': sorted(names - rnames)}
    def duplicates(rows):
        grouped = defaultdict(list)
        for row in rows:
            grouped[row['name']].append({'file': row['file'], 'line': row['line']})
        return {name: sites for name, sites in sorted(grouped.items()) if len(sites) > 1}
    return {'schema_version': 1, 'measurement': 'declared-primitive-coverage',
            'gnu_revision': gnu_revision, 'rune_revision': rune_revision,
            'limitations': ['Lexical declarations only; no semantic correctness claim.',
                            'Includes inactive cfg/preprocessor branches and source-only declarations.',
                            'Does not expand macros or discover runtime defalias registrations.'],
            'totals': counts(gnames), 'rune_only_names': sorted(rnames - gnames),
            'files': {file: counts(names) for file, names in sorted(groups.items())},
            'declarations': {'gnu': gnu, 'rune': rune},
            'duplicates': {'gnu': duplicates(gnu), 'rune': duplicates(rune)}}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--gnu-root', type=Path, required=True, help='GNU checkout containing src/*.c')
    parser.add_argument('--rune-root', type=Path, default=Path('.'))
    parser.add_argument('--gnu-revision', required=True)
    parser.add_argument('--rune-revision', required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    try:
        data = report(scan(args.gnu_root, 'c'), scan(args.rune_root, 'rust'),
                      args.gnu_revision, args.rune_revision)
    except (ValueError, OSError) as exc:
        parser.error(str(exc))
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(data, indent=2) + '\n', encoding='utf-8')


if __name__ == '__main__':
    main()
