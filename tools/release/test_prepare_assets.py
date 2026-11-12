"""Publishing labels change filenames, never the verified product source."""
from pathlib import PurePosixPath
import tarfile
import unittest

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


if __name__ == '__main__':
    unittest.main()
