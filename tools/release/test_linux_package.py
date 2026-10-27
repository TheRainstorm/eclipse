"""Linux payload integrity, build provenance and isolated installation checks."""
import contextlib
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import sys
import platform
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
    return module
installer = load('linux_installer', ROOT / 'tools/linux/install.py')
packager = load('linux_packager', ROOT / 'tools/linux/package.py')


class LinuxPackageTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)

    def cached_bundle(self, changed=False, extra=None, revision='a'*40):
        package=self.package()
        manifest=json.loads((package/'version.json').read_text())
        manifest.update(source_commit=revision,platform=f'linux-{platform.machine()}',features=packager.FEATURES,build_glibc=list(platform.libc_ver()))
        (package/'version.json').write_text(json.dumps(manifest))
        if changed:(package/'uu').write_text('corrupt')
        destination=self.root/f'uu-client-linux-{platform.machine()}-aaaaaaaaaaaa'
        package.rename(destination)
        archive=Path(shutil.make_archive(str(self.root/'cache'),'gztar',root_dir=self.root,base_dir=destination.name))
        if extra:
            with tarfile.open(archive,'w:gz') as bundle:
                bundle.add(destination,arcname=destination.name)
                member=tarfile.TarInfo(extra);member.size=1;bundle.addfile(member,io.BytesIO(b'x'))
        shutil.rmtree(destination)
        return archive,destination

    def test_verified_archive_restores_an_absent_directory_without_a_compiler(self):
        archive,destination=self.cached_bundle()
        with patch.object(packager,'source_revision',return_value='a'*40),patch.object(packager.subprocess,'run',side_effect=AssertionError('no compiler')):
            result=packager.reuse_archive(archive,destination,'a'*40)
        self.assertTrue(result['cached'])
        self.assertEqual(installer.verify_package(destination)['source_commit'],'a'*40)
        self.assertTrue((destination/'uu').stat().st_mode&0o111)

    def test_corrupt_and_unsafe_caches_never_materialize_payload(self):
        for changed,extra in [(True,None),(False,'../escape')]:
            archive,destination=self.cached_bundle(changed,extra)
            with patch.object(packager,'source_revision',return_value='a'*40),self.assertRaises(ValueError):
                packager.reuse_archive(archive,destination,'a'*40)
            self.assertFalse(destination.exists())
        self.assertFalse((self.root.parent/'escape').exists())

    def test_wrong_revision_cache_does_not_replace_the_requested_build(self):
        archive,destination=self.cached_bundle(revision='b'*40)
        self.assertIsNone(packager.reuse_archive(archive,destination,'a'*40))
        self.assertFalse(destination.exists())

    def test_help_never_starts_a_build(self):
        result=subprocess.run([sys.executable,str(ROOT/'tools/linux/package.py'),'--help'],capture_output=True,text=True,check=True)
        self.assertIn('--rebuild',result.stdout)

    def package(self, revision='a' * 40):
        package = self.root / revision[:12];package.mkdir()
        for name in installer.PAYLOAD:
            target = package / name
            if name in ('uu', 'uu-gui','libpyrowave-shared.so.1'): shutil.copy2('/bin/true', target)
            else: target.write_text('owned fixture/' + name)
        manifest = {'format_version':2,'source_commit': revision, 'sha256': {name: hashlib.sha256((package / name).read_bytes()).hexdigest() for name in installer.PAYLOAD}}
        (package / 'version.json').write_text(json.dumps(manifest))
        return package

    def test_every_payload_is_checked_including_installer_and_readme(self):
        package = self.package();installer.verify_package(package)
        for name in installer.PAYLOAD:
            with self.subTest(name=name):
                path = package / name;original = path.read_bytes();path.write_bytes(b'corrupt')
                with self.assertRaisesRegex(ValueError, name): installer.verify_package(package)
                path.write_bytes(original)
        (package / 'README.txt').unlink();(package / 'README.txt').symlink_to(package / 'LICENSE')
        with self.assertRaises(ValueError): installer.verify_package(package)

    def test_upgrade_preserves_old_release_and_user_config(self):
        first = self.package();second = self.package('b' * 40)
        prefix = self.root / 'owned prefix % with spaces'
        config = prefix / 'share/uu-remote/user-settings.json'
        config.parent.mkdir(parents=True);config.write_text('user configuration')
        for package in (first, second):
            with patch('sys.argv', ['install.py', '--package', str(package), '--prefix', str(prefix)]), contextlib.redirect_stdout(io.StringIO()):
                installer.main()
        base = prefix / 'opt/uu-remote-rust'
        self.assertEqual((base / 'current').resolve(), base / 'releases' / ('b' * 12))
        self.assertTrue((base / 'releases' / ('a' * 12) / 'uu-gui').exists())
        self.assertEqual(config.read_text(), 'user configuration')
        self.assertEqual(subprocess.run([str(prefix / 'bin/uu-client')]).returncode, 0)
        before = (base / 'current').resolve()
        (second / 'README.txt').write_text('corrupt')
        with patch('sys.argv', ['install.py', '--package', str(second), '--prefix', str(prefix)]), self.assertRaises(ValueError): installer.main()
        self.assertEqual((base / 'current').resolve(), before)

    def test_reusing_damaged_release_never_changes_current_or_launchers(self):
        first = self.package(); second = self.package('b' * 40)
        prefix = self.root / 'installed'
        def install(package):
            with patch('sys.argv', ['install.py', '--package', str(package), '--prefix', str(prefix)]), contextlib.redirect_stdout(io.StringIO()):
                installer.main()
        install(first); install(second)
        base = prefix / 'opt/uu-remote-rust'
        metadata = base / 'releases' / ('a' * 12) / 'version.json'
        original = metadata.read_text()
        desktop = prefix / 'share/applications/uu-client.desktop'
        before = ((base / 'current').resolve(), desktop.read_bytes())
        malformed = [None, 'not json', '[]', json.dumps(dict(json.loads(original), source_commit='c' * 40))]
        for content in malformed:
            with self.subTest(metadata=content):
                if content is None: metadata.unlink()
                else: metadata.write_text(content)
                with self.assertRaises(ValueError): install(first)
                self.assertEqual(((base / 'current').resolve(), desktop.read_bytes()), before)
                metadata.write_text(original)
        metadata.unlink(); metadata.symlink_to(first / 'version.json')
        with self.assertRaises(ValueError): install(first)
        self.assertEqual(((base / 'current').resolve(), desktop.read_bytes()), before)

    def test_same_source_rebuild_metadata_can_reuse_verified_payload(self):
        package = self.package(); prefix = self.root / 'installed'
        def install():
            with patch('sys.argv', ['install.py', '--package', str(package), '--prefix', str(prefix)]), contextlib.redirect_stdout(io.StringIO()):
                installer.main()
        install()
        metadata = package / 'version.json'
        value = json.loads(metadata.read_text()); value['built_at_utc'] = '2000-01-01T00:00:00Z'
        metadata.write_text(json.dumps(value)); install()
        current = prefix / 'opt/uu-remote-rust/current'
        self.assertEqual(installer.verify_package(current)['source_commit'], 'a' * 40)
        self.assertEqual(subprocess.run([str(prefix / 'bin/uu-client')]).returncode, 0)

    def test_source_guard_rejects_untracked_and_dirty_inputs(self):
        def git(*args):return subprocess.check_output(['git', *args], cwd=self.root, stderr=subprocess.DEVNULL)
        git('init', '-q');(self.root / 'Cargo.toml').write_text('# owned fixture')
        git('add', 'Cargo.toml');git('-c', 'user.name=Test', '-c', 'user.email=test@example.invalid', 'commit', '-qm', 'fixture')
        with patch.object(packager, 'ROOT', self.root):
            self.assertEqual(len(packager.source_revision()), 40)
            source = self.root / 'tools/linux/README.txt';source.parent.mkdir(parents=True);source.write_text('new package input')
            with self.assertRaises(RuntimeError):packager.source_revision()
            source.unlink();(self.root / 'Cargo.toml').write_text('# modified')
            with self.assertRaises(RuntimeError):packager.source_revision()

    def test_revision_change_during_build_never_publishes_a_package(self):
        metadata = json.dumps({'target_directory': str(self.root / 'custom-target')})
        with patch.object(packager, 'ROOT', self.root), patch.object(packager, 'source_revision', side_effect=['a' * 40, 'b' * 40]), patch.object(packager.subprocess, 'check_output', return_value=metadata), patch.object(packager.subprocess, 'run'), patch.object(packager.pyrowave_runtime,'build',return_value=self.root/'fixture-runtime.so'):
            with self.assertRaisesRegex(RuntimeError, 'changed during'):packager.main([])
        self.assertFalse((self.root / 'artifacts').exists())

    def test_custom_cargo_target_and_complete_archive(self):
        metadata = json.dumps({'target_directory': str(self.root / 'custom-target')})
        binaries = self.root / 'custom-target/release';binaries.mkdir(parents=True)
        for name in ('uu', 'uu-gui'):(binaries / name).write_bytes(('owned ' + name).encode())
        (self.root/'fixture-runtime.so').write_bytes(b'owned Vulkan runtime fixture')
        for path in ['tools/linux/install.py', 'tools/linux/README.txt', 'assets/branding/eclipse-512.png', *packager.LICENSES]:
            target = self.root / path;target.parent.mkdir(parents=True, exist_ok=True);target.write_text(path)
        with patch.object(packager, 'ROOT', self.root), patch.object(packager, 'source_revision', return_value='a' * 40), patch.object(packager.subprocess, 'check_output', return_value=metadata), patch.object(packager.subprocess, 'run'), patch.object(packager.pyrowave_runtime,'build',return_value=self.root/'fixture-runtime.so'), contextlib.redirect_stdout(io.StringIO()):packager.main([])
        archives = list((self.root / 'artifacts/packages').glob('*.tar.gz'));self.assertEqual(len(archives), 1)
        with tarfile.open(archives[0]) as archive:
            manifest = next(entry for entry in archive if entry.name.endswith('/version.json'))
            record = json.load(archive.extractfile(manifest))
            self.assertEqual(set(record['sha256']), set(installer.PAYLOAD))
            self.assertEqual(record['source_commit'], 'a' * 40)
            self.assertIn('intel-vpl', record['features'])


if __name__ == '__main__':unittest.main()
