import importlib.util
from pathlib import Path
import tempfile
import json
import subprocess
import sys
import unittest

spec = importlib.util.spec_from_file_location('census', Path(__file__).parents[1] / 'census.py')
census = importlib.util.module_from_spec(spec)
spec.loader.exec_module(census)


class CensusTests(unittest.TestCase):
    def test_rust_attributes_and_false_positives(self):
        source = '''// #[defun] fn fake() {}
/* nested /* #[defun] */ fn fake() {} */
const DOC: &str = r###"#[defun] fn fake() {}"###;
#[defun(
 name = "actual-name", arity = "1"
)]
pub(crate) fn rust_name() {}
#[defun]
fn default_name() {}
'''
        self.assertEqual(census.declarations(source, 'rust'), [
            {'name': 'actual-name', 'line': 4}, {'name': 'default-name', 'line': 8}])

    def test_c_comments_do_not_nest_but_rust_comments_do(self):
        source = '/* outer /* inner */ DEFUN("real", Freal) /* tail */'
        self.assertEqual(census.declarations(source, 'c'), [{'name': 'real', 'line': 1}])
        source = '/* outer /* inner */ #[defun] fn hidden() {} */ #[defun] fn visible() {}'
        self.assertEqual(census.declarations(source, 'rust'), [{'name': 'visible', 'line': 1}])

    def test_c_multiline_and_comments(self):
        source = '''#define DEFUN(lname, fn, ...) anything
/* DEFUN("fake", fake) */
const char *doc = "DEFUN(\\"fake\\", fake)";
DEFUN (
 "real-primitive", Freal, Sreal, 0, 1, 0,
 doc: /* documentation */)
'''
        self.assertEqual(census.declarations(source, 'c'), [{'name': 'real-primitive', 'line': 4}])

    def test_reject_unsupported_names_and_malformed_source(self):
        for source, language in [('DEFUN(NAME, f)', 'c'),
                                 ('DEFUN("x" "y", Fx)', 'c'),
                                 ('#[defun(name = "x" + "y")] fn x() {}', 'rust'),
                                 ('#[defun(name = other)] fn f() {}', 'rust'),
                                 ('#[defun] struct Other {} fn later() {}', 'rust'),
                                 ('/* unfinished', 'rust'),
                                 ('const S: &str = r#"unfinished;', 'rust')]:
            with self.subTest(source=source), self.assertRaises(ValueError):
                census.declarations(source, language)

    def test_report_unique_counts_and_duplicates(self):
        gnu = [{'name': name, 'file': file, 'line': line} for name, file, line in [
            ('b', 'src/b.c', 1), ('a', 'src/a.c', 2), ('a', 'src/b.c', 3)]]
        rune = [{'name': 'a', 'file': 'src/a.rs', 'line': 1},
                {'name': 'extra', 'file': 'src/a.rs', 'line': 2}]
        data = census.report(gnu, rune, 'gnu-test', 'rune-test')
        self.assertEqual(data['totals']['declared'], 2)
        self.assertEqual(data['totals']['declared_coverage'], .5)
        self.assertEqual(data['totals']['missing_names'], ['b'])
        self.assertEqual(list(data['files']), ['src/a.c', 'src/b.c'])
        self.assertEqual(data['rune_only_names'], ['extra'])
        self.assertEqual(len(data['duplicates']['gnu']['a']), 2)

    def test_cli_report_and_missing_input_failure(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            for tree in ('gnu', 'rune'):
                (root / tree / 'src').mkdir(parents=True)
            (root / 'gnu/src/data.c').write_text('DEFUN("actual-name", Factual)')
            (root / 'rune/src/data.rs').write_text('#[defun] fn actual_name() {}')
            output = root / 'report.json'
            command = [sys.executable, str(Path(census.__file__)),
                       '--gnu-root', str(root / 'gnu'), '--rune-root', str(root / 'rune'),
                       '--gnu-revision', 'gnu-fixture', '--rune-revision', 'rune-fixture',
                       '--output', str(output)]
            result = subprocess.run(command, capture_output=True, text=True, timeout=5)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(json.loads(output.read_text())['totals']['declared_coverage'], 1)
            original = output.read_bytes()
            (root / 'gnu/src/data.c').unlink()
            result = subprocess.run(command, capture_output=True, text=True, timeout=5)
            self.assertEqual(result.returncode, 2)
            self.assertEqual(output.read_bytes(), original)

    def test_scan_paths_order_and_empty_input(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            with self.assertRaises(ValueError):
                census.scan(root, 'rust')
            (root / 'src').mkdir()
            with self.assertRaises(ValueError):
                census.scan(root, 'rust')
            (root / 'src/z.rs').write_text('#[defun] fn z() {}')
            (root / 'src/a.rs').write_text('#[defun] fn a() {}')
            self.assertEqual([r['name'] for r in census.scan(root, 'rust')], ['a', 'z'])


if __name__ == '__main__':
    unittest.main()
