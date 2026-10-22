"""Check pinned-source validation without a network, compiler, or GPU."""
import io
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch

import vpl_runtime as vpl


class DispatcherSourceTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.cache = self.root / 'cache'
        self.cache.mkdir()

    def archive(self, entries=None):
        path = self.root / 'source.tar.gz'
        if entries is None:
            entries = [('CMakeLists.txt', b'project(owned_fixture)'), ('api/vpl.h', b'owned header')]
        with tarfile.open(path, 'w:gz') as bundle:
            for name, data in entries:
                member = tarfile.TarInfo('libvpl-' + vpl.COMMIT + '/' + name)
                if data is None:
                    member.type = tarfile.SYMTYPE
                    member.linkname = '../../outside'
                    bundle.addfile(member)
                else:
                    member.size = len(data)
                    bundle.addfile(member, io.BytesIO(data))
        return path

    def test_corrupt_source_is_rejected_before_compiler_or_network(self):
        source = self.archive()
        with patch.object(vpl.subprocess, 'run') as execute, patch.object(vpl.subprocess, 'check_output') as compiler, patch.object(vpl.urllib.request, 'urlopen') as network:
            with self.assertRaisesRegex(RuntimeError, '校验失败'):
                vpl.build_dispatcher(self.cache, source)
            execute.assert_not_called();compiler.assert_not_called();network.assert_not_called()
        self.assertEqual(list(self.cache.iterdir()), [])

    def test_verified_source_and_cache_revalidation(self):
        archive = self.archive()
        with patch.object(vpl, 'ARCHIVE_SHA256', vpl.sha256(archive)):
            self.assertEqual(vpl.verified_archive(self.cache, archive), archive)
            source = vpl.verified_source(self.cache, archive)
            self.assertEqual((source / 'api/vpl.h').read_bytes(), b'owned header')
            self.assertEqual(vpl.verified_source(self.cache, archive), source)
            (source / 'api/vpl.h').write_bytes(b'modified')
            with self.assertRaisesRegex(RuntimeError, '发生变化'):
                vpl.verified_source(self.cache, archive)

    def test_added_cache_source_is_rejected(self):
        archive = self.archive()
        source = vpl.verified_source(self.cache, archive)
        (source / 'unexpected.cmake').write_bytes(b'unexpected build instructions')
        with self.assertRaisesRegex(RuntimeError, '清单发生变化'):
            vpl.verified_source(self.cache, archive)

    def test_path_traversal_and_links_and_duplicates_do_not_extract(self):
        for entries in ([('../outside', b'data')], [('link', None)], [('same', b'a'), ('same', b'b')]):
            with self.subTest(entries=entries):
                archive = self.archive(entries)
                with self.assertRaises(RuntimeError):
                    vpl.verified_source(self.cache, archive)
                self.assertEqual(list(self.cache.iterdir()), [])
                self.assertFalse((self.root / 'outside').exists())

    def test_oversized_source_is_rejected_before_extraction(self):
        archive = self.archive()
        with patch.object(vpl, 'MAX_SOURCE', 4), self.assertRaisesRegex(RuntimeError, '解压限制'):
            vpl.verified_source(self.cache, archive)
        self.assertEqual(list(self.cache.iterdir()), [])

    def test_corrupt_download_is_not_cached_or_executed(self):
        with patch.object(vpl.urllib.request, 'urlopen', return_value=io.BytesIO(b'corrupt archive')):
            with self.assertRaisesRegex(RuntimeError, '下载校验失败'):
                vpl.verified_archive(self.cache)
        self.assertEqual(list(self.cache.iterdir()), [])

    def test_corrupt_cache_is_preserved_for_review_and_not_downloaded_over(self):
        cached = self.cache / (vpl.COMMIT + '.tar.gz')
        cached.write_bytes(b'corrupt cache')
        with patch.object(vpl.urllib.request, 'urlopen') as network:
            with self.assertRaisesRegex(RuntimeError, '缓存归档校验失败'):
                vpl.verified_archive(self.cache)
            network.assert_not_called()
        self.assertEqual(cached.read_bytes(), b'corrupt cache')


if __name__ == '__main__':
    unittest.main()
