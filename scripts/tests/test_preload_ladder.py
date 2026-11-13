import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPTS = Path(__file__).parents[1]
sys.path.insert(0, str(SCRIPTS))
import preload_ladder as ladder

FILES = [{'file': 'one.el', 'path': '/source/one.el'},
         {'file': 'two.el', 'path': '/source/two.el'},
         {'file': 'three.el', 'path': '/source/three.el'}]
FAKE = '''import re,sys
s=sys.stdin.read()
m=re.search(r'RUNE-LADDER-[a-f0-9]+',s).group()
n=s.count('(if rune-preload-running')
for i in range(n): print('("'+m+'" '+str(i)+' loaded)')
print('("'+m+'" done '+str(n)+')')
'''


class PreloadLadderTests(unittest.TestCase):
    def test_driver_needs_no_unexposed_read_eval_or_print(self):
        script = ladder.driver(FILES, 'M')
        for unavailable in ('(read ', '(eval ', '(princ ', '(prin1 '):
            self.assertNotIn(unavailable, script)
        self.assertIn('(load "/source/one.el" nil t)', script)
        self.assertIn('(setq load-path (list ' + ladder.literal(str(Path('/source'))) + '))', script)
        self.assertEqual(script.count('(if rune-preload-running'), 3)
        self.assertIn('(list "M" \'done 3)', script)
        self.assertEqual(len(script.splitlines()), len(FILES) + 3)

    def test_parse_blocker_and_protocol_errors(self):
        prefix = '("M" '
        output = '\n'.join(prefix + line for line in
                           ('0 loaded)', '1 load-error)', '2 not-attempted)', 'done 3)'))
        self.assertEqual(ladder.parse(output, 'M', 3),
                         ({0: 'loaded', 1: 'load-error', 2: 'not-attempted'}, True))
        for output in ['("M" 1 loaded)', '("M" 0 loaded)\n("M" 0 loaded)',
                       '("M" 0 not-attempted)', '("M" 0 load-error)\n("M" 1 loaded)',
                       '("M" done 3)\n("M" 0 loaded)', '("M" done 9)',
                       '("M" 00 loaded)', '("M" done 3)\n("M" done 3)']:
            with self.subTest(output=output), self.assertRaises(ValueError):
                ladder.parse(output, 'M', 3)

    def test_fake_runtime_success_and_first_failure(self):
        run = ladder.run(FILES, [sys.executable, '-c', FAKE], 2)
        self.assertEqual(run['status'], 'completed')
        self.assertEqual(run['loaded_prefix'], 3)
        self.assertIsNone(run['first_blocker'])
        failed = FAKE.replace("str(i)+' loaded)'", "str(i)+' '+('loaded' if i==0 else 'load-error' if i==1 else 'not-attempted')+')'")
        run = ladder.run(FILES, [sys.executable, '-c', failed], 2)
        self.assertEqual(run['status'], 'completed')
        self.assertEqual(run['loaded_prefix'], 1)
        self.assertEqual(run['first_blocker'], 'two.el')
        self.assertEqual(run['files'][2]['status'], 'not-attempted')

    def test_incomplete_timeout_retains_prefix(self):
        code = FAKE[:FAKE.index('for i in range(n):')] + '''print('("'+m+'" 0 loaded)',flush=True)
import time;time.sleep(10)
'''
        run = ladder.run(FILES, [sys.executable, '-c', code], 2)
        self.assertEqual(run['status'], 'timeout')
        self.assertEqual(run['loaded_prefix'], 1)
        self.assertEqual(run['files'][1]['status'], 'not-observed')
        self.assertIsNone(run['first_blocker'])
        self.assertEqual(ladder.run(FILES, [sys.executable, '-c', 'pass'], 2)['status'], 'incomplete')

    def test_manifest_validation(self):
        with tempfile.TemporaryDirectory(prefix='preload-manifest-tests-') as tmp:
            root = Path(tmp)
            manifest = root / 'manifest.json'
            for data in [[], ['../escape.el'], ['no-suffix'], ['same.el', './same.el'],
                         [1], ['/absolute.el'], ['zero\x00.el']]:
                manifest.write_text(json.dumps(data))
                with self.subTest(data=data), self.assertRaises(ValueError):
                    ladder.read_manifest(manifest, root)
            manifest.write_text('["missing.el"]')
            self.assertEqual(ladder.read_manifest(manifest, root)[0]['file'], 'missing.el')

    def test_cli_failure_writes_report(self):
        with tempfile.TemporaryDirectory(prefix='preload-cli-tests-') as tmp:
            root = Path(tmp)
            manifest, output = root / 'manifest.json', root / 'report.json'
            manifest.write_text('["one.el"]')
            command = [sys.executable, str(SCRIPTS / 'preload_ladder.py'), str(manifest),
                       '--lisp-root', str(root), '--binary', sys.executable,
                       '--arg=-c', '--arg=' + FAKE, '--revision', 'fake',
                       '--source-revision', 'fixture', '--output', str(output)]
            result = subprocess.run(command, capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(json.loads(output.read_text())['run']['loaded_prefix'], 1)
            failed = command[:]
            failed[failed.index('--arg=' + FAKE)] = '--arg=pass'
            result = subprocess.run(failed, capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertEqual(json.loads(output.read_text())['run']['status'], 'incomplete')


if __name__ == '__main__':
    unittest.main()
