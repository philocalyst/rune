from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import package_provenance as provenance

class PackageProvenanceTests(unittest.TestCase):
    def make_repo(self, root):
        repo = root/'example'
        repo.mkdir(parents=True)
        subprocess.run(['git','init','-q',str(repo)],check=True)
        (repo/'file.el').write_text('(message "synthetic")\n')
        for args in [['add','.'],['-c','user.name=Synthetic','-c','user.email=synthetic@example.invalid','commit','-qm','fixture']]:
            subprocess.run(['git','-C',str(repo),*args],check=True)
        return repo

    def test_relocation_is_identical_and_dirty_bytes_change_without_head_change(self):
        with tempfile.TemporaryDirectory() as temporary:
            base=Path(temporary)
            repo=self.make_repo(base/'one')
            first=provenance.capture(base/'one')
            shutil.copytree(base/'one',base/'two')
            self.assertEqual(first,provenance.capture(base/'two'))
            (repo/'file.el').write_text('(message "different")\n')
            second=provenance.capture(base/'one')
            self.assertEqual(first['packages']['example']['revision'],second['packages']['example']['revision'])
            self.assertNotEqual(first,second)
            self.assertNotIn(str(base),str(first))

    def test_symlink_target_is_hashed_not_followed(self):
        with tempfile.TemporaryDirectory() as temporary:
            base=Path(temporary);repo=self.make_repo(base/'repos')
            outside=base/'outside';outside.write_text('not package source')
            (repo/'link').symlink_to(outside)
            subprocess.run(['git','-C',str(repo),'add','link'],check=True)
            first=provenance.capture(base/'repos')
            outside.write_text('changed outside content')
            self.assertEqual(first,provenance.capture(base/'repos'))
            (repo/'link').unlink();(repo/'link').symlink_to('new-dangling-target')
            self.assertNotEqual(first,provenance.capture(base/'repos'))

    def test_harness_rejects_mismatched_manifest_before_launching_runtime(self):
        import contextlib
        import io
        import json
        import doom_mu4e_smoke as smoke
        with tempfile.TemporaryDirectory() as temporary:
            base=Path(temporary);repos=base/'local/straight/repos';repo=self.make_repo(repos)
            mu=base/'mu-lisp';mu.mkdir();(mu/'mu4e.el').write_text('; synthetic source')
            expected=provenance.capture(repos, {'@doom':repo}, {'@mu4e':mu})
            expected['packages']['example']['revision']='0'*40
            manifest=base/'manifest.json';manifest.write_text(json.dumps(expected))
            names=('doom-root','doom-config','doom-local','gnu','rune','mu-driver','mu-home','mu-binary','mu-lisp','workspace','maildir','mail-manifest','package-manifest','output')
            values={'doom-root':repo,'doom-local':base/'local','package-manifest':manifest,'mu-lisp':mu}
            argv=['smoke','--revision','fixture']
            for name in names:argv.extend(['--'+name,str(values.get(name,base/'unused'))])
            with patch.object(sys,'argv',argv), patch.object(smoke,'capture',side_effect=AssertionError('runtime launched')), contextlib.redirect_stderr(io.StringIO()):
                with self.assertRaises(SystemExit) as failure:smoke.main()
            self.assertEqual(failure.exception.code,2)

    def test_post_run_missing_source_preserves_report_and_invalidates_pass(self):
        import json
        from types import SimpleNamespace
        import doom_mu4e_smoke as smoke
        with tempfile.TemporaryDirectory() as temporary:
            base=Path(temporary);repos=base/'local/straight/repos';repo=self.make_repo(repos)
            mu=base/'mu';mu.mkdir();(mu/'mu4e-config.el').write_text('(defconst mu4e-mu-version "1.12.9")')
            profile=base/'profile';profile.mkdir()
            for name in ('init.el','config.el','packages.el'):(profile/name).write_text('; synthetic')
            binary=base/'binary';binary.write_text('not executed')
            mail=base/'mail';mail.mkdir();(mail/'message').write_text('synthetic only')
            mail_manifest=base/'mail.json';mail_manifest.write_text(json.dumps({'synthetic_message_sha256':{'message':smoke.sha(mail/'message')}}))
            expected=provenance.capture(repos, {'@doom':repo}, {'@mu4e':mu})
            manifest=base/'packages.json';manifest.write_text(json.dumps(expected));output=base/'output.json'
            names=('doom-root','doom-config','doom-local','gnu','rune','mu-driver','mu-home','mu-binary','mu-lisp','workspace','maildir','mail-manifest','package-manifest','output')
            values={'doom-root':repo,'doom-local':base/'local','doom-config':profile,'mu-lisp':mu,'maildir':mail,'mail-manifest':mail_manifest,'package-manifest':manifest,'output':output}
            argv=['smoke','--revision','synthetic-plumbing-only','--runtime','gnu','--gate','doom-profile-boot']
            for name in names:argv.extend(['--'+name,str(values.get(name,binary))])
            def fake_capture(command,*args):
                return ('version 1.12.9' if '--version' in command else 'RUNE-DOOM-MU4E-fixture:passed',0,None)
            with patch.object(sys,'argv',argv), patch.object(smoke,'capture',side_effect=fake_capture), patch.object(smoke,'capture_package_sources',side_effect=[expected,FileNotFoundError('removed source')]), patch.object(smoke.uuid,'uuid4',return_value=SimpleNamespace(hex='fixture')), patch.object(smoke.subprocess,'check_output',return_value='1'*40):
                self.assertEqual(smoke.main(),1)
            report=json.loads(output.read_text())
            self.assertFalse(report['package_sources_unchanged'])
            self.assertFalse(report['gates'][0]['runtimes']['gnu']['passed'])
            self.assertIn('capture_error',report['package_sources_after'])
            self.assertIn(':passed',report['gates'][0]['runtimes']['gnu']['output'])

    def test_plain_mu4e_source_tree_is_path_independent_and_content_checked(self):
        with tempfile.TemporaryDirectory() as temporary:
            base=Path(temporary);self.make_repo(base/'repos')
            mu=base/'mu-one';mu.mkdir();(mu/'mu4e-server.el').write_text('; synthetic mu4e source')
            shutil.copytree(mu,base/'mu-two')
            first=provenance.capture(base/'repos',source_trees={'@mu4e':mu})
            self.assertEqual(first,provenance.capture(base/'repos',source_trees={'@mu4e':base/'mu-two'}))
            self.assertIsNone(first['packages']['@mu4e']['revision'])
            self.assertEqual(first['packages']['@mu4e']['source_kind'],'directory-files')
            (mu/'mu4e-server.el').write_text('; modified source')
            self.assertNotEqual(first,provenance.capture(base/'repos',source_trees={'@mu4e':mu}))

    def test_caps_and_nonregular_parent_reject(self):
        with tempfile.TemporaryDirectory() as temporary:
            base=Path(temporary);self.make_repo(base/'repos')
            with patch.object(provenance,'MAX_TOTAL_BYTES',1):
                with self.assertRaises(ValueError):provenance.capture(base/'repos')
            (base/'link').symlink_to(base/'repos',target_is_directory=True)
            with self.assertRaises(ValueError):provenance.capture(base/'link')

if __name__=='__main__':unittest.main()
