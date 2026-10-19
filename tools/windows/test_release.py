"""Regression checks for version accuracy and validation before remote mutations."""
import contextlib
import hashlib
import io
import json
from pathlib import Path
import subprocess
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
        manifest = {'source_commit': 'a' * 40,
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

    def test_corrupt_payload_rejected(self):
        path = self.make_bundle(lambda files: files.update({'uu-gui.exe': b'corrupt'}))
        with self.assertRaisesRegex(ValueError, 'uu-gui.exe'):
            sync.verify_package(path)

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
        args = type('Args', (), dict(package=path, host='192.168.34.3', user='yfy', port=2202,
                                    identity=None, install_dir="D:\\测试 目录\\yfy's demo",
                                    no_shortcut=True, dry_run=dry_run))()
        with contextlib.redirect_stdout(io.StringIO()):
            sync.sync_package(args)

    def test_dry_run_never_connects(self):
        with patch.object(sync.subprocess, 'run') as run:
            self.run_sync(self.make_bundle())
            run.assert_not_called()

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
        self.assertEqual(upload[:4], ['scp', '-O', '-P', '2202'])
        script = base64.b64decode(remote[-1]).decode('utf-16-le')
        self.assertIn("-InstallDir 'D:\\测试 目录\\yfy''s demo' -NoShortcut", script)
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
            (self.root / 'Cargo.toml').write_text('# changed\n')
            with self.assertRaisesRegex(RuntimeError, 'Cargo.toml'):
                package.clean_revision()


if __name__ == '__main__':
    unittest.main()
