import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPTS = Path(__file__).parents[1]
sys.path.insert(0, str(SCRIPTS))
import panic_inventory as panic


class PanicInventoryTests(unittest.TestCase):
    def test_direct_syntax_and_locations(self):
        source = 'fn f() {\n value.unwrap(); value.expect("why"); std::panic!("x");\n todo!{}; unreachable![]; unimplemented!();\n}'
        rows = panic.sites(source)
        self.assertEqual([r['kind'] for r in rows],
                         ['.unwrap()', '.expect()', 'panic!', 'todo!', 'unreachable!', 'unimplemented!'])
        self.assertEqual(rows[0], {'kind': '.unwrap()', 'line': 2, 'column': 8})

    def test_comments_strings_and_similar_names_ignored(self):
        source = '''// todo!()
/* nested /* panic!() */ value.unwrap() */
const S: &str = r###"panic!(); value.expect(\"x\")"###;
fn unwrap() {} fn expect() {} unwrap(); expect();
object.unwrap_or(1); object.expected(); my_todo!(); r#panic!(); object.r#unwrap();
'''
        self.assertEqual(panic.sites(source), [])

    def test_tests_and_inactive_branches_explicitly_included(self):
        rows = panic.sites('#[cfg(test)] mod tests { fn test() { panic!(); } }\n#[cfg(any())] fn f() { todo!(); }')
        self.assertEqual([r['kind'] for r in rows], ['panic!', 'todo!'])

    def test_inventory_stable_and_invalid_input(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            with self.assertRaises(ValueError):
                panic.inventory(root, 'fixture')
            (root / 'src').mkdir()
            with self.assertRaises(ValueError):
                panic.inventory(root, 'fixture')
            (root / 'src/z.rs').write_text('fn z() { todo!(); }')
            (root / 'src/a.rs').write_text('fn a() {}')
            data = panic.inventory(root, 'fixture')
            self.assertEqual(data['files_scanned'], 2)
            self.assertEqual(list(data['files']), ['src/a.rs', 'src/z.rs'])
            self.assertEqual(data['totals']['todo!'], 1)
            self.assertEqual(data['totals']['panic!'], 0)
            self.assertEqual(data['sites'][0]['file'], 'src/z.rs')
            self.assertEqual(panic.inventory(root, 'fixture'), data)

    def test_cli_preserves_report_on_invalid_source(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / 'src').mkdir()
            source, output = root / 'src/test.rs', root / 'report.json'
            source.write_text('fn main() { todo!(); }')
            command = [sys.executable, str(SCRIPTS / 'panic_inventory.py'),
                       '--rune-root', str(root), '--revision', 'fixture', '--output', str(output)]
            result = subprocess.run(command, capture_output=True, text=True, timeout=5)
            self.assertEqual(result.returncode, 0, result.stderr)
            data = json.loads(output.read_text())
            self.assertEqual(data['totals']['todo!'], 1)
            original = output.read_bytes()
            source.write_text('/* unterminated')
            result = subprocess.run(command, capture_output=True, text=True, timeout=5)
            self.assertEqual(result.returncode, 2)
            self.assertEqual(output.read_bytes(), original)


if __name__ == '__main__':
    unittest.main()
