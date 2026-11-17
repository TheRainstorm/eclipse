"""Publishing labels change filenames, never the verified product source."""
from pathlib import PurePosixPath
import hashlib
import io
import json
import tarfile
import unittest
import zipfile

import download
import prepare_assets
import test_download


class PrepareTests(unittest.TestCase):
    def setUp(self):
        self.fixture = test_download.DownloadTests()
        self.fixture.setUp()
        self.addCleanup(self.fixture.doCleanups)
        self.output = self.fixture.base / 'published'

    def prepare(self, tag='v1.2.3'):
        return prepare_assets.prepare(self.fixture.assets, self.output, tag, self.fixture.revision)

    def test_both_assets_and_linux_directory_use_tag_and_preserve_payloads(self):
        old_windows = download.digest(self.fixture.assets / self.fixture.windows)
        old_linux = download.linux_manifest(self.fixture.assets / self.fixture.linux)
        result = self.prepare('v1.2.3-rc.1')
        names = {r['archive'] for r in result['assets']}
        self.assertEqual(names, {'eclipse-windows-x64-v1.2.3-rc.1.zip', 'eclipse-linux-x86_64-v1.2.3-rc.1.tar.gz'})
        self.assertEqual(download.digest(self.output / 'eclipse-windows-x64-v1.2.3-rc.1.zip'), old_windows)
        linux = self.output / 'eclipse-linux-x86_64-v1.2.3-rc.1.tar.gz'
        self.assertEqual(download.linux_manifest(linux), old_linux)
        with tarfile.open(linux) as archive:
            self.assertEqual({PurePosixPath(m.name).parts[0] for m in archive.getmembers()}, {'eclipse-linux-x86_64-v1.2.3-rc.1'})
        sums = download.checksums((self.output / 'SHA256SUMS').read_text())
        self.assertEqual(set(sums), names)
        for name in names:
            self.assertEqual(sums[name], download.digest(self.output / name))
        self.assertTrue((self.fixture.assets / self.fixture.windows).exists())

    def test_missing_platform_never_publishes_an_incomplete_directory(self):
        (self.fixture.assets / self.fixture.linux).unlink()
        with self.assertRaisesRegex(ValueError, 'Windows and one verified Linux'):
            self.prepare()
        self.assertFalse(self.output.exists())

    def test_complete_linux_package_keeps_nested_plugin_and_uninstaller(self):
        self.fixture.make_linux(sdk=True)
        path = self.fixture.assets/self.fixture.linux
        with tarfile.open(path) as archive:
            files = {PurePosixPath(member.name).name: archive.extractfile(member).read()
                     for member in archive.getmembers() if member.isfile()}
        manifest = json.loads(files.pop('version.json'))
        for old,new in [('uu','eclipse-cli'),('uu-gui','eclipse')]: files[new] = files.pop(old)
        manifest.update(format_version=4, uninstaller='uninstall.py', target='x86_64-unknown-linux-gnu',
                        bundled_plugins={'wallpaper-collects':'libcollects.so'})
        prefix = 'plugins/wallpaper-collects/'
        for name in ('libcollects.so','README.md','LICENSE-MIT','LICENSE-APACHE','THIRD-PARTY-NOTICES.txt'):
            files[prefix+name] = ('fixture '+name).encode()
        files['uninstall.py'] = b'owned uninstaller fixture'
        files[prefix+'plugin.json'] = json.dumps(dict(id='wallpaper-collects', api_version=1, sdk_id='f'*64,
            library='libcollects.so', library_sha256=hashlib.sha256(files[prefix+'libcollects.so']).hexdigest())).encode()
        files[prefix+'build.json'] = json.dumps(dict(sdk_id='f'*64, target=manifest['target'],
            source_commit=manifest['source_commit'], application_source_commit=manifest['source_commit'])).encode()
        manifest['sha256'] = {name:hashlib.sha256(data).hexdigest() for name,data in files.items()}
        files['version.json'] = json.dumps(manifest).encode()
        with tarfile.open(path,'w:gz') as archive:
            for name,data in files.items():
                info = tarfile.TarInfo('package/'+name); info.size = len(data)
                archive.addfile(info,io.BytesIO(data))
        self.prepare()
        delivered = self.output/'eclipse-linux-x86_64-v1.2.3.tar.gz'
        self.assertEqual(download.linux_manifest(delivered), manifest)
        with tarfile.open(delivered) as archive:
            self.assertEqual(archive.extractfile('eclipse-linux-x86_64-v1.2.3/plugins/wallpaper-collects/libcollects.so').read(), files[prefix+'libcollects.so'])

    def test_corrupt_payload_never_publishes_a_directory(self):
        self.fixture.make_linux(corrupt=True)
        with self.assertRaisesRegex(ValueError, 'payload hash'):
            self.prepare()
        self.assertFalse(self.output.exists())

    def test_renamed_archive_cannot_disguise_another_source(self):
        self.fixture.use_tag_names('v1.2.3')
        self.fixture.revision = 'b' * 40
        with self.assertRaisesRegex(ValueError, 'source'):
            self.prepare()
        self.assertFalse(self.output.exists())

    def test_existing_reviewed_assets_are_not_replaced(self):
        self.prepare()
        previous = (self.output / 'SHA256SUMS').read_bytes()
        with self.assertRaisesRegex(ValueError, 'already exists'):
            self.prepare()
        self.assertEqual((self.output / 'SHA256SUMS').read_bytes(), previous)

    def plugin(self, target='x86_64-pc-windows-gnu', sdk=None, revision=None, corrupt=False):
        from package import REQUIRED
        runtime = ['uu_plugin_sdk.dll', 'std-0123456789abcdef.dll']
        files = {n: ('fixture ' + n).encode() for n in [*REQUIRED, *runtime]}
        manifest = json.loads(self.fixture.manifest(files, format_version=5))
        manifest.update(plugin_sdk_id='f' * 64, plugin_runtime=runtime)
        with zipfile.ZipFile(self.fixture.assets / self.fixture.windows, 'w') as archive:
            for name, data in files.items(): archive.writestr(name, data)
            archive.writestr('version.json', json.dumps(manifest))
        self.fixture.make_linux(sdk=True)
        library = 'collects.dll' if 'windows' in target else 'libcollects.so'
        payload = b'plugin fixture'
        manifest = dict(api_version=1, id='wallpaper-collects', sdk_id=sdk or 'f' * 64,
                        library=library, library_sha256=hashlib.sha256(payload).hexdigest())
        build = dict(sdk_id=manifest['sdk_id'], source_commit=revision or self.fixture.revision,
                     application_source_commit=self.fixture.revision, target=target)
        name = f'eclipse-plugin-wallpaper-collects-{target}-{self.fixture.revision[:12]}.zip'
        with zipfile.ZipFile(self.fixture.assets / name, 'w') as archive:
            prefix = 'plugins/wallpaper-collects/'
            archive.writestr(prefix + 'plugin.json', json.dumps(manifest))
            archive.writestr(prefix + 'build.json', json.dumps(build))
            archive.writestr(prefix + library, b'corrupt' if corrupt else payload)
        return name

    def test_optional_plugins_are_tag_named_and_share_the_release_checksums(self):
        self.plugin()
        self.plugin('x86_64-unknown-linux-gnu')
        result = self.prepare()
        names = {r['archive'] for r in result['assets']}
        self.assertEqual(len(names), 4)
        self.assertIn('eclipse-plugin-wallpaper-collects-x86_64-pc-windows-gnu-v1.2.3.zip', names)
        self.assertIn('eclipse-plugin-wallpaper-collects-x86_64-unknown-linux-gnu-v1.2.3.zip', names)
        self.assertEqual(set(download.checksums((self.output / 'SHA256SUMS').read_text())), names)

    def test_plugin_sdk_must_match_the_platform_application(self):
        self.plugin(sdk='e' * 64)
        with self.assertRaisesRegex(ValueError, 'SDK'): self.prepare()
        self.assertFalse(self.output.exists())

    def test_plugin_source_must_match_the_release(self):
        self.plugin(revision='b' * 40)
        with self.assertRaisesRegex(ValueError, 'source'): self.prepare()
        self.assertFalse(self.output.exists())

    def test_changed_plugin_library_is_rejected_before_publication(self):
        self.plugin(corrupt=True)
        with self.assertRaisesRegex(ValueError, 'payload'): self.prepare()
        self.assertFalse(self.output.exists())


if __name__ == '__main__':
    unittest.main()
