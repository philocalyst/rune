#!/usr/bin/env python3
"""Compare declared GNU C variables with Rune defvar macros (not semantics)."""
import argparse
import json
from pathlib import Path
import re

import census

GNU_MACROS = {'DEFVAR_LISP', 'DEFVAR_INT', 'DEFVAR_BOOL',
              'DEFVAR_PER_BUFFER', 'DEFVAR_KBOARD', 'DEFVAR_LISP_NOPRO'}


def arguments(ts, start):
    """Split direct macro arguments, respecting nested (), [] and {}."""
    stack, args, current = [')'], [], []
    matching = {'(': ')', '[': ']', '{': '}'}
    for token, _ in ts[start + 1:]:
        if token in matching:
            stack.append(matching[token])
        elif token in {')', ']', '}'}:
            if token != stack.pop():
                raise ValueError('mismatched macro delimiter')
            if not stack:
                if current:
                    args.append(current)
                return args
        if token == ',' and len(stack) == 1:
            if not current:
                raise ValueError('empty macro argument')
            args.append(current)
            current = []
        else:
            current.append(token)
    raise ValueError('unterminated variable macro')


def declarations(source, language):
    ts, rows = census.tokens(source, language), []
    for i, (token, offset) in enumerate(ts):
        if language == 'c':
            if token not in GNU_MACROS or i + 1 >= len(ts) or ts[i + 1][0] != '(':
                continue
            line = source[source.rfind('\n', 0, offset) + 1:offset]
            if re.match(r'\s*#\s*define\s*$', line):
                continue
            args = arguments(ts, i + 1)
            if len(args) < 2 or len(args[0]) != 1:
                raise ValueError('unsupported GNU variable declaration')
            name = census.literal(args[0][0])
        else:
            if token not in {'defvar', 'defvar_bool'} or [t[0] for t in ts[i + 1:i + 3]] != ['!', '(']:
                continue
            args = arguments(ts, i + 2)
            if len(args) not in {1, 2, 3} or len(args[0]) != 1 or not re.fullmatch(r'[A-Z_][A-Z_0-9]*', args[0][0]):
                raise ValueError('unsupported Rune variable declaration')
            ident = args[0][0]
            # As in build.rs: two arguments mean ident+VALUE, not ident+NAME.
            if len(args) == 3:
                if len(args[1]) != 1:
                    raise ValueError('unsupported Rune variable name expression')
                name = census.literal(args[1][0])
            else:
                name = (':' + ident[3:] if ident.startswith('KW_') else ident).lower().replace('_', '-')
        rows.append({'name': name, 'kind': token,
                     'line': source.count('\n', 0, offset) + 1})
    return rows


def scan(root, language):
    bases = [root / 'src'] if language == 'c' else [root / 'src', root / 'crates']
    if not bases[0].is_dir():
        raise ValueError(f'expected source directory: {bases[0]}')
    extensions = {'.c', '.m'} if language == 'c' else {'.rs'}
    files = sorted({p for base in bases if base.is_dir() for p in base.rglob('*')
                    if p.is_file() and p.suffix in extensions})
    rows = []
    for path in files:
        try:
            found = declarations(path.read_text(encoding='utf-8'), language)
        except (ValueError, UnicodeError) as exc:
            raise ValueError(f'{path}: {exc}') from exc
        rows.extend(dict(row, file=path.relative_to(root).as_posix()) for row in found)
    if not rows:
        raise ValueError(f'no variable declarations found beneath {root}')
    return rows


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--gnu-root', type=Path, required=True)
    parser.add_argument('--rune-root', type=Path, default=Path('.'))
    parser.add_argument('--gnu-revision', required=True)
    parser.add_argument('--rune-revision', required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    try:
        data = census.report(scan(args.gnu_root, 'c'), scan(args.rune_root, 'rust'),
                             args.gnu_revision, args.rune_revision)
    except (ValueError, OSError) as exc:
        parser.error(str(exc))
    data['measurement'] = 'declared-c-variable-coverage'
    data['limitations'] = [
        'Lexical declarations only, including inactive conditional branches.',
        'No claim about runtime registration, defaults, forwarding, locality or correctness.',
        'Only six listed GNU DEFVAR macros and direct Rune defvar/defvar_bool calls; no macro expansion.']
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(data, indent=2) + '\n', encoding='utf-8')


if __name__ == '__main__':
    main()
