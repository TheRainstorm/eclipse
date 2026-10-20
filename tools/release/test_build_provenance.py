"""Exercise the actual build script in isolated repositories and source archives."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


@unittest.skipUnless(shutil.which('rustc') and shutil.which('git'), 'Rust and Git required')
class BuildProvenanceTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.compilation = tempfile.TemporaryDirectory(prefix='uu-build-script-test-')
        cls.binary = Path(cls.compilation.name) / ('build-script.exe' if os.name == 'nt' else 'build-script')
        root = Path(__file__).resolve().parents[2]
        subprocess.run(['rustc', '--edition=2021', str(root / 'crates/uu-gui/build.rs'),
                        '-o', str(cls.binary)], check=True, capture_output=True)

    @classmethod
    def tearDownClass(cls):
        cls.compilation.cleanup()

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='uu-provenance-fixture-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.env = {k: v for k, v in os.environ.items() if not k.startswith('GIT_')}
        self.env.update(GIT_CONFIG_NOSYSTEM='1', GIT_CONFIG_GLOBAL=os.devnull)

    def git(self, *args):
        return subprocess.check_output(['git', *args], cwd=self.root, env=self.env,
                                       stderr=subprocess.DEVNULL, text=True).strip()

    def workspace(self, root):
        crate = root / 'crates/uu-gui'
        (crate / 'src').mkdir(parents=True)
        (crate / 'src/main.rs').write_text('fn main() {}\n')
        (root / 'Cargo.toml').write_text('# isolated fixture\n')
        return crate

    def run_script(self, crate):
        env = dict(self.env, CARGO_MANIFEST_DIR=str(crate), CARGO_CFG_TARGET_OS='linux')
        output = subprocess.check_output([str(self.binary)], cwd=crate, env=env, text=True)
        values = dict(line.removeprefix('cargo:rustc-env=').split('=', 1)
                      for line in output.splitlines() if line.startswith('cargo:rustc-env='))
        return values, output

    def commit_fixture(self):
        self.git('init', '-q')
        self.git('add', '.')
        self.git('-c', 'user.name=Test', '-c', 'user.email=test@example.invalid',
                 'commit', '-qm', 'isolated fixture')
        return self.git('rev-parse', 'HEAD')

    def test_actual_source_revision_and_dirty_state(self):
        crate = self.workspace(self.root)
        revision = self.commit_fixture()
        values, output = self.run_script(crate)
        self.assertEqual(values, {'UU_BUILD_COMMIT': revision, 'UU_BUILD_DIRTY': 'false'})
        self.assertIn('.git/HEAD', output.replace('\\', '/'))
        self.assertIn('.git/index', output.replace('\\', '/'))
        (crate / 'src/main.rs').write_text('fn main() { /* changed */ }\n')
        self.assertEqual(self.run_script(crate)[0]['UU_BUILD_DIRTY'], 'true')
        self.git('checkout', '--', 'crates/uu-gui/src/main.rs')
        (crate / 'src/new.rs').write_text('// not committed yet\n')
        self.assertEqual(self.run_script(crate)[0]['UU_BUILD_DIRTY'], 'true')

    def test_source_archive_does_not_borrow_parent_repository_identity(self):
        (self.root / 'README').write_text('unrelated repository\n')
        self.commit_fixture()
        crate = self.workspace(self.root / 'downloaded-source')
        values, _ = self.run_script(crate)
        self.assertEqual(values, {'UU_BUILD_COMMIT': '', 'UU_BUILD_DIRTY': 'unknown'})

    def test_source_archive_without_git_is_supported(self):
        values, _ = self.run_script(self.workspace(self.root))
        self.assertEqual(values, {'UU_BUILD_COMMIT': '', 'UU_BUILD_DIRTY': 'unknown'})


if __name__ == '__main__':
    unittest.main()
