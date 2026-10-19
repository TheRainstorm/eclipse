#!/usr/bin/env python3
"""Reject release tags that do not match the version being built."""
import argparse
from pathlib import Path
import re
import tomllib

ROOT = Path(__file__).resolve().parents[2]


def validate(tag, version):
    if not re.fullmatch(r'v\d+\.\d+\.\d+(?:-[0-9A-Za-z]+(?:[.-][0-9A-Za-z]+)*)?', tag):
        raise ValueError('Release tag must be vMAJOR.MINOR.PATCH with an optional prerelease suffix')
    if tag != 'v' + version:
        raise ValueError('Release tag does not match workspace.package.version')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('tag')
    args = parser.parse_args()
    version = tomllib.loads((ROOT / 'Cargo.toml').read_text())['workspace']['package']['version']
    validate(args.tag, version)
    print(f'Validated release version {args.tag}')


if __name__ == '__main__':
    main()
