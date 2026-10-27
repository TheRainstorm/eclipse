"""Regression checks for version accuracy and validation before remote mutations."""
import contextlib
import hashlib
import io
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
import warnings
import zipfile

import package
import sync


class ReleaseTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)

    def make_bundle(self, transform=None):
        files = {name: ('test ' + name).encode() for name in package.REQUIRED}
        manifest = {'format_version': 3, 'target':package.TARGET, 'source_commit': 'a' * 40,
                    'sha256': {name: hashlib.sha256(data).hexdigest() for name, data in files.items()}}
        files['version.json'] = json.dumps(manifest).encode()
        if transform:
            transform(files)
        path = self.root / 'package 空格.zip'
        with zipfile.ZipFile(path, 'w') as bundle:
            for name, data in files.items():
                bundle.writestr(name, data)
        return path

    def test_valid_package(self):
        self.assertEqual(sync.verify_package(self.make_bundle())['source_commit'], 'a' * 40)

    def test_same_revision_package_is_verified_and_reused_without_a_compiler(self):
        archive=self.make_bundle().rename(self.root/('uu-client-windows-x64-'+'a'*12+'.zip'))
        with patch.object(package,'clean_revision',return_value='a'*40), patch.object(package,'check_tools',side_effect=AssertionError('compiler must not run')), contextlib.redirect_stdout(io.StringIO()):
            result=package.build_package(self.root)
        self.assertEqual(result['sha256'],package.sha256(archive))
        self.assertEqual(json.loads((self.root/'windows-latest.json').read_text())['source_commit'],'a'*40)

    def test_corrupt_stale_and_wrong_target_caches_require_building(self):
        for kind in ['corrupt','stale','target','force']:
            def change(files):
                if kind=='corrupt':files['uu.exe']=b'changed'
                metadata=json.loads(files['version.json'])
                if kind=='stale':metadata['source_commit']='b'*40
                if kind=='target':metadata['target']='other-target'
                files['version.json']=json.dumps(metadata).encode()
            self.make_bundle(change).replace(self.root/('uu-client-windows-x64-'+'a'*12+'.zip'))
            with self.subTest(kind=kind), patch.object(package,'clean_revision',return_value='a'*40), patch.object(package,'check_tools',side_effect=RuntimeError('build required')), contextlib.redirect_stderr(io.StringIO()), self.assertRaisesRegex(RuntimeError,'build required'):
                package.build_package(self.root,rebuild=kind=='force')

    def test_corrupt_payload_rejected(self):
        path = self.make_bundle(lambda files: files.update({'uu-gui.exe': b'corrupt'}))
        with self.assertRaisesRegex(ValueError, 'uu-gui.exe'):
            sync.verify_package(path)

    def test_license_notices_are_required_and_verified(self):
        for name in package.LICENSES:
            with self.subTest(name=name):
                path = self.make_bundle(lambda files: files.pop(name))
                with self.assertRaises(ValueError):
                    sync.verify_package(path)
                path = self.make_bundle(lambda files: files.update({name: b'changed notice'}))
                with self.assertRaisesRegex(ValueError, name):
                    sync.verify_package(path)

    def test_dispatcher_is_required_and_cannot_be_corrupted_in_new_bundles(self):
        for corrupt in (False, True):
            def transform(files):
                if corrupt: files['libvpl.dll'] = b'changed dispatcher'
                else: files.pop('libvpl.dll')
            with self.assertRaises(ValueError):
                sync.verify_package(self.make_bundle(transform))

    def test_legacy_bundles_remain_installable_and_unknown_formats_are_rejected(self):
        def legacy(files):
            files.pop('libvpl.dll')
            files.pop('libpyrowave-shared-1.dll')
            manifest = json.loads(files['version.json'])
            manifest.pop('format_version')
            manifest['sha256'].pop('libvpl.dll')
            manifest['sha256'].pop('libpyrowave-shared-1.dll')
            files['version.json'] = json.dumps(manifest).encode()
        self.assertNotIn('format_version', sync.verify_package(self.make_bundle(legacy)))
        def unknown(files):
            manifest = json.loads(files['version.json']);manifest['format_version'] = 4
            files['version.json'] = json.dumps(manifest).encode()
        with self.assertRaises(ValueError): sync.verify_package(self.make_bundle(unknown))

    def test_vpl_only_format_two_remains_installable_and_pyrowave_runtime_is_verified(self):
        def previous(files):
            files.pop('libpyrowave-shared-1.dll');manifest=json.loads(files['version.json'])
            manifest['format_version']=2;manifest['sha256'].pop('libpyrowave-shared-1.dll')
            files['version.json']=json.dumps(manifest).encode()
        self.assertEqual(sync.verify_package(self.make_bundle(previous))['format_version'],2)
        for missing in [False,True]:
            def changed(files):
                if missing:files.pop('libpyrowave-shared-1.dll')
                else:files['libpyrowave-shared-1.dll']=b'wrong runtime'
            with self.assertRaises(ValueError):sync.verify_package(self.make_bundle(changed))

    def test_missing_file_rejected(self):
        path = self.make_bundle(lambda files: files.pop('libstdc++-6.dll'))
        with self.assertRaises(ValueError):
            sync.verify_package(path)

    def test_path_traversal_rejected(self):
        path = self.make_bundle(lambda files: files.update({'../install.ps1': b'bad'}))
        with self.assertRaises(ValueError):
            sync.verify_package(path)

    def test_duplicate_zip_entry_rejected(self):
        path = self.make_bundle()
        with warnings.catch_warnings():
            warnings.simplefilter('ignore', UserWarning)
            with zipfile.ZipFile(path, 'a') as bundle:
                bundle.writestr('install.ps1', b'different installer')
        with self.assertRaises(ValueError):
            sync.verify_package(path)

    def test_latest_pointer_checks_zip_hash(self):
        path = self.make_bundle()
        record = {'archive': path.name, 'sha256': '0' * 64}
        (self.root / 'windows-latest.json').write_text(json.dumps(record), encoding='utf-8')
        with patch.object(sync, 'OUTPUT', self.root):
            with self.assertRaisesRegex(ValueError, 'ZIP'):
                sync.latest_package()
            record['sha256'] = package.sha256(path)
            (self.root / 'windows-latest.json').write_text(json.dumps(record), encoding='utf-8')
            self.assertEqual(sync.latest_package(), path)

    def run_sync(self, path, dry_run=True):
        args = type('Args', (), dict(package=path, host='192.0.2.10', user='tester', port=22,
                                    identity=None, install_dir="D:\\测试 目录\\tester's demo",
                                    no_shortcut=True, dry_run=dry_run))()
        with contextlib.redirect_stdout(io.StringIO()):
            sync.sync_package(args)

    def test_dry_run_never_connects(self):
        with patch.object(sync.subprocess, 'run') as run:
            self.run_sync(self.make_bundle())
            run.assert_not_called()

    def test_cli_requires_user_and_install_directory(self):
        result = subprocess.run([sys.executable, str(Path(sync.__file__)), '--host', '192.0.2.10',
                                 '--dry-run'], capture_output=True, text=True)
        self.assertEqual(result.returncode, 2)
        self.assertIn('--user', result.stderr)
        self.assertIn('--install-dir', result.stderr)

    def test_cli_defaults_to_standard_ssh_port(self):
        result = subprocess.run([sys.executable, str(Path(sync.__file__)), '--host', '192.0.2.10',
                                 '--user', 'tester', '--install-dir', r'C:\Apps\uu-client',
                                 '--package', str(self.make_bundle()), '--dry-run'],
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('tester@192.0.2.10:22', result.stdout)

    def test_corrupt_package_never_uploads(self):
        path = self.make_bundle(lambda files: files.update({'install.ps1': b'bad installer'}))
        with patch.object(sync.subprocess, 'run') as run:
            with self.assertRaises(ValueError):
                self.run_sync(path, dry_run=False)
            run.assert_not_called()

    def test_powershell_literal_and_unicode_transport(self):
        import base64
        with patch.object(sync.subprocess, 'run') as run:
            self.run_sync(self.make_bundle(), dry_run=False)
        upload, remote = [call.args[0] for call in run.call_args_list]
        self.assertEqual(upload[:4], ['scp', '-O', '-P', '22'])
        script = base64.b64decode(remote[-1]).decode('utf-16-le')
        self.assertIn("-InstallDir 'D:\\测试 目录\\tester''s demo' -NoShortcut", script)
        self.assertIn('Get-FileHash -LiteralPath $uploaded', script)
        self.assertEqual(sync.ps_literal("x'; $(whoami)"), "'x''; $(whoami)'")

    def test_untracked_and_modified_source_block_release(self):
        def git(*args):
            return subprocess.check_output(['git', *args], cwd=self.root, stderr=subprocess.DEVNULL)
        git('init', '-q')
        (self.root / 'Cargo.toml').write_text('# test\n')
        git('add', 'Cargo.toml')
        git('-c', 'user.name=Test', '-c', 'user.email=test@example.invalid', 'commit', '-qm', 'test')
        with patch.object(package, 'ROOT', self.root):
            self.assertEqual(len(package.clean_revision()), 40)
            (self.root / 'crates').mkdir()
            new_source = self.root / 'crates/new.rs'
            new_source.write_text('// untracked\n')
            with self.assertRaisesRegex(RuntimeError, 'crates/new.rs'):
                package.clean_revision()
            new_source.unlink()
            (self.root / 'assets').mkdir()
            icon = self.root / 'assets/icon.png'
            icon.write_bytes(b'untracked icon')
            with self.assertRaisesRegex(RuntimeError, 'assets/icon.png'):
                package.clean_revision()
            icon.unlink()
            (self.root / 'Cargo.toml').write_text('# changed\n')
            with self.assertRaisesRegex(RuntimeError, 'Cargo.toml'):
                package.clean_revision()


if __name__ == '__main__':
    unittest.main()
