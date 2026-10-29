"""Published downloads must be verified before replacing preferred packages."""
import hashlib
import io
import json
from pathlib import Path
import shutil
import tarfile
import tempfile
import unittest
from unittest.mock import patch
import zipfile

import download


class DownloadTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)
        self.assets = self.base / 'assets'
        self.assets.mkdir()
        self.output = self.base / 'output'
        self.revision = 'a' * 40
        self.windows = 'uu-client-windows-x64-' + self.revision[:12] + '.zip'
        self.linux = 'uu-client-linux-x86_64-' + self.revision[:12] + '.tar.gz'
        self.make_windows()
        self.make_linux()
        self.hashes()

    def manifest(self, files, format_version=None):
        record = {'source_commit': self.revision, 'sha256': {n: hashlib.sha256(v).hexdigest() for n, v in files.items()}}
        if format_version is not None: record['format_version'] = format_version
        return json.dumps(record).encode()

    def make_windows(self):
        # Inventory is imported by the same strict Windows installer verifier.
        from package import V3_REQUIRED as REQUIRED
        files = {name: ('fixture ' + name).encode() for name in REQUIRED}
        files['version.json'] = self.manifest(files, format_version=3)
        with zipfile.ZipFile(self.assets / self.windows, 'w') as archive:
            for name, data in files.items():
                archive.writestr(name, data)

    def make_linux(self, corrupt=False, symlink=False, sdk=False):
        names = ('uu', 'uu-gui', 'install.py', 'README.txt', 'eclipse.png',
                 'LICENSE', 'LICENSE-MIT', 'LICENSE-APACHE', 'THIRD-PARTY-NOTICES.txt')
        files = {name: ('fixture ' + name).encode() for name in names}
        files['version.json'] = self.manifest(files)
        if sdk:
            runtime = ['libuu_plugin_sdk.so', 'libstd-0123456789abcdef.so']
            files.pop('version.json')
            for name in ['libpyrowave-shared.so.1', *runtime]: files[name] = ('fixture ' + name).encode()
            manifest = json.loads(self.manifest(files, format_version=3))
            manifest.update(plugin_runtime=runtime, plugin_sdk_id='f' * 64)
            files['version.json'] = json.dumps(manifest).encode()
        if corrupt:
            files['uu'] = b'changed executable'
        with tarfile.open(self.assets / self.linux, 'w:gz') as archive:
            for name, data in files.items():
                info = tarfile.TarInfo('package/' + name)
                info.size = len(data)
                if symlink and name == 'uu':
                    info.type = tarfile.SYMTYPE
                    info.linkname = '/outside'
                    info.size = 0
                archive.addfile(info, io.BytesIO(data))

    def hashes(self):
        (self.assets / 'SHA256SUMS').write_text(''.join(
            download.digest(self.assets / name) + '  ' + name + '\n' for name in (self.windows, self.linux)))

    def fake_gh(self, arguments):
        if arguments[:2] == ['release', 'view']:
            return json.dumps({'tagName': 'v1.2.3', 'assets': [
                {'name': name} for name in ('SHA256SUMS', self.windows, self.linux)]})
        if arguments[0] == 'api':
            return self.revision + '\n'
        if arguments[:2] == ['release', 'download']:
            destination = Path(arguments[arguments.index('--dir') + 1])
            for i, item in enumerate(arguments):
                if item == '--pattern':
                    shutil.copy2(self.assets / arguments[i + 1], destination)
            return ''
        self.fail('Unexpected external command: ' + repr(arguments))

    def test_verified_both_platforms_update_compatible_latest_pointers(self):
        with patch.object(download, 'gh', side_effect=self.fake_gh):
            result = download.download('example/project', None, self.output)
        self.assertEqual(result['source_commit'], self.revision)
        for platform, name in (('windows', self.windows), ('linux', self.linux)):
            record = json.loads((self.output / (platform + '-latest.json')).read_text())
            self.assertEqual(record['archive'], name)
            self.assertEqual(record['sha256'], download.digest(self.output / name))
        self.assertFalse(list(self.output.glob('.release-download-*')))

    def test_corrupt_second_platform_preserves_previous_pointer_and_packages(self):
        self.output.mkdir()
        previous = b'{"archive":"previous.zip"}\n'
        (self.output / 'windows-latest.json').write_bytes(previous)
        self.make_linux(corrupt=True)
        self.hashes()  # Outer checksum is correct; inner executable is wrong.
        with patch.object(download, 'gh', side_effect=self.fake_gh):
            with self.assertRaisesRegex(ValueError, 'payload hash'):
                download.download('example/project', None, self.output)
        self.assertEqual((self.output / 'windows-latest.json').read_bytes(), previous)
        self.assertFalse((self.output / self.windows).exists())
        self.assertFalse(list(self.output.glob('.release-download-*')))

    def test_source_manifest_must_match_tag_before_publishing(self):
        self.revision = 'b' * 40
        # Rename archives to the new tag prefix without changing their manifests.
        for old in (self.windows, self.linux):
            (self.assets / old).rename(self.assets / old.replace('a' * 12, 'b' * 12))
        self.windows=self.windows.replace('a' * 12,'b' * 12)
        self.linux=self.linux.replace('a' * 12,'b' * 12)
        self.hashes()
        with patch.object(download, 'gh', side_effect=self.fake_gh):
            with self.assertRaisesRegex(ValueError,'source'):
                download.download('example/project',None,self.output)
        self.assertFalse(list(self.output.glob('*latest.json')))

    def test_outer_checksum_is_verified_even_when_payload_is_valid(self):
        (self.assets / self.windows).write_bytes((self.assets / self.windows).read_bytes()+b'changed')
        with patch.object(download, 'gh', side_effect=self.fake_gh):
            with self.assertRaisesRegex(ValueError,'Archive hash mismatch'):
                download.download('example/project',None,self.output)
        self.assertFalse(list(self.output.glob('*latest.json')))

    def test_platform_selection_does_not_replace_another_platform(self):
        self.make_linux(corrupt=True)
        self.hashes()
        with patch.object(download, 'gh', side_effect=self.fake_gh):
            download.download('example/project', 'v1.2.3', self.output, 'windows')
        self.assertTrue((self.output / self.windows).exists())
        self.assertFalse((self.output / 'linux-latest.json').exists())

    def test_symlink_payload_is_rejected_without_extraction(self):
        self.make_linux(symlink=True)
        with self.assertRaisesRegex(ValueError, 'entry'):
            download.linux_manifest(self.assets / self.linux)

    def test_shared_sdk_payload_is_verified_and_corruption_preserves_download_state(self):
        self.make_linux(sdk=True)
        self.hashes()
        with patch.object(download, 'gh', side_effect=self.fake_gh):
            download.download('example/project', 'v1.2.3', self.output)
        previous = (self.output / 'linux-latest.json').read_bytes()
        self.make_linux(corrupt=True, sdk=True)
        self.hashes()
        with patch.object(download, 'gh', side_effect=self.fake_gh), self.assertRaisesRegex(ValueError, 'payload hash'):
            download.download('example/project', 'v1.2.3', self.output)
        self.assertEqual((self.output / 'linux-latest.json').read_bytes(), previous)

    def test_invalid_requested_tag_never_reaches_github(self):
        with patch.object(download, 'gh') as github:
            with self.assertRaises(ValueError):
                download.download('example/project', '--web', self.output)
        github.assert_not_called()

    def test_checksum_paths_and_duplicate_entries_are_rejected(self):
        for text in ('a' * 64 + '  ../outside.zip\n',
                     ('a' * 64 + '  ' + self.windows + '\n') * 2,
                     'not-a-digest  ' + self.windows + '\n'):
            with self.subTest(text=text):
                with self.assertRaises(ValueError):
                    download.checksums(text)


if __name__ == '__main__':
    unittest.main()
