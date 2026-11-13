from pathlib import Path
import json
import subprocess
import tempfile
import sys
import unittest
from unittest import mock

SCRIPTS = Path(__file__).parents[1]
sys.path.insert(0, str(SCRIPTS))
import primitive_probes as probes


class PrimitiveProbeTests(unittest.TestCase):
    def test_reproducible_bounded_generation(self):
        cases = probes.generate(42, 200)
        self.assertEqual(cases, probes.generate(42, 200))
        self.assertNotEqual(cases, probes.generate(43, 200))
        self.assertEqual({r['family'] for r in cases}, set(probes.FAMILIES))
        self.assertEqual(len({r['id'] for r in cases}), 200)
        self.assertTrue(all(not any(c in r['form'] for c in '\n\r;\x00') for r in cases))
        self.assertTrue(all(r['form'].startswith('(') and r['form'].endswith(')') for r in cases))

    def test_conditions_not_falsely_reported_as_runtime_failure(self):
        cases = probes.generate(1, 2)
        runtime = {'status': 'completed', 'returncode': 0,
                   'results': {0: '(value nil)', 1: '(error wrong-type-argument (listp 1))'}, 'output': ''}
        with mock.patch.object(probes, 'run_batch', return_value=runtime):
            report = probes.probe(cases, ['unused'], 1)
        self.assertFalse(report['runtime_finding'])
        self.assertEqual(report['counts'], {'condition': 1, 'value': 1})
        self.assertIsNone(report['first_unobserved_candidate'])

    def test_partial_run_preserves_reproduction_and_candidate(self):
        cases = probes.generate(2, 3)
        runtime = {'status': 'timeout', 'returncode': -9, 'results': {0: '(value 3)'}, 'output': 'partial'}
        with mock.patch.object(probes, 'run_batch', return_value=runtime):
            report = probes.probe(cases, ['unused'], 1)
        self.assertTrue(report['runtime_finding'])
        self.assertEqual(report['first_unobserved_candidate'], cases[1]['id'])
        self.assertEqual(report['observations'][1]['form'], cases[1]['form'])
        self.assertEqual(report['observations'][2]['status'], 'not-observed')

    def test_cli_report_and_runtime_failure(self):
        fake = """import re,sys
s=sys.stdin.read();m=re.search(r'RUNE-STDIN-DIFF-[a-f0-9]+',s).group()
n=s.count('(condition-case err')
for i in range(n): print('("'+m+'" '+str(i)+' (value nil))')
print('("'+m+'" done '+str(n)+')')
"""
        with tempfile.TemporaryDirectory(prefix='primitive-probe-tests-') as tmp:
            output = Path(tmp) / 'report.json'
            command = [sys.executable, str(SCRIPTS / 'primitive_probes.py'),
                       '--binary', sys.executable, '--arg=-c', '--arg=' + fake,
                       '--seed=42', '--count=2', '--revision=fake', '--output', str(output)]
            result = subprocess.run(command, capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stderr)
            report = json.loads(output.read_text())
            self.assertEqual(report['result']['counts'], {'value': 2})
            self.assertEqual(report['replay_cases'], [
                {'id': case['id'], 'form': case['form']} for case in probes.generate(42, 2)])
            command[command.index('--arg=' + fake)] = '--arg=pass'
            result = subprocess.run(command, capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertTrue(json.loads(output.read_text())['result']['runtime_finding'])

    def test_cli_argument_bounds(self):
        for option in ('--count=0', '--count=5001', '--timeout=nan', '--timeout=inf', '--timeout=0'):
            with self.subTest(option=option), mock.patch.object(sys, 'argv', [
                    'probes', '--revision=fake', '--output=unused.json', option]), \
                    mock.patch('sys.stderr'), self.assertRaises(SystemExit) as exc:
                probes.main()
            self.assertEqual(exc.exception.code, 2)


if __name__ == '__main__':
    unittest.main()
