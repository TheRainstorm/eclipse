"""Exercise the actual build script in isolated repositories and source archives."""
import os
import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import tomllib
import unittest


@unittest.skipUnless(shutil.which('cargo') and shutil.which('git'), 'Cargo and Git required')
class BuildProvenanceTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.compilation = tempfile.TemporaryDirectory(prefix='uu-build-script-test-')
        cls.addClassCleanup(cls.compilation.cleanup)
        harness = Path(cls.compilation.name)
        cls.binary = harness / 'target/debug' / ('build-script.exe' if os.name == 'nt' else 'build-script')
        root = Path(__file__).resolve().parents[2]
        package = tomllib.loads((root / 'crates/uu-gui/Cargo.toml').read_text())
        def literal(value):
            if isinstance(value, dict):
                return '{' + ', '.join(k + '=' + literal(v) for k, v in value.items()) + '}'
            return json.dumps(value)
        dependencies = '\n'.join(name + '=' + literal(value)
                                 for name, value in package.get('build-dependencies', {}).items())
        # Run the actual production script with its real build dependencies,
        # without compiling the GUI, media SDK or native desktop libraries.
        (harness / 'Cargo.toml').write_text(
            '[package]\nname="eclipse-provenance-test"\nversion="0.0.0"\nedition="2021"\nbuild=false\n'
            '[[bin]]\nname="build-script"\npath=' + json.dumps(str(root / 'crates/uu-gui/build.rs'))
            + '\n[dependencies]\n' + dependencies + '\n')
        shutil.copyfile(root / 'Cargo.lock', harness / 'Cargo.lock')
        subprocess.run(['cargo', 'build', '--quiet', '--manifest-path', str(harness / 'Cargo.toml'),
                        '--target-dir', str(harness / 'target')], cwd=harness, check=True)

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
        env = dict(self.env, CARGO_MANIFEST_DIR=str(crate), CARGO_CFG_TARGET_OS='linux',
                   SOURCE_DATE_EPOCH='1234567890')
        output = subprocess.check_output([str(self.binary)], cwd=crate, env=env, text=True)
        values = dict(line.removeprefix('cargo:rustc-env=').split('=', 1)
                      for line in output.splitlines() if line.startswith('cargo:rustc-env=UU_BUILD_'))
        return values, output

    def test_source_date_epoch_formats_the_build_time_to_utc_seconds(self):
        _, output = self.run_script(self.workspace(self.root))
        if 'cargo:rustc-env=ECLIPSE_BUILD_TIME=' not in output:
            self.skipTest('historical source does not publish a build timestamp')
        self.assertIn('cargo:rustc-env=ECLIPSE_BUILD_TIME=2009-02-13 23:31:30 UTC\n', output)

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
        self.assertEqual(values, {'UU_BUILD_COMMIT': revision, 'UU_BUILD_TAG': '',
                                  'UU_BUILD_DIRTY': 'false'})
        self.assertIn('.git/HEAD', output.replace('\\', '/'))
        self.assertIn('.git/index', output.replace('\\', '/'))
        (crate / 'src/main.rs').write_text('fn main() { /* changed */ }\n')
        self.assertEqual(self.run_script(crate)[0]['UU_BUILD_DIRTY'], 'true')
        self.git('checkout', '--', 'crates/uu-gui/src/main.rs')
        (crate / 'src/new.rs').write_text('// not committed yet\n')
        self.assertEqual(self.run_script(crate)[0]['UU_BUILD_DIRTY'], 'true')

    def test_exact_release_tag_survives_packing_but_does_not_label_descendants(self):
        crate = self.workspace(self.root)
        revision = self.commit_fixture()
        self.git('-c', 'user.name=Test', '-c', 'user.email=test@example.invalid',
                 'tag', '-a', 'v0.24.0', '-m', 'isolated release')
        values, output = self.run_script(crate)
        self.assertEqual(values, {'UU_BUILD_COMMIT': revision, 'UU_BUILD_TAG': 'v0.24.0',
                                  'UU_BUILD_DIRTY': 'false'})
        self.assertIn('.git/refs/tags', output.replace('\\', '/'))
        self.git('pack-refs', '--all')
        values, output = self.run_script(crate)
        self.assertEqual(values['UU_BUILD_TAG'], 'v0.24.0')
        self.assertIn('.git/packed-refs', output.replace('\\', '/'))
        self.git('-c', 'user.name=Test', '-c', 'user.email=test@example.invalid',
                 'commit', '--allow-empty', '-qm', 'development after release')
        values, _ = self.run_script(crate)
        self.assertEqual(values, {'UU_BUILD_COMMIT': self.git('rev-parse', 'HEAD'),
                                  'UU_BUILD_TAG': '', 'UU_BUILD_DIRTY': 'false'})

    def test_source_archive_does_not_borrow_parent_repository_identity(self):
        (self.root / 'README').write_text('unrelated repository\n')
        self.commit_fixture()
        self.git('tag', 'v9.9.9')
        crate = self.workspace(self.root / 'downloaded-source')
        values, _ = self.run_script(crate)
        self.assertEqual(values, {'UU_BUILD_COMMIT': '', 'UU_BUILD_TAG': '',
                                  'UU_BUILD_DIRTY': 'unknown'})

    def test_source_archive_without_git_is_supported(self):
        values, _ = self.run_script(self.workspace(self.root))
        self.assertEqual(values, {'UU_BUILD_COMMIT': '', 'UU_BUILD_TAG': '',
                                  'UU_BUILD_DIRTY': 'unknown'})


if __name__ == '__main__':
    unittest.main()
