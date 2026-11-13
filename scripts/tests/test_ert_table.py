import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPTS = Path(__file__).parents[1]
sys.path.insert(0, str(SCRIPTS))
import ert_table as table


def report(rows, revision='fixture'):
    return {'schema_version': 1, 'revision': revision, 'files': rows}


def row(file, status, tests):
    return {'file': file, 'status': status, 'tests': tests}


class ErtTableTests(unittest.TestCase):
    def test_counts_are_recomputed_including_incomplete(self):
        item = row('suite.el', 'timeout', {'one': 'passed', 'two': 'failed',
                                         'three': 'skipped', 'four': 'unimplemented', 'five': 'errored'})
        item.update(counts={'passed': 999}, pass_rate=1)
        text = table.render(report([item]))
        self.assertIn('| suite.el | timeout | 1 | 1 | 1 | 1 | 1 | 20.0% (1/5) |', text)
        self.assertIn('observed test records only', text)
        self.assertIn('Completed files: 0.', text)

    def test_missing_files_and_results_are_regressions(self):
        old = report([row('gone.el', 'completed', {'first': 'passed'}),
                      row('partial.el', 'completed', {'keep': 'passed', 'lost': 'passed'})], 'old')
        now = report([row('partial.el', 'timeout', {'keep': 'passed'})], 'new')
        text = table.render(now, old)
        self.assertIn('| gone.el | missing-current | 0 | 0 | 0 | 0 | 0 | — | 1 |', text)
        self.assertIn('Formerly-passing test regressions: 2.', text)
        self.assertIn('| partial.el | lost |', text)
        self.assertNotIn('| partial.el | keep |', text)

    def test_escaping_and_no_records(self):
        text = table.render(report([row('<script>|[link]\n.el', 'could-not-load', {})], '<b>rev</b>'))
        self.assertNotIn('<script>', text)
        self.assertNotIn('<b>', text)
        self.assertIn('&lt;script&gt;\\|\\[link\\] .el', text)
        self.assertIn('| could-not-load | 0 | 0 | 0 | 0 | 0 | — |', text)

    def test_bad_reports_and_duplicates_rejected(self):
        bad = [report([row('a', 'completed', {})]),
               report([row('a', 'permanently-skipped', {'test': 'passed'})]),
               report([row('a', 'unknown', {})]),
               report([row('a', 'completed', {'test': 'bogus'})]),
               report([row('a', 'timeout', {}), row('a', 'timeout', {})]),
               {'schema_version': True, 'revision': 'x', 'files': []},
               {'schema_version': 2, 'revision': 'x', 'files': []},
               {'schema_version': 1, 'files': []}]
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / 'report.json'
            for data in bad:
                path.write_text(json.dumps(data))
                with self.subTest(data=data), self.assertRaises(ValueError):
                    table.read_report(path)
            path.write_text('{"schema_version":1,"revision":"x","files":[],"files":[]}')
            with self.assertRaises(ValueError):
                table.read_report(path)

    def test_cli_valid_and_invalid_preserves_output(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source, output = root / 'input.json', root / 'output.md'
            source.write_text(json.dumps(report([row('empty.el', 'no-tests', {})])))
            command = [sys.executable, str(SCRIPTS / 'ert_table.py'), str(source), '--output', str(output)]
            result = subprocess.run(command, capture_output=True, text=True, timeout=5)
            self.assertEqual(result.returncode, 0, result.stderr)
            original = output.read_bytes()
            self.assertIn(b'no-tests', original)
            source.write_text('{}')
            result = subprocess.run(command, capture_output=True, text=True, timeout=5)
            self.assertEqual(result.returncode, 2)
            self.assertEqual(output.read_bytes(), original)


if __name__ == '__main__':
    unittest.main()
