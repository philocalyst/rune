import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

SCRIPTS = Path(__file__).parents[1]
sys.path.insert(0, str(SCRIPTS))
import probe_replay as replay


def report():
    return {'schema_version': 1, 'measurement': 'bounded-primitive-form-probes',
            'revision': 'original-fixture', 'replay_cases': [
                {'id': 'first', 'form': '(+ 1 2)'}, {'id': 'candidate', 'form': '(/ 1 0)'}],
            'result': {'runtime': {'status': 'incomplete'}, 'first_unobserved_candidate': 'candidate'}}


class ProbeReplayTests(unittest.TestCase):
    def test_saved_selection_and_explicit_override(self):
        with tempfile.TemporaryDirectory(prefix='probe-replay-tests-') as tmp:
            path = Path(tmp) / 'source.json'
            path.write_text(json.dumps(report()))
            case, revision, status = replay.read_candidate(path)
            self.assertEqual(case, {'id': 'candidate', 'form': '(/ 1 0)'})
            self.assertEqual(revision, 'original-fixture')
            self.assertEqual(status, 'incomplete')
            self.assertEqual(replay.read_candidate(path, 'first')[0]['id'], 'first')
            with self.assertRaises(ValueError):
                replay.read_candidate(path, 'missing')

    def test_invalid_reports_rejected(self):
        bad = [{}, dict(report(), schema_version=True), dict(report(), measurement='other'),
               dict(report(), replay_cases=[]), dict(report(), replay_cases=[{'id': 'x', 'form': '1; comment'}]),
               dict(report(), replay_cases=[{'id': 'x', 'form': '1'}, {'id': 'x', 'form': '2'}]),
               dict(report(), result={'runtime': {'status': 'completed'}, 'first_unobserved_candidate': None})]
        with tempfile.TemporaryDirectory(prefix='probe-replay-invalid-') as tmp:
            path = Path(tmp) / 'source.json'
            for data in bad:
                path.write_text(json.dumps(data))
                with self.subTest(data=data), self.assertRaises(ValueError):
                    replay.read_candidate(path)
            path.write_text('{"schema_version":1,"schema_version":1}')
            with self.assertRaises(ValueError):
                replay.read_candidate(path)

    def test_runtime_completion_not_semantic_or_same_failure_claim(self):
        case = {'id': 'test', 'form': '(car 1)'}
        runtime = {'status': 'completed', 'returncode': 0, 'results': {0: '(error error "bad type")'}, 'output': ''}
        with mock.patch.object(replay, 'run_batch', return_value=runtime):
            result = replay.replay(case, ['unused'], 1)
        self.assertFalse(result['isolated_runtime_failure'])
        self.assertEqual(result['assessment'], 'isolated-runtime-completed')
        runtime = dict(runtime, status='incomplete', returncode=101, results={})
        with mock.patch.object(replay, 'run_batch', return_value=runtime):
            result = replay.replay(case, ['unused'], 1)
        self.assertTrue(result['isolated_runtime_failure'])
        self.assertIsNone(result['observation'])

    def test_cli_bounded_report_and_invalid_preservation(self):
        fake = '''import re,sys
s=sys.stdin.read();m=re.search(r'RUNE-STDIN-DIFF-[a-f0-9]+',s).group()
print('("'+m+'" 0 (error arithmetic-error nil))')
print('("'+m+'" done 1)')
'''
        with tempfile.TemporaryDirectory(prefix='probe-replay-cli-') as tmp:
            root = Path(tmp)
            source, output = root / 'source.json', root / 'output.json'
            source.write_text(json.dumps(report()))
            command = [sys.executable, str(SCRIPTS / 'probe_replay.py'), str(source),
                       '--binary', sys.executable, '--arg=-c', '--arg=' + fake,
                       '--revision=replay-fixture', '--output', str(output)]
            result = subprocess.run(command, capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stderr)
            data = json.loads(output.read_text())
            self.assertEqual(data['revision'], 'replay-fixture')
            self.assertEqual(data['original_revision'], 'original-fixture')
            self.assertEqual(data['case']['id'], 'candidate')
            original = output.read_bytes()
            source.write_text('{}')
            result = subprocess.run(command, capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 2)
            self.assertEqual(output.read_bytes(), original)


if __name__ == '__main__':
    unittest.main()
