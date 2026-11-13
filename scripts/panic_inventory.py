#!/usr/bin/env python3
"""Inventory direct Rust panic-prone syntax; not a runtime or Clippy verdict."""
import argparse
from collections import Counter
import json
from pathlib import Path

from census import tokens

MACROS = {'todo', 'unimplemented', 'panic', 'unreachable'}
METHODS = {'unwrap', 'expect'}
KINDS = {name + '!' for name in MACROS} | {'.' + name + '()' for name in METHODS}


def sites(source):
    ts = tokens(source, 'rust')
    found = []
    for i, (token, offset) in enumerate(ts):
        kind = None
        raw_identifier = i >= 2 and [t[0] for t in ts[i - 2:i]] == ['r', '#']
        if not raw_identifier and token in MACROS and i + 2 < len(ts) and ts[i + 1][0] == '!' and ts[i + 2][0] in {'(', '[', '{'}:
            kind = token + '!'
        elif token in METHODS and i > 0 and ts[i - 1][0] == '.' and i + 1 < len(ts) and ts[i + 1][0] == '(':
            kind = '.' + token + '()'
        if kind:
            line_start = source.rfind('\n', 0, offset) + 1
            found.append({'kind': kind, 'line': source.count('\n', 0, offset) + 1,
                          'column': offset - line_start + 1})
    return found


def inventory(root, revision):
    if not (root / 'src').is_dir():
        raise ValueError(f'expected source directory: {root / "src"}')
    files = sorted({p for base in (root / 'src', root / 'crates') if base.is_dir()
                    for p in base.rglob('*.rs') if p.is_file()})
    if not files:
        raise ValueError('no Rust source files found')
    locations, file_counts = [], {}
    for path in files:
        try:
            found = sites(path.read_text(encoding='utf-8'))
        except (UnicodeError, ValueError) as exc:
            raise ValueError(f'{path}: {exc}') from exc
        file = path.relative_to(root).as_posix()
        file_counts[file] = dict(sorted(Counter(site['kind'] for site in found).items()))
        locations.extend(dict(site, file=file) for site in found)
    counts = Counter(site['kind'] for site in locations)
    return {'schema_version': 1, 'measurement': 'lexical-rust-panic-syntax',
            'revision': revision, 'files_scanned': len(files),
            'limitations': [
                'Includes tests, inactive cfg branches and macro templates; not a production-only count.',
                'Direct syntax only; no macro expansion, name resolution, Clippy or runtime execution.',
                'A site may be safe or unreachable; zero sites does not prove absence of panics or aborts.',
                'Method turbofish and alternative spellings are not counted; build.rs and dependencies are outside scope.'],
            'totals': {kind: counts[kind] for kind in sorted(KINDS)},
            'files': file_counts, 'sites': locations}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--rune-root', type=Path, default=Path('.'))
    parser.add_argument('--revision', required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    try:
        data = inventory(args.rune_root, args.revision)
    except (OSError, ValueError) as exc:
        parser.error(str(exc))
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(data, indent=2) + '\n', encoding='utf-8')


if __name__ == '__main__':
    main()
