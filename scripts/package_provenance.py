#!/usr/bin/env python3
"""Bounded path-independent fingerprints of installed tracked package sources."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import stat
import subprocess

MAX_PACKAGES = 512
MAX_FILES = 100000
MAX_FILE_BYTES = 32 * 1024 * 1024
MAX_TOTAL_BYTES = 512 * 1024 * 1024


def git(repo, *args):
    return subprocess.check_output(['git', '-C', str(repo), *args], timeout=10)


def capture(repos, additional=None, source_trees=None):
    repos = Path(repos)
    if repos.is_symlink() or not repos.is_dir():
        raise ValueError('Package repository root must be a real directory')
    children = sorted(repos.iterdir(), key=lambda p: p.name)
    if len(children) > MAX_PACKAGES:
        raise ValueError('Too many package repositories')
    entries = [(repo.name, repo, True) for repo in children]
    for label, repo in sorted((additional or {}).items()):
        if any(name == label for name, _, _ in entries):
            raise ValueError('Duplicate source repository label')
        entries.append((label, Path(repo), True))
    for label, repo in sorted((source_trees or {}).items()):
        if any(name == label for name, _, _ in entries):
            raise ValueError('Duplicate source repository label')
        entries.append((label, Path(repo), False))
    if len(entries) > MAX_PACKAGES:
        raise ValueError('Too many source repositories')
    result = {}
    total_files = total_bytes = 0
    for label, repo, is_git in entries:
        if repo.is_symlink() or not repo.is_dir():
            raise ValueError('Package entries must be real repository directories')
        if is_git:
            revision = git(repo, 'rev-parse', 'HEAD').decode('ascii').strip()
            if len(revision) not in (40, 64) or any(c not in '0123456789abcdef' for c in revision):
                raise ValueError('Invalid package revision')
            names = git(repo, 'ls-files', '--recurse-submodules', '-z').split(b'\0')
            names = sorted(set(name for name in names if name))
        else:
            revision = None
            names = sorted(os.fsencode(str(path.relative_to(repo))) for path in repo.rglob('*')
                           if path.is_symlink() or not path.is_dir())
        total_files += len(names)
        if total_files > MAX_FILES:
            raise ValueError('Too many tracked package files')
        tree = hashlib.sha256()
        package_bytes = 0
        for raw in names:
            name = raw.decode('utf-8')
            relative = Path(name)
            if relative.is_absolute() or '..' in relative.parts:
                raise ValueError('Invalid tracked package path')
            path = repo / relative
            parent = path.parent
            while parent != repo:
                if parent.is_symlink() or not parent.is_dir():
                    raise ValueError('Tracked file has a non-directory or symlink parent')
                parent = parent.parent
            info = path.lstat()
            if stat.S_ISLNK(info.st_mode):
                data = os.fsencode(os.readlink(path))
                kind = 'symlink'
            elif stat.S_ISREG(info.st_mode):
                if info.st_size > MAX_FILE_BYTES:
                    raise ValueError('Package source exceeds per-file byte limit')
                with path.open('rb') as stream:
                    if not stat.S_ISREG(os.fstat(stream.fileno()).st_mode):
                        raise ValueError('Opened package source is not regular')
                    data = stream.read(MAX_FILE_BYTES + 1)
                kind = 'executable' if info.st_mode & 0o111 else 'regular'
            else:
                raise ValueError('Tracked package source is not regular or symlink: ' + repo.name + '/' + name)
            if len(data) > MAX_FILE_BYTES:
                raise ValueError('Package source exceeds per-file byte limit')
            package_bytes += len(data)
            total_bytes += len(data)
            if total_bytes > MAX_TOTAL_BYTES:
                raise ValueError('Package sources exceed total byte limit')
            entry = [name, kind, hashlib.sha256(data).hexdigest()]
            tree.update(json.dumps(entry, ensure_ascii=True, separators=(',', ':')).encode('ascii') + b'\n')
        result[label] = {'revision': revision, 'source_kind':'git-tracked' if is_git else 'directory-files', 'tracked_source_sha256': tree.hexdigest(),
                             'tracked_files': len(names), 'source_bytes': package_bytes}
    if not result:
        raise ValueError('No installed package repositories')
    return {'schema_version': 1, 'scope': 'git-tracked-working-tree-sources', 'packages': result}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--repos', required=True, type=Path)
    parser.add_argument('--doom-root', type=Path)
    parser.add_argument('--mu-lisp', type=Path)
    parser.add_argument('--output', required=True, type=Path)
    args = parser.parse_args()
    manifest = capture(args.repos, {'@doom': args.doom_root} if args.doom_root else None,
                       {'@mu4e': args.mu_lisp} if args.mu_lisp else None)
    args.output.write_text(json.dumps(manifest, indent=2, sort_keys=True) + '\n', encoding='utf-8')


if __name__ == '__main__':
    main()
