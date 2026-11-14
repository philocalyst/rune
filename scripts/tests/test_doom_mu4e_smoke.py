from pathlib import Path
import sys
import unittest
sys.path.insert(0,str(Path(__file__).resolve().parents[1]))
import doom_mu4e_smoke as smoke
class DoomSourcesTests(unittest.TestCase):
 def test_actual_full_requirements_no_mock_features(self):
  prelude,gates=smoke.sources('marker',Path('/synthetic.org'),Path('/synthetic-mu-driver.el'),Path('/synthetic-db'),Path('/official-mu'),Path('/official-mu4e'))
  self.assertIn('mu4e-get-mail-command nil',prelude)
  self.assertIn('mu4e-view-auto-mark-as-read nil',prelude)
  self.assertEqual(len(gates),4)
  forms='\n'.join(form for _,form in gates)
  self.assertIn('(modulep! :email mu4e)',forms)
  self.assertIn('(make-sparse-keymap)',forms)
  self.assertIn('(lookup-key',forms)
  self.assertIn('(org-todo "DONE")',forms)
  self.assertIn('(revert-buffer t t)',forms)
  self.assertIn('(load "/synthetic-mu-driver.el"',forms)
  self.assertNotIn('(provide ',forms)
  self.assertNotIn('defun mu4e',forms)
 def test_fixture_contains_only_synthetic_content(self):
  self.assertIn('Synthetic Doom benchmark',smoke.ORG)
  self.assertIn('TODO',smoke.ORG)
if __name__=='__main__':unittest.main()
