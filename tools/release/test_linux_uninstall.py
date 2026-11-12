"""Uninstallation checks confined to owned temporary installations."""
import importlib.util
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('linux_uninstaller', ROOT / 'tools/linux/uninstall.py')
uninstaller = importlib.util.module_from_spec(spec)
spec.loader.exec_module(uninstaller)


class UninstallTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.home = Path(temporary.name)
        self.prefix = self.home / '.local'
        self.base = self.prefix / 'opt/eclipse'
        self.release = self.base / 'releases/owned'
        self.release.mkdir(parents=True)
        (self.release / 'eclipse').write_text('owned application')
        (self.base / 'current').symlink_to(self.release)
        (self.prefix / 'bin').mkdir()
        (self.prefix / 'bin/eclipse').symlink_to(self.base / 'current/eclipse')
        self.desktop = self.prefix / 'share/applications/Eclipse.desktop'
        self.desktop.parent.mkdir(parents=True)
        self.desktop.write_text('TryExec=' + str(self.base / 'current/eclipse') + '\n')
        patch_home = patch.object(Path, 'home', return_value=self.home)
        patch_home.start(); self.addCleanup(patch_home.stop)
        patch_env = patch.dict(os.environ, {key: str(self.home / name) for key, name in
                                           [('XDG_DATA_HOME', 'data'), ('XDG_STATE_HOME', 'state'), ('XDG_CACHE_HOME', 'cache')]})
        patch_env.start(); self.addCleanup(patch_env.stop)
        self.profiles = uninstaller.profile_paths(self.home, os.environ)
        for path in self.profiles:
            path.mkdir(parents=True)
            (path / 'owned-settings').write_text('owned preferences')

    def test_preview_is_read_only_and_default_keeps_profiles_and_unrelated_entries(self):
        unrelated = self.prefix / 'bin/eclipse-cli'
        unrelated.symlink_to('/bin/true')
        other_desktop = self.desktop.with_name('uu-client.desktop')
        other_desktop.write_text('TryExec=/opt/another-application\n')
        report = uninstaller.uninstall(self.prefix, dry_run=True)
        self.assertIn(str(self.base), report['paths'])
        self.assertTrue(self.base.is_dir())
        uninstaller.uninstall(self.prefix)
        self.assertFalse(self.base.exists())
        self.assertFalse(self.desktop.exists())
        self.assertFalse((self.prefix / 'bin/eclipse').is_symlink())
        self.assertTrue(unrelated.is_symlink())
        self.assertTrue(other_desktop.is_file())
        self.assertTrue(all((path / 'owned-settings').is_file() for path in self.profiles))

    def test_purge_removes_both_profile_names_without_following_wine_links(self):
        outside = self.home / 'outside'
        outside.mkdir(); (outside / 'keep').write_text('owned outside fixture')
        wine = self.profiles[1] / 'wine/dosdevices'
        wine.mkdir(parents=True)
        (wine / 'z:').symlink_to(outside)
        uninstaller.uninstall(self.prefix, purge=True)
        self.assertTrue(all(not path.exists() for path in self.profiles))
        self.assertEqual((outside / 'keep').read_text(), 'owned outside fixture')
        self.assertEqual(uninstaller.uninstall(self.prefix, purge=True)['paths'], [])

    def test_running_installation_is_preserved(self):
        binary = self.release / 'eclipse'
        shutil.copy2('/bin/sleep', binary)
        process = subprocess.Popen([str(binary), '30'])
        try:
            with self.assertRaisesRegex(RuntimeError, 'Exit Eclipse'):
                uninstaller.uninstall(self.prefix, purge=True)
            self.assertTrue(self.base.is_dir())
            self.assertTrue(all(path.exists() for path in self.profiles))
        finally:
            process.terminate(); process.wait(timeout=5)


if __name__ == '__main__':
    unittest.main()
