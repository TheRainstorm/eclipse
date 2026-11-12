#!/usr/bin/env python3
"""Prepare version-named release assets from verified platform build packages."""
import argparse
import copy
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import subprocess
import tarfile
import tempfile
import tomllib

import check_version
import download


def prepare(input_dir, output_dir, tag, revision):
    if not download.TAG.fullmatch(tag) or not re.fullmatch('[0-9a-f]{40}', revision):
        raise ValueError('Expected a release tag and full source revision')
    output = Path(output_dir)
    if output.exists():
        raise ValueError('Release output directory already exists; use a fresh directory')
    packages = {}
    for path in sorted(Path(input_dir).rglob('*')):
        if not path.is_file() or not path.name.endswith(('.zip', '.tar.gz')):
            continue
        match = download.ARCHIVE.fullmatch(path.name)
        if not match:
            raise ValueError('Unexpected desktop archive: ' + path.name)
        platform = match[1]
        suffix = 'zip' if platform == 'windows-x64' else 'tar.gz'
        if platform in packages or match[2] not in (revision[:12], tag) or match[3] != suffix:
            raise ValueError('Archive name does not match source or platform: ' + path.name)
        manifest = download.verify_package(path) if platform == 'windows-x64' else download.linux_manifest(path)
        if manifest.get('source_commit') != revision:
            raise ValueError('Archive source does not match release revision: ' + path.name)
        packages[platform] = (path, suffix)
    if set(packages) != {'windows-x64', 'linux-x86_64'}:
        raise ValueError('Release requires one verified Windows and one verified Linux package')
    output.parent.mkdir(parents=True, exist_ok=True)
    records = []
    with tempfile.TemporaryDirectory(prefix='.release-assets-', dir=output.parent) as temporary:
        stage = Path(temporary)
        for platform, (source, suffix) in sorted(packages.items()):
            name = f'eclipse-{platform}-{tag}'
            destination = stage / f'{name}.{suffix}'
            if platform == 'windows-x64':
                shutil.copy2(source, destination)
            else:
                # The manifest and payload bytes stay intact; make the extracted
                # directory as readable as the downloaded filename. No extraction.
                with tarfile.open(source, 'r:gz') as original, tarfile.open(destination, 'w:gz') as archive:
                    directory = tarfile.TarInfo(name)
                    directory.type = tarfile.DIRTYPE
                    directory.mode = 0o755
                    directory.mtime = next(m.mtime for m in original.getmembers() if m.name.endswith('/version.json'))
                    archive.addfile(directory)
                    for member in original.getmembers():
                        if member.isdir():
                            continue
                        renamed = copy.copy(member)
                        renamed.name = name + '/' + PurePosixPath(member.name).name
                        renamed.pax_headers = {k: v for k, v in member.pax_headers.items() if k != 'path'}
                        archive.addfile(renamed, original.extractfile(member))
                download.linux_manifest(destination)
            records.append({'archive': destination.name, 'sha256': download.digest(destination)})
        (stage / 'SHA256SUMS').write_text(''.join(r['sha256'] + '  ' + r['archive'] + '\n' for r in records))
        # No partial output directory becomes visible if verification fails.
        os.rename(stage, output)
    return {'release_tag': tag, 'source_commit': revision, 'assets': records}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--tag', required=True)
    parser.add_argument('--revision', required=True)
    parser.add_argument('--input-dir', type=Path, required=True)
    parser.add_argument('--output-dir', type=Path, required=True)
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[2]
    version = tomllib.loads((root / 'Cargo.toml').read_text())['workspace']['package']['version']
    check_version.validate(args.tag, version)
    tagged = subprocess.check_output(['git', 'rev-parse', '--verify', args.tag + '^{commit}'], cwd=root, text=True).strip()
    if tagged != args.revision:
        parser.error('Release tag does not point to the requested source revision')
    print(json.dumps(prepare(args.input_dir, args.output_dir, args.tag, args.revision), indent=2))


if __name__ == '__main__':
    main()
