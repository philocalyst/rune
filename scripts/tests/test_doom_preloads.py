from pathlib import Path
import shutil
import sys
import tempfile
import unittest
sys.path.insert(0,str(Path(__file__).resolve().parents[1]))
import doom_mu4e_smoke as smoke

class GenuinePreloadTests(unittest.TestCase):
    def test_ordered_exact_source_loads_are_not_a_doom_success_marker(self):
        wrapper=smoke.preload_wrapper([Path('/original/frame.el'),Path('/original/startup.el')],'marker','(t (signal (car rune-benchmark-error) (cdr rune-benchmark-error)))')
        self.assertIn('(load "/original/frame.el" nil nil t)',wrapper)
        self.assertIn('(load "/original/startup.el" nil nil t)',wrapper)
        self.assertLess(wrapper.index('/original/frame.el'),wrapper.index('/original/startup.el'))
        self.assertIn('marker:preloads-complete',wrapper)
        self.assertNotIn('marker:passed',wrapper)
        self.assertNotIn('(provide ',wrapper)
        self.assertNotIn('(defun ',wrapper)
        self.assertNotIn('normal-top-level',wrapper)

    def test_fingerprints_are_relocation_independent_and_detect_mutation(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary);one=root/'one';two=root/'two';one.mkdir()
            (one/'frame.el').write_text('; genuine source fixture')
            shutil.copytree(one,two)
            before=smoke.preload_fingerprints([one/'frame.el'])
            self.assertEqual(before,smoke.preload_fingerprints([two/'frame.el']))
            (two/'frame.el').write_text('; changed source')
            self.assertNotEqual(before,smoke.preload_fingerprints([two/'frame.el']))
            self.assertNotIn(str(root),str(before))

    def test_limits_and_nonregular_sources_reject_before_runtime(self):
        with tempfile.TemporaryDirectory() as temporary:
            path=Path(temporary)
            with self.assertRaises(ValueError):smoke.preload_fingerprints([path])
            with self.assertRaises(ValueError):smoke.preload_fingerprints([path]*9)
            source=path/'large.el'
            with source.open('wb') as stream:stream.truncate(8*1024*1024+1)
            with self.assertRaises(ValueError):smoke.preload_fingerprints([source])

    def _simulate_gate(self, runtime, mutate=False, completion=True):
        import json
        from types import SimpleNamespace
        from unittest.mock import patch
        with tempfile.TemporaryDirectory() as temporary:
            base=Path(temporary);profile=base/'profile';profile.mkdir()
            for name in ('init.el','config.el','packages.el'):(profile/name).write_text('; synthetic')
            mu=base/'mu';mu.mkdir();(mu/'mu4e-config.el').write_text('(defconst mu4e-mu-version "1.12.9")')
            binary=base/'binary';binary.write_text('never executed')
            source=base/'frame.el';source.write_text('; original source')
            mail=base/'mail';mail.mkdir();(mail/'message').write_text('synthetic')
            mail_manifest=base/'mail.json';mail_manifest.write_text(json.dumps({'synthetic_message_sha256':{'message':smoke.sha(mail/'message')}}))
            packages={'schema_version':1,'packages':{'@doom':{'revision':'1'*40}}}
            manifest=base/'packages.json';manifest.write_text(json.dumps(packages));output=base/'output.json'
            names=('doom-root','doom-config','doom-local','gnu','rune','mu-driver','mu-home','mu-binary','mu-lisp','workspace','maildir','mail-manifest','package-manifest','output')
            values={'doom-root':base/'doom','doom-local':base/'local','doom-config':profile,'mu-lisp':mu,'maildir':mail,'mail-manifest':mail_manifest,'package-manifest':manifest,'output':output}
            argv=['smoke','--revision','synthetic-plumbing','--runtime',runtime,'--gate','doom-profile-boot','--rune-preload',str(source)]
            for name in names:argv.extend(['--'+name,str(values.get(name,binary))])
            def fake_capture(command,*args):
                if '--version' in command:return ('version 1.12.9',0,None)
                if mutate:source.write_text('; changed during simulated run')
                phase = 'RUNE-DOOM-MU4E-fixture:preloads-complete\n' if completion else ''
                return (phase+'RUNE-DOOM-MU4E-fixture:passed',0,None)
            with patch.object(sys,'argv',argv),patch.object(smoke,'capture',side_effect=fake_capture),patch.object(smoke,'capture_package_sources',return_value=packages),patch.object(smoke.uuid,'uuid4',return_value=SimpleNamespace(hex='fixture')),patch.object(smoke.subprocess,'check_output',return_value='1'*40):
                code=smoke.main()
            report=json.loads(output.read_text())
            return code,report

    def test_preload_mutation_invalidates_rune_not_an_unrelated_gnu_run(self):
        for runtime in ('rune','gnu'):
            with self.subTest(runtime=runtime):
                code,report=self._simulate_gate(runtime,mutate=True)
                self.assertEqual(code,1 if runtime=='rune' else 0)
                self.assertFalse(report['rune_preloads_unchanged'])
                self.assertEqual(report['gates'][0]['runtimes'][runtime]['passed'],runtime=='gnu')
                if runtime=='rune':
                    self.assertTrue(report['gates'][0]['runtimes']['rune']['rune_preloads_completed'])
                    self.assertIn('preload changed',report['gates'][0]['runtimes']['rune']['first_error'])

    def test_required_completion_marker_missing_rejects_exit0_workflow_marker(self):
        code,report=self._simulate_gate('rune',completion=False)
        self.assertEqual(code,1)
        result=report['gates'][0]['runtimes']['rune']
        self.assertTrue(report['rune_preloads_unchanged'])
        self.assertFalse(result['rune_preloads_completed'])
        self.assertFalse(result['passed'])
        self.assertEqual(result['returncode'],0)
        self.assertIn(':passed',result['output'])
        self.assertIn('completion marker missing',result['first_error'])
        code,report=self._simulate_gate('rune',completion=True)
        self.assertEqual(code,0)
        self.assertTrue(report['gates'][0]['runtimes']['rune']['passed'])
        code,report=self._simulate_gate('gnu',completion=False)
        self.assertEqual(code,0)
        self.assertTrue(report['gates'][0]['runtimes']['gnu']['passed'])

if __name__=='__main__':unittest.main()
