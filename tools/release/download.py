#!/usr/bin/env python3
"""Download and verify published packages without running a Rust build."""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path, PurePosixPath
import re
import subprocess
import sys
import tarfile
import tempfile
import zipfile

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'tools/windows'))
from sync import verify_package  # Reuse the installer's strict ZIP validation.

_linux_spec = importlib.util.spec_from_file_location('release_linux_installer', ROOT / 'tools/linux/install.py')
_linux_installer = importlib.util.module_from_spec(_linux_spec)
_linux_spec.loader.exec_module(_linux_installer)

TAG = re.compile(r'v\d+\.\d+\.\d+(?:-[0-9A-Za-z]+(?:[.-][0-9A-Za-z]+)*)?')
ARCHIVE = re.compile(r'(?:eclipse|uu-client)-(windows-x64|linux-x86_64)-([0-9a-f]{12})\.(zip|tar\.gz)')


def gh(arguments):
    return subprocess.check_output(['gh', *arguments], text=True)


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def checksums(text):
    result = {}
    for line in text.splitlines():
        fields = line.split()
        if len(fields) != 2 or not re.fullmatch('[0-9a-fA-F]{64}', fields[0]):
            raise ValueError('Invalid SHA256SUMS')
        name = fields[1].removeprefix('*')
        if not ARCHIVE.fullmatch(name) or name in result:
            raise ValueError('Unsafe or duplicate archive name in SHA256SUMS')
        result[name] = fields[0].lower()
    if not result:
        raise ValueError('Empty SHA256SUMS')
    return result


def linux_manifest(path):
    # Inspect without extracting files or executing the downloaded installer.
    with tarfile.open(path, 'r:gz') as archive:
        members = archive.getmembers()
        if len(members) > 64:
            raise ValueError('Unexpected Linux archive size')
        files = {}
        total = 0
        for member in members:
            parts = PurePosixPath(member.name).parts
            if len(parts) not in (1, 2) or any(p in ('.', '..') for p in parts) or member.name.startswith('/') or '\\' in member.name:
                raise ValueError('Unsafe Linux archive path')
            if member.isdir():
                continue
            if not member.isfile() or len(parts) != 2 or member.name in files:
                raise ValueError('Unexpected Linux archive entry')
            total += member.size
            if member.size > 128 * 1024**2 or total > 512 * 1024**2:
                raise ValueError('Unexpected Linux payload size')
            files[member.name] = archive.extractfile(member).read()
        manifests = [name for name in files if name.endswith('/version.json')]
        if len(manifests) != 1:
            raise ValueError('Linux version manifest is missing or ambiguous')
        name = manifests[0]
        manifest = json.loads(files[name])
        prefix = name[:-len('version.json')]
        required = set(_linux_installer.required_files(manifest))
        if set(manifest.get('sha256', {})) != required or set(files) != {prefix + n for n in required | {'version.json'}}:
            raise ValueError('Unexpected Linux payload inventory')
        for filename, expected in manifest['sha256'].items():
            if hashlib.sha256(files[prefix + filename]).hexdigest() != expected:
                raise ValueError('Linux payload hash mismatch: ' + filename)
        return manifest


def download(repo, tag, output, platform='all'):
    if not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9_.-]*/[A-Za-z0-9][A-Za-z0-9_.-]*', repo):
        raise ValueError('Repository must be OWNER/NAME')
    if tag is not None and not TAG.fullmatch(tag):
        raise ValueError('Unexpected requested release tag')
    requested = tag
    release = json.loads(gh(['release', 'view', *([tag] if tag else []), '--repo', repo, '--json', 'tagName,assets']))
    tag = release['tagName']
    if not TAG.fullmatch(tag) or (requested is not None and tag != requested):
        raise ValueError('Unexpected release tag')
    revision = gh(['api', f'repos/{repo}/commits/{tag}', '--jq', '.sha']).strip()
    if not re.fullmatch('[0-9a-f]{40}', revision):
        raise ValueError('Release tag did not resolve to a commit')
    selected = {}
    for asset in release['assets']:
        name = asset['name']
        match = ARCHIVE.fullmatch(name)
        if not match:
            continue
        target = 'windows' if match[1] == 'windows-x64' else 'linux'
        if platform != 'all' and target != platform:
            continue
        if target in selected or match[2] != revision[:12] or match[3] != ('zip' if target == 'windows' else 'tar.gz'):
            raise ValueError('Release archive does not match its tag')
        selected[target] = name
    if set(selected) != ({'windows', 'linux'} if platform == 'all' else {platform}):
        raise ValueError('Requested platform package is missing')
    if not any(asset['name'] == 'SHA256SUMS' for asset in release['assets']):
        raise ValueError('Release checksums are missing')
    output = Path(output)
    output.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='.release-download-', dir=output) as temporary:
        stage = Path(temporary)
        arguments = ['release', 'download', tag, '--repo', repo, '--dir', str(stage)]
        for name in ['SHA256SUMS', *selected.values()]:
            arguments += ['--pattern', name]
        gh(arguments)
        hashes = checksums((stage / 'SHA256SUMS').read_text())
        records = {}
        for target, name in selected.items():
            path = stage / name
            actual = digest(path)
            if actual != hashes.get(name):
                raise ValueError('Archive hash mismatch: ' + name)
            manifest = verify_package(path) if target == 'windows' else linux_manifest(path)
            if manifest.get('source_commit') != revision:
                raise ValueError('Payload source differs from release tag')
            if (output / name).exists() and digest(output / name) != actual:
                raise ValueError('Existing archive has different contents: ' + name)
            records[target] = {'source_commit': revision, 'archive': name, 'sha256': actual, 'release_tag': tag}
        # Validate every selected package before changing any latest pointer.
        for record in records.values():
            os.replace(stage / record['archive'], output / record['archive'])
        for target, record in records.items():
            pointer = stage / (target + '-latest.json')
            pointer.write_text(json.dumps(record, indent=2) + '\n')
            os.replace(pointer, output / pointer.name)
        return {'release_tag': tag, 'source_commit': revision, 'packages': records}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--tag', help='Published version; defaults to the latest stable release')
    parser.add_argument('--repo', default='TheRainstorm/eclipse')
    parser.add_argument('--platform', choices=('all', 'windows', 'linux'), default='all')
    parser.add_argument('--output-dir', type=Path, default=ROOT / 'artifacts/packages')
    args = parser.parse_args()
    try:
        result = download(args.repo, args.tag, args.output_dir, args.platform)
    except (OSError, ValueError, subprocess.CalledProcessError, tarfile.TarError, zipfile.BadZipFile, RuntimeError) as error:
        parser.exit(1, 'Download failed: ' + str(error) + '\n')
    print(json.dumps(result, ensure_ascii=False, indent=2))
    print('Verified packages: ' + str(args.output_dir.resolve()))


if __name__ == '__main__':
    main()
