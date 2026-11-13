import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPTS = Path(__file__).parents[1]
sys.path.insert(0, str(SCRIPTS))
import variable_census as variables


class VariableCensusTests(unittest.TestCase):
    def test_rune_forms_and_value_not_name(self):
        source = '''defvar!(ONE);
defvar!(TWO, "value-not-name");
defvar!(THREE, "renamed", list![format!("{}", value), (1, 2)]);
defvar_bool!(FOUR, true);
defvar!(KW_EXAMPLE);
'''
        self.assertEqual([r['name'] for r in variables.declarations(source, 'rust')],
                         ['one', 'two', 'renamed', 'four', ':example'])

    def test_gnu_kinds_and_comments(self):
        source = '#define DEFVAR_LISP(name, value) whatever\n'
        source += '/* DEFVAR_LISP("hidden", Vhidden) */\n'
        source += 'const char *s = "DEFVAR_INT(\\"hidden\\", Vhidden)";\n'
        source += '\n'.join(f'{kind}("variable-{i}", value, doc: /* prose */);'
                            for i, kind in enumerate(sorted(variables.GNU_MACROS)))
        rows = variables.declarations(source, 'c')
        self.assertEqual(len(rows), 6)
        self.assertEqual({r['kind'] for r in rows}, variables.GNU_MACROS)

    def test_invalid_forms_fail(self):
        for source, language in [
            ('DEFVAR_LISP("a" "b", Vab)', 'c'),
            ('DEFVAR_BOOL(NAME, var)', 'c'),
            ('defvar!(NAME, "a" + "b", nil);', 'rust'),
            ('defvar!(NAME, "a", list![1,2);', 'rust'),
            ('defvar!();', 'rust'), ('defvar!(NAME,1,2,3);', 'rust'),
            ('defvar!(NAME,', 'rust')]:
            with self.subTest(source=source), self.assertRaises(ValueError):
                variables.declarations(source, language)

    def test_cli_and_objective_c(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            for tree in ('gnu', 'rune'):
                (root / tree / 'src').mkdir(parents=True)
            (root / 'gnu/src/platform.m').write_text('DEFVAR_BOOL("example", flag);')
            (root / 'rune/src/test.rs').write_text('defvar_bool!(EXAMPLE, false);')
            output = root / 'variables.json'
            command = [sys.executable, str(SCRIPTS / 'variable_census.py'),
                       '--gnu-root', str(root / 'gnu'), '--rune-root', str(root / 'rune'),
                       '--gnu-revision', 'gnu-fixture', '--rune-revision', 'rune-fixture',
                       '--output', str(output)]
            result = subprocess.run(command, capture_output=True, text=True, timeout=5)
            self.assertEqual(result.returncode, 0, result.stderr)
            data = json.loads(output.read_text())
            self.assertEqual(data['measurement'], 'declared-c-variable-coverage')
            self.assertEqual(data['totals']['declared_coverage'], 1)
            self.assertEqual(data['declarations']['gnu'][0]['kind'], 'DEFVAR_BOOL')
            (root / 'gnu/src/platform.m').unlink()
            original = output.read_bytes()
            result = subprocess.run(command, capture_output=True, text=True, timeout=5)
            self.assertEqual(result.returncode, 2)
            self.assertEqual(output.read_bytes(), original)


if __name__ == '__main__':
    unittest.main()
