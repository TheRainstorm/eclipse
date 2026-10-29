import contextlib
import io
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
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

    def test_public_pc_constant_is_explicitly_scoped_and_never_overrides_forbidden_values(self):
        # The main checkout must be testable without a concrete plugin. Exercise
        # the exact path/hash policy with generated data, not external source.
        import hashlib
        constant = b'owned-public-policy-fixture'
        data = b'const PUBLIC_PC_SIGNING_KEY: &str = "' + constant + b'";'
        with patch.object(scan, 'PUBLIC_PC_SHA256', hashlib.sha256(constant).hexdigest()):
            self.assertFalse(scan.inspect(scan.PUBLIC_PC_SOURCE, data))
            self.assertTrue(scan.inspect('unapproved.rs', data))
            self.assertTrue(scan.inspect(scan.PUBLIC_PC_SOURCE, data, scan.variants([constant])))
            changed = data.replace(constant, self.SECRET)
            self.assertTrue(scan.inspect(scan.PUBLIC_PC_SOURCE, changed))


class NativeInvitationGuard(unittest.TestCase):
    def test_native_lan_identity_and_saved_pairings_remain_private_after_rename(self):
        import json
        fixtures = [
            dict(id='fixture', certificate=[1], key=[2]),
            dict(client_id='fixture', devices=[dict(certificate=[1], token=[0]*32)]),
            dict(server_id='fixture', clients=[dict(token_hash=[0]*32)]),
        ]
        for fixture in fixtures:
            result = scan.inspect('renamed.json', json.dumps(fixture).encode())
            self.assertTrue(result)
            self.assertNotIn('fixture', str(result))
        for name in ('server-identity.json', 'paired-devices.json', 'authorized-clients.json'):
            self.assertTrue(scan.inspect(name, b'{}'))
        self.assertFalse(scan.inspect('public-metadata.json', b'{"certificate":[1],"fingerprint":"fixture"}'))

    def test_renamed_native_invitation_content_is_also_private(self):
        import json
        data=json.dumps(dict(address='127.0.0.1:12345',certificate=[1],token=[0]*32,expires_at=1)).encode()
        self.assertTrue(scan.inspect('renamed.txt',data))

    def test_local_invitation_files_are_never_allowed_in_source_or_packages(self):
        for name in ('pair.eclipse-invite.json', 'config/share.eclipse-invite.json'):
            self.assertTrue(scan.inspect(name, b'{"version":1}'))


class AccountStateGuard(unittest.TestCase):
    def test_account_state_remains_private_after_rename_or_bom(self):
        import json
        data = json.dumps(dict(identity=dict(client_id='fixture-client'), token='fixture-auth',
                               device_id='fixture-device', user_id='fixture-user')).encode()
        for prefix in (b'', b'\xef\xbb\xbf'):
            result = scan.inspect('renamed.json', prefix + data)
            self.assertTrue(result)
            self.assertNotIn('fixture-auth', str(result))

    def test_account_backups_and_temporary_files_are_private_even_when_empty(self):
        for name in ('config/state.json.backup', 'config/.uu-account-fixture.tmp'):
            self.assertTrue(scan.inspect(name, b''))
        self.assertFalse(scan.inspect('sample.json', b'{"token":"fixture"}'))

    def test_local_device_navigation_is_private_without_an_account_token(self):
        for name in ('config/device-list.fixture.json', 'config/device-list.fixture.123.tmp',
                     'config/gui-settings.json', 'config/gui-settings.backup-fixture.json',
                     'diagnostics/uu-session-fixture.jsonl'):
            self.assertTrue(scan.inspect(name, b'{"favorites":["fixture-id"],"recent":[]}'))


if __name__ == '__main__':
    unittest.main()
