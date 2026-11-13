"""Optional real-process CLI loading regression checks; never build a runtime."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


@unittest.skipUnless(os.environ.get('RUNE_TEST_BINARY'), 'existing real Rune binary not supplied')
class RealCommandLineLoadTests(unittest.TestCase):
    def run_file(self, directory, name):
        binary = str(Path(os.environ['RUNE_TEST_BINARY']).resolve())
        return subprocess.run([binary, '--no-bootstrap', '--load', name], cwd=directory,
                              capture_output=True, text=True, timeout=10)

    def test_relative_existing_file_outside_default_load_path(self):
        with tempfile.TemporaryDirectory(prefix='rune-cli-load-') as tmp:
            name = 'relative file \u03bb.el'
            Path(tmp, name).write_text('(setq cli-smoke-value 42)\n(+ cli-smoke-value 1)\n', encoding='utf-8')
            result = self.run_file(tmp, name)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIn('true', result.stdout)

    def test_missing_and_invalid_relative_files_are_not_successes(self):
        with tempfile.TemporaryDirectory(prefix='rune-cli-load-') as tmp:
            missing = self.run_file(tmp, 'missing-regression-file.el')
            self.assertNotEqual(missing.returncode, 0)
            Path(tmp, 'invalid.el').write_text('(if)', encoding='utf-8')
            invalid = self.run_file(tmp, 'invalid.el')
            self.assertNotEqual(invalid.returncode, 0)


if __name__ == '__main__':
    unittest.main()
