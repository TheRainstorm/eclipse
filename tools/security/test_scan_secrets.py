import contextlib
import io
from pathlib import Path
import tempfile
import unittest
import zipfile
import scan_secrets as scan


class CredentialGuardTests(unittest.TestCase):
    SECRET = b'only-a-unit-test-credential-42'

    def test_detects_value_and_encodings_without_returning_value(self):
        forbidden = scan.variants([self.SECRET + b'\r\n'])
        for value in forbidden:
            result = scan.inspect('sample.rs', b'prefix' + value + b'suffix', forbidden)
            self.assertTrue(result)
            self.assertNotIn(self.SECRET.decode(), str(result))

    def test_detects_embedded_constant_and_accepts_removed_placeholder(self):
        prefix = b'pub ' + b'const NATIVE_SIGN_KEY: &[u8] = b"'
        self.assertTrue(scan.inspect('sign.rs', prefix + self.SECRET + b'";'))
        self.assertFalse(scan.inspect('sign.rs', prefix + b'REMOVED_SERVICE_KEY_0000";'))

    def test_rejects_credentials_and_source_binaries_but_accepts_release_exe(self):
        self.assertTrue(scan.inspect('config/signing-key', b'private'))
        self.assertTrue(scan.inspect('config/state.json', b'{}'))
        self.assertTrue(scan.inspect('dist/client.exe', b'MZ', source=True))
        self.assertFalse(scan.inspect('client.exe', b'MZ', source=False))

    def test_scans_compressed_payload_and_rejects_unsafe_paths(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / 'release.zip'
            with zipfile.ZipFile(path, 'w', compression=zipfile.ZIP_DEFLATED) as archive:
                archive.writestr('client.exe', b'MZ' + self.SECRET)
                archive.writestr('../config/signing-key', b'private')
            result = scan.artifact(path, scan.variants([self.SECRET]))
            self.assertTrue(any(f['reason'] == 'unsafe archive path' for f in result))
            self.assertTrue(any('forbidden credential' in f['reason'] for f in result))
            self.assertNotIn(self.SECRET.decode(), str(result))

    def test_accepts_clean_package(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / 'release.zip'
            with zipfile.ZipFile(path, 'w') as archive:
                archive.writestr('client.exe', b'MZ clean')
                archive.writestr('LICENSE', b'license')
            self.assertFalse(scan.artifact(path, scan.variants([self.SECRET])))


class NativeInvitationGuard(unittest.TestCase):
    def test_renamed_native_invitation_content_is_also_private(self):
        import json
        data=json.dumps(dict(address='127.0.0.1:12345',certificate=[1],token=[0]*32,expires_at=1)).encode()
        self.assertTrue(scan.inspect('renamed.txt',data))

    def test_local_invitation_files_are_never_allowed_in_source_or_packages(self):
        for name in ('pair.eclipse-invite.json', 'config/share.eclipse-invite.json'):
            self.assertTrue(scan.inspect(name, b'{"version":1}'))


if __name__ == '__main__':
    unittest.main()
