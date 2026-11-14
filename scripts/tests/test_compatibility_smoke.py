import sys
from pathlib import Path
import tempfile
import unittest
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import compatibility_smoke as smoke

class CompatibilitySmokeTests(unittest.TestCase):
    def run_report(self, actual='(value t)', selected=(), mutate=False):
        with tempfile.TemporaryDirectory() as tmp:
            directory=Path(tmp)
            binary=directory/'fake-binary'
            binary.write_bytes(b'fake executable only for mocked capture')
            calls=[]
            def runner(cases,command,flavor,timeout,cwd):
                calls.append((cases,command,flavor,timeout,cwd))
                if mutate:
                    (directory/'synthetic.org').write_text('unexpected mutation')
                return {'status':'completed','returncode':0,'results':{0:actual},'output':'fixture-output'}
            report=smoke.execute(binary,binary,'mock-only','mock-only',2,directory,runner=runner,selected=selected)
            return report,calls
    def test_true_required_not_matching_errors(self):
        report,calls=self.run_report('(error void-function missing)')
        self.assertEqual(report['summary']['rune']['passed'],0)
        self.assertEqual(report['first_rune_failure']['id'],'core-text')
        self.assertEqual(len(calls),18)
        self.assertTrue(all(report['fixtures_unchanged'].values()))
    def test_pass_and_selected_gate(self):
        report,calls=self.run_report(selected=('org-file-read',))
        self.assertEqual(report['summary']['rune']['passed'],1)
        self.assertIsNone(report['first_rune_failure'])
        self.assertEqual(len(calls),2)
        self.assertIn('insert-file-contents',calls[0][0][0]['form'])
        self.assertIn('isolated-home',calls[0][1][3])
    def test_synthetic_disk_mutation_never_hidden(self):
        report,_=self.run_report(mutate=True)
        self.assertFalse(report['fixtures_unchanged']['org'])
        self.assertTrue(report['fixtures_unchanged']['mail'])
    def test_generated_forms_are_single_line_and_no_mail_retrieval(self):
        rows=smoke.gates({'org':Path('/synthetic.org'),'mail':Path('/synthetic.mbox')})
        for _,_,form in rows:
            self.assertNotIn('\n',form)
            self.assertNotIn(';',form)
            self.assertNotIn('(rmail ',form)
            self.assertNotIn('send-mail',form)
        self.assertEqual(smoke.MAIL.count('From '),2)
        self.assertIn('example.invalid',smoke.MAIL)
    def test_incomplete_transport_is_not_pass_even_true(self):
        with tempfile.TemporaryDirectory() as tmp:
            p=Path(tmp);binary=p/'fake';binary.write_bytes(b'x')
            def runner(*args):return {'status':'incomplete','returncode':1,'results':{0:'(value t)'},'output':''}
            report=smoke.execute(binary,None,'mock','none',1,p,runner=runner,selected=('core-text',))
            self.assertEqual(report['summary']['rune']['passed'],0)
            self.assertEqual(report['gates'][0]['runtimes']['emacs']['status'],'not-run')

if __name__=='__main__':unittest.main()
