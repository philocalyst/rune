import importlib.util
from pathlib import Path
import sys
import unittest
from unittest import mock

spec = importlib.util.spec_from_file_location('ert_runner', Path(__file__).parents[1] / 'run_upstream_ert.py')
ert = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ert)


class RunnerTests(unittest.TestCase):
    def test_protocol(self):
        tests, events = ert.parse_output('noise\nM\tloaded\tnil\nM\tpassed\ttest-one\nM\tunimplemented\ttest-two\nM\tdone\tnil', 'M')
        self.assertEqual(tests, {'test-one': 'passed', 'test-two': 'unimplemented'})
        self.assertEqual(events, {'loaded', 'done'})

    def test_duplicate_rejected(self):
        with self.assertRaises(ValueError):
            ert.parse_output('M\tpassed\ta\nM\tfailed\ta', 'M')

    def test_invalid_protocol_events_rejected(self):
        for output in ('M\tselected\t1\nM\tselected\t2',
                       'M\tselected\t-1', 'M\tselected\tnope',
                       'M\tbogus\tnil', 'M\tdone\tnil\nM\tdone\tnil'):
            with self.subTest(output=output), self.assertRaises(ValueError):
                ert.parse_output(output, 'M')

    def test_nonfinite_timeout_rejected(self):
        for timeout in ('nan', 'inf', '-inf', '0', '-1'):
            with self.subTest(timeout=timeout), mock.patch.object(sys, 'argv', [
                    'runner', 'dummy.el', '--output', 'unused.json', '--timeout=' + timeout]), \
                    mock.patch('sys.stderr'), self.assertRaises(SystemExit) as error:
                ert.main()
            self.assertEqual(error.exception.code, 2)

    def test_exit_zero_not_success(self):
        row = ert.run_file(Path('dummy.el'), [sys.executable, '-c', 'pass'], 2)
        self.assertEqual(row['status'], 'could-not-load')

    def test_timeout(self):
        row = ert.run_file(Path('dummy.el'), [sys.executable, '-c', 'import time; time.sleep(10)'], .05)
        self.assertEqual(row['status'], 'timeout')

    def test_launch_error(self):
        self.assertEqual(ert.run_file(Path('dummy.el'), ['/no/such/rune'], 1)['status'], 'launch-error')

    def test_skip(self):
        self.assertEqual(ert.run_file(Path('comp-tests.el'), ['/no/such/rune'], 1)['status'], 'permanently-skipped')

    def test_removed_results_regress(self):
        before = {'files': [{'file': 'a.el', 'tests': {'one': 'passed', 'two': 'failed'}}]}
        self.assertEqual(ert.regressions(before, {'files': []}), [{'file': 'a.el', 'test': 'one'}])

    def test_output_limit(self):
        output, code, status = ert.capture([sys.executable, '-c',
            'import sys; sys.stdout.write("x" * 1000000)'], None, 2, limit=1024)
        self.assertEqual(status, 'output-limit')
        self.assertEqual(len(output), 1024)

    def test_descendant_pipe_does_not_hang(self):
        import os
        if os.name != 'posix':
            self.skipTest('POSIX group cleanup')
        output, code, status = ert.capture([sys.executable, '-c',
            'import subprocess; subprocess.Popen(["sleep", "30"]); print("done")'], None, 2)
        self.assertIsNone(status)
        self.assertEqual(output.strip(), 'done')

    def test_no_tests(self):
        code = '''import pathlib,re,sys
m=re.search(r'RUNE-ERT-[a-f0-9]+',pathlib.Path(sys.argv[-1]).read_text()).group()
for kind,name in [('loaded','nil'),('selected','0'),('done','nil')]: print(m+'\\t'+kind+'\\t'+name)
'''
        self.assertEqual(ert.run_file(Path('dummy.el'), [sys.executable, '-c', code], 2)['status'], 'no-tests')

    def test_incomplete_runs_retain_results(self):
        prefix = """import pathlib,re,sys,time
m=re.search(r'RUNE-ERT-[a-f0-9]+',pathlib.Path(sys.argv[-1]).read_text()).group()
for kind,name in [('loaded','nil'),('selected','2'),('passed','a')]:
 print(m+'\\t'+kind+'\\t'+name, flush=True)
"""
        cases = [
            ('time.sleep(10)', 'timeout'),
            ("print(m+'\\tdone\\tnil')", 'protocol-error'),
            ("print(m+'\\taborted\\tnil'); print(m+'\\tdone\\tnil')", 'runner-error'),
            ('sys.exit(1)', 'runner-error'),
        ]
        for tail, status in cases:
            with self.subTest(status=status):
                row = ert.run_file(Path('dummy.el'), [sys.executable, '-c', prefix + tail], .2)
                self.assertEqual(row['status'], status)
                self.assertEqual(row['tests'], {'a': 'passed'})

    def test_fake_protocol_process(self):
        # The stand-in reads the generated driver marker, not a fixed known token.
        code = '''import pathlib,re,sys
s=pathlib.Path(sys.argv[-1]).read_text()
m=re.search(r'RUNE-ERT-[a-f0-9]+',s).group()
for kind,name in [('loaded','nil'),('selected','1'),('passed','a'),('done','nil')]: print(m+'\\t'+kind+'\\t'+name)
'''
        row = ert.run_file(Path('dummy.el'), [sys.executable, '-c', code], 2)
        self.assertEqual(row['status'], 'completed')
        self.assertEqual(row['tests'], {'a': 'passed'})


if __name__ == '__main__':
    unittest.main()
