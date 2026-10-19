import unittest
from check_version import validate


class ReleaseVersionTests(unittest.TestCase):
    def test_matching_stable_and_prerelease_versions(self):
        validate('v0.1.0', '0.1.0')
        validate('v0.2.0-rc.1', '0.2.0-rc.1')

    def test_mismatched_or_unsafe_tags_fail(self):
        for tag in ['v0.1.1', 'nightly', 'v0.1.0;echo x', 'v0.1.0\n', '../v0.1.0', 'v0.1.0-']:
            with self.subTest(tag=tag):
                with self.assertRaises(ValueError): validate(tag, '0.1.0')


if __name__ == '__main__':
    unittest.main()
