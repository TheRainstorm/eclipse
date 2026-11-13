#!/usr/bin/env python3
"""Resolve and validate immutable CI sources before starting platform builds."""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import tomllib

import check_version


def git(root, *arguments):
    return subprocess.check_output(['git', *arguments], cwd=root, text=True).strip()


def resolve(root, *, event, ref, revision, release_tag=''):
    tag = release_tag or (ref.removeprefix('refs/tags/') if ref.startswith('refs/tags/') else '')
    if tag:
        if not re.fullmatch(r'v\d+\.\d+\.\d+(?:-[0-9A-Za-z]+(?:[.-][0-9A-Za-z]+)*)?', tag):
            raise ValueError('Release tag must include v, for example v0.24.5')
        # Fetch before checkout so a missing remote tag fails once, with a
        # useful message, rather than failing every platform checkout.
        if release_tag:
            subprocess.run(['git', 'fetch', '--no-tags', 'origin',
                            f'refs/tags/{tag}:refs/tags/{tag}'], cwd=root, check=True)
        source = git(root, 'rev-parse', '--verify', tag + '^{commit}')
        if not release_tag and source != revision:
            raise ValueError('Release tag does not match the triggering commit')
    else:
        if not re.fullmatch(r'[0-9a-f]{40}', revision):
            raise ValueError('CI source must be a full commit revision')
        source = git(root, 'rev-parse', '--verify', revision + '^{commit}')
    manifest = tomllib.loads(git(root, 'show', source + ':Cargo.toml'))
    version = manifest['workspace']['package']['version']
    if tag:
        check_version.validate(tag, version)
        git(root, 'cat-file', '-e', source + ':docs/releases/' + tag + '.md')
    build_required = True
    owner = ''
    if event == 'push' and ref == 'refs/heads/master' and re.fullmatch(r'\d+\.\d+\.\d+', version):
        expected = 'v' + version
        tagged = subprocess.run(['git', 'rev-parse', '--verify', expected + '^{commit}'], cwd=root,
                                capture_output=True, text=True)
        if tagged.returncode == 0 and tagged.stdout.strip() == source:
            # An atomic branch+tag push has one owner for the expensive build:
            # its tag workflow. Branch source checks still run normally.
            build_required = False
            owner = expected
    return dict(source_commit=source, release_tag=tag, build_required=build_required, build_owner=owner)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, default=Path.cwd())
    args = parser.parse_args()
    result = resolve(args.root, event=os.environ['GITHUB_EVENT_NAME'], ref=os.environ['GITHUB_REF'],
                     revision=os.environ['GITHUB_SHA'], release_tag=os.environ.get('RELEASE_TAG', ''))
    if path := os.environ.get('GITHUB_OUTPUT'):
        with open(path, 'a') as output:
            for key, value in result.items():
                output.write(f'{key}={str(value).lower() if isinstance(value, bool) else value}\n')
    if result['build_owner']:
        print('Full platform validation belongs to the tag workflow: ' + result['build_owner'])
    print(json.dumps(result))


if __name__ == '__main__':
    main()
