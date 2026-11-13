import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPTS = Path(__file__).parents[1]
sys.path.insert(0, str(SCRIPTS))
import run_stdin_differential as diff

CASES = [{'id': 'sum', 'form': '(+ 1 2)'}, {'id': 'condition', 'form': '(car 1)'}]
FAKE = '''import pathlib,re,sys
s=pathlib.Path(sys.argv[-1]).read_text() if '--load' in sys.argv else sys.stdin.read()
m=re.search(r'RUNE-STDIN-DIFF-[a-f0-9]+',s).group()
n=s.count('(condition-case err')
for i in range(n): print('("'+m+'" '+str(i)+' '+('(value 3)' if i==0 else '(error test-condition nil)')+')')
print('("'+m+'" done '+str(n)+')')
'''


class StdinDifferentialTests(unittest.TestCase):
    def test_rune_driver_uses_only_supported_direct_bridge(self):
        script = diff.driver(CASES, 'M', 'rune')
        for absent in ('(read ', '(eval ', '(princ ', '(prin1 '):
            self.assertNotIn(absent, script)
        self.assertEqual(len(script.splitlines()), len(CASES) + 1)
        self.assertIn('(progn (+ 1 2))', script)
        self.assertIn('(list "M" \'done 2)', script)
        self.assertIn('(prin1 rune-diff-observation)', diff.driver(CASES, 'M', 'emacs'))

    def test_protocol_rejects_corruption(self):
        self.assertEqual(diff.parse('noise\n("M" 0 (value nil))\n("M" done 1)', 'M', 1),
                         ({0: '(value nil)'}, True))
        for output in ['("M" 1 (value 3))', '("M" 00 (value 3))',
                       '("M" 0 (value 3))\n("M" 0 (value 3))',
                       '("M" done 1)\n("M" 0 (value 3))',
                       '("M" done 9)', '("M" 0 garbage)',
                       '("M" done 1)\n("M" done 1)']:
            with self.subTest(output=output), self.assertRaises(ValueError):
                diff.parse(output, 'M', 1)

    def test_both_fake_adapters_and_comparison(self):
        rune = diff.run_batch(CASES, [sys.executable, '-c', FAKE], 'rune', 2)
        oracle = diff.run_batch(CASES, [sys.executable, '-c', FAKE], 'emacs', 2)
        self.assertEqual(rune['status'], 'completed')
        self.assertEqual(oracle['status'], 'completed')
        self.assertEqual([r['status'] for r in diff.compare(CASES, rune, oracle)], ['matched', 'matched'])
        other = dict(oracle, results={0: '(value 4)'})
        self.assertEqual([r['status'] for r in diff.compare(CASES, rune, other)], ['mismatched', 'not-observed'])

    def test_timeout_and_zero_exit_without_records(self):
        code = FAKE[:FAKE.index('for i in range(n):')] + '''print('("'+m+'" 0 (value 3))',flush=True)
import time;time.sleep(10)
'''
        run = diff.run_batch(CASES, [sys.executable, '-c', code], 'rune', 2)
        self.assertEqual(run['status'], 'timeout')
        self.assertEqual(run['results'], {0: '(value 3)'})
        self.assertEqual(diff.run_batch(CASES, [sys.executable, '-c', 'pass'], 'rune', 2)['status'], 'incomplete')
        missing = diff.run_batch(CASES, ['/no/such/rune'], 'rune', 2)
        self.assertNotEqual(missing['status'], 'completed')

    def test_validation_and_cli(self):
        with tempfile.TemporaryDirectory(prefix='stdin-diff-tests-') as tmp:
            root = Path(tmp)
            source, output = root / 'cases.json', root / 'output.json'
            for data in [[], [{'id': 'a', 'form': '1\n2'}], [{'id': 'a', 'form': '1; comment'}],
                         [{'id': 'a', 'form': '1'}, {'id': 'a', 'form': '2'}],
                         [{'id': 'a', 'form': '1', 'extra': 0}]]:
                source.write_text(json.dumps(data))
                with self.subTest(data=data), self.assertRaises(ValueError):
                    diff.read_cases(source)
            source.write_text(json.dumps(CASES))
            command = [sys.executable, str(SCRIPTS / 'run_stdin_differential.py'), str(source),
                       '--rune', sys.executable, '--oracle', sys.executable,
                       '--rune-arg=-c', '--rune-arg=' + FAKE, '--oracle-arg=-c', '--oracle-arg=' + FAKE,
                       '--revision', 'fake', '--oracle-revision', 'fake', '--output', str(output)]
            result = subprocess.run(command, capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(json.loads(output.read_text())['measurement'], 'stdin-exact-printer-differential')
            original = output.read_bytes()
            source.write_text('[]')
            result = subprocess.run(command, capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 2)
            self.assertEqual(output.read_bytes(), original)

    @unittest.skipUnless(os.environ.get('RUNE_TEST_BINARY'), 'optional existing real Rune binary not supplied')
    def test_existing_real_rune_bridge(self):
        run = diff.run_batch(CASES, [os.environ['RUNE_TEST_BINARY']], 'rune', 5)
        self.assertEqual(run['status'], 'completed', run['output'])
        self.assertEqual(run['results'][0], '(value 3)')
        self.assertTrue(run['results'][1].startswith('(error '))


if __name__ == '__main__':
    unittest.main()
