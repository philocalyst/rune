#!/usr/bin/env python3
"""Check exclusive-GC API rejection fixtures in a disposable source overlay.

Run only with an exclusive Cargo build slot. This never edits the checkout and
uses the provided warm target. The wrapper changes only the rune crate's source
input and emit mode; dependencies retain their ordinary cached compiler flags.
"""
import argparse
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target-dir", required=True, type=Path)
    parser.add_argument("--scratch", type=Path, default=None)
    parser.add_argument("--min-free-mib", type=int, default=350)
    args = parser.parse_args()
    repo = Path(__file__).resolve().parents[2]
    fixtures = [("gc_clear_alias.rs", "E0502"),
                ("gc_clear_bind_ref_alias.rs", "E0502"),
                ("gc_unchecked_root_access.rs", "E0133")]
    args.target_dir.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="rune-gc-ui-", dir=args.scratch) as directory:
        overlay = Path(directory)
        shutil.copytree(repo / "src", overlay / "src")
        for item in repo.iterdir():
            if item.name != "src":
                (overlay / item.name).symlink_to(item, target_is_directory=item.is_dir())
        original_main = (repo / "src/main.rs").read_text()
        wrapper = overlay / "rustc-wrapper.py"
        wrapper.write_text('''#!/usr/bin/env python3
import os, pathlib, sys
compiler, arguments = sys.argv[1], sys.argv[2:]
if "--crate-name" in arguments and arguments[arguments.index("--crate-name")+1] == "rune":
    original = pathlib.Path(os.environ["GC_UI_ORIGINAL_MAIN"]).resolve()
    arguments = [os.environ["GC_UI_OVERLAY_MAIN"] if pathlib.Path(a).resolve() == original else a for a in arguments]
    arguments = ["--emit=metadata" if a.startswith("--emit=") else a for a in arguments]
os.execv(compiler, [compiler, *arguments])
''')
        wrapper.chmod(0o755)
        env = dict(os.environ)
        env.update(CARGO_TARGET_DIR=str(args.target_dir.resolve()),
                   CARGO_PROFILE_TEST_DEBUG="0", RUSTC_WRAPPER=str(wrapper),
                   GC_UI_ORIGINAL_MAIN=str(repo / "src/main.rs"),
                   GC_UI_OVERLAY_MAIN=str(overlay / "src/main.rs"))
        for index, (fixture, expected) in enumerate(fixtures):
            source = repo / "scripts/tests/fixtures" / fixture
            (overlay / "src/main.rs").write_text(
                original_main + f'\n#[path = "{source}"]\nmod gc_negative_fixture;\n')
            log = args.target_dir / f"compile-fail-{index}.log"
            with log.open("w") as output:
                child = subprocess.Popen(
                    ["cargo", "rustc", "--offline", "--tests", "--",
                     "--cfg", f"gc_negative_probe_{index}"], cwd=repo, env=env,
                    stdout=output, stderr=subprocess.STDOUT, start_new_session=True)
                started = time.monotonic()
                stopped = False
                while child.poll() is None:
                    disk = os.statvfs(args.target_dir)
                    free = disk.f_bavail * disk.f_frsize // (1024 * 1024)
                    if free < args.min_free_mib or time.monotonic() - started > 180:
                        os.killpg(child.pid, signal.SIGTERM)
                        stopped = True
                        break
                    time.sleep(0.25)
                code = child.wait()
            diagnostic = log.read_text()
            error_lines = [line for line in diagnostic.splitlines() if line.startswith("error[E")]
            if stopped or code == 0 or not error_lines or any(
                    not line.startswith(f"error[{expected}]") for line in error_lines):
                print(f"FAIL {fixture}: unexpected compiler outcome {code}; {log}", flush=True)
                return 1
            print(f"PASS {fixture}: rejected with {expected}; {log}", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
