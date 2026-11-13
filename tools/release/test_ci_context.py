from pathlib import Path
import subprocess
import tempfile
import unittest

import ci_context


class CiSourceTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)/'source'
        self.root.mkdir()
        self.git('init', '-q')
        (self.root/'Cargo.toml').write_text('[workspace.package]\nversion="1.2.3"\n')
        notes = self.root/'docs/releases/v1.2.3.md'
        notes.parent.mkdir(parents=True)
        notes.write_text('Release fixture\n')
        self.git('add', '.')
        self.git('-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', 'commit', '-qm', 'fixture')
        self.revision = self.git('rev-parse', 'HEAD')
        self.git('tag', 'v1.2.3')

    def git(self, *args):
        return subprocess.check_output(['git', *args], cwd=self.root, text=True).strip()

    def resolve(self, event='push', ref='refs/heads/master', **kwargs):
        return ci_context.resolve(self.root, event=event, ref=ref, revision=self.revision, **kwargs)

    def test_atomic_branch_and_tag_push_only_builds_platforms_for_tag(self):
        self.assertFalse(self.resolve()['build_required'])
        tagged = self.resolve(ref='refs/tags/v1.2.3')
        self.assertTrue(tagged['build_required'])
        self.assertEqual(tagged['source_commit'], self.revision)

    def test_new_branch_commit_and_pull_request_still_build(self):
        self.git('-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', 'commit', '--allow-empty', '-qm', 'next')
        self.revision = self.git('rev-parse', 'HEAD')
        self.assertTrue(self.resolve()['build_required'])
        self.assertTrue(self.resolve(event='pull_request', ref='refs/pull/1/merge')['build_required'])

    def test_manual_checks_are_never_suppressed_by_an_existing_tag(self):
        self.assertTrue(self.resolve(event='workflow_dispatch')['build_required'])

    def test_tag_event_cannot_build_a_different_commit(self):
        self.revision = '0' * 40
        with self.assertRaisesRegex(ValueError, 'triggering commit'):
            self.resolve(ref='refs/tags/v1.2.3')

    def test_invalid_or_version_mismatched_tag_fails_before_build(self):
        with self.assertRaisesRegex(ValueError, 'include v'):
            self.resolve(event='workflow_dispatch', release_tag='1.2.3')
        self.git('tag', 'v9.9.9')
        with self.assertRaisesRegex(ValueError, 'workspace.package.version'):
            self.resolve(ref='refs/tags/v9.9.9')

    def test_manual_tag_uses_its_remote_source_instead_of_workflow_head(self):
        remote = Path(self.temp.name)/'remote.git'
        subprocess.run(['git', 'init', '--bare', '-q', str(remote)], check=True)
        self.git('remote', 'add', 'origin', str(remote))
        self.git('push', '-q', 'origin', 'refs/tags/v1.2.3')
        self.git('-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', 'commit', '--allow-empty', '-qm', 'workflow fix')
        self.revision = self.git('rev-parse', 'HEAD')
        self.git('tag', '-d', 'v1.2.3')
        result = self.resolve(event='workflow_dispatch', release_tag='v1.2.3')
        self.assertNotEqual(result['source_commit'], self.revision)
        self.assertTrue(result['build_required'])

    def test_release_without_curated_notes_fails_before_build(self):
        self.git('tag', '-d', 'v1.2.3')
        self.git('rm', 'docs/releases/v1.2.3.md')
        self.git('-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', 'commit', '-qm', 'missing notes')
        self.revision = self.git('rev-parse', 'HEAD')
        self.git('tag', 'v1.2.3')
        with self.assertRaises(subprocess.CalledProcessError):
            self.resolve(ref='refs/tags/v1.2.3')


if __name__ == '__main__':
    unittest.main()
