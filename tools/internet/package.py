#!/usr/bin/env python3
"""Package and verify the standalone Linux rendezvous server and its Rust runtime."""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('release_source', ROOT / 'tools/release/source.py')
source = importlib.util.module_from_spec(spec)
spec.loader.exec_module(source)
SOURCES = ['Cargo.toml', 'Cargo.lock', 'rust-toolchain', 'rust-toolchain.toml', '.cargo',
           'crates', 'vendor', 'assets', 'tools/internet', 'tools/release/source.py',
           'LICENSE', 'LICENSE-MIT', 'LICENSE-APACHE', 'THIRD-PARTY-NOTICES.txt']
PAYLOAD = ['eclipse-signal', 'LICENSE', 'LICENSE-MIT', 'LICENSE-APACHE',
           'THIRD-PARTY-NOTICES.txt', 'eclipse-signal.service', 'eclipse-turn.service',
           'coturn.conf.example']


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def source_revision():
    top = subprocess.check_output(['git', 'rev-parse', '--show-toplevel'], cwd=ROOT, text=True).strip()
    if Path(top).resolve() != ROOT.resolve():
        raise RuntimeError('Package source must be this checkout, not an archive inside another repository')
    return source.revision(ROOT, SOURCES)


def readelf(path, option):
    return subprocess.check_output(['readelf', option, path], text=True,
                                   env=dict(os.environ, LC_ALL='C'))


def needed(path):
    return re.findall(r'\(NEEDED\).*?\[([^\]]+)\]', readelf(path, '-d'))


def runtime_name(binary):
    libraries = needed(binary)
    if any(name.startswith('libuu_plugin_sdk') for name in libraries):
        raise RuntimeError('The standalone server unexpectedly requires the desktop SDK')
    runtimes = [name for name in libraries if re.fullmatch(r'libstd-[a-zA-Z0-9]+\.so', name)]
    if len(runtimes) != 1:
        raise RuntimeError('Expected exactly one Rust standard runtime in the server ELF NEEDED entries')
    return runtimes[0]


def verify_elf(binary, runtime):
    def identity(path):
        header = readelf(path, '-h')
        fields = [re.search(r'^\s*' + key + r':\s*(.+)$', header, re.M) for key in ['Class', 'Data', 'Machine']]
        if not all(fields):
            raise RuntimeError('Cannot read the packaged ELF architecture')
        return tuple(match.group(1) for match in fields)
    if runtime.name != runtime_name(binary) or identity(binary) != identity(runtime):
        raise RuntimeError('Rust runtime does not match the server ELF name or architecture')
    soname = re.findall(r'\(SONAME\).*?\[([^\]]+)\]', readelf(runtime, '-d'))
    if soname != [runtime.name]:
        raise RuntimeError('Rust runtime SONAME does not match the server ELF dependency')
    paths = re.findall(r'\((?:RUNPATH|RPATH)\).*?\[([^\]]*)\]', readelf(binary, '-d'))
    if not any(part in ('$ORIGIN', '${ORIGIN}') for value in paths for part in value.split(':')):
        raise RuntimeError('The server must find its packaged runtime through $ORIGIN')


def verify_directory(directory, revision):
    manifest = json.loads((directory / 'version.json').read_text())
    runtime = runtime_name(directory / 'eclipse-signal')
    expected = set(PAYLOAD + [runtime])
    if (manifest.get('format_version') != 1 or manifest.get('source_commit') != revision
            or not re.fullmatch(r'(?:[0-9a-f]{40}|[0-9a-f]{64})', revision)
            or set(manifest.get('files', {})) != expected):
        raise RuntimeError('Invalid server package manifest or source identity')
    if {path.name for path in directory.iterdir()} != expected | {'version.json'}:
        raise RuntimeError('Server package directory contains missing or unexpected files')
    for name in expected | {'version.json'}:
        path = directory / name
        if path.is_symlink() or not path.is_file():
            raise RuntimeError('Server package payload must contain regular files only')
        if name != 'version.json' and digest(path) != manifest['files'][name]:
            raise RuntimeError(f'Server package checksum mismatch: {name}')
    verify_elf(directory / 'eclipse-signal', directory / runtime)
    return manifest


def verify_archive(archive, directory_name, manifest):
    expected = {directory_name + '/' + name: checksum for name, checksum in manifest['files'].items()}
    manifest_name = directory_name + '/version.json'
    with tarfile.open(archive, 'r:gz') as bundle:
        members = bundle.getmembers()
        if (len({member.name for member in members}) != len(members)
                or {member.name for member in members} != set(expected) | {manifest_name}):
            raise RuntimeError('Server archive contains duplicate, missing or unexpected paths')
        for member in members:
            if not member.isfile():
                raise RuntimeError('Server archive payload must contain regular files only')
            mode = 0o755 if member.name == directory_name + '/eclipse-signal' else 0o644
            if member.mode & 0o777 != mode:
                raise RuntimeError('Server archive payload has incorrect permissions')
            with bundle.extractfile(member) as stream:
                if member.name == manifest_name:
                    if member.size > 1024 * 1024 or json.load(stream) != manifest:
                        raise RuntimeError('Server archive manifest mismatch')
                elif hashlib.file_digest(stream, 'sha256').hexdigest() != expected[member.name]:
                    raise RuntimeError('Server archive payload checksum mismatch')


def build(target):
    result = subprocess.run(['cargo', 'build', '--release', '--locked', '--target', target,
                             '-p', 'uu-cli', '--bin', 'eclipse-signal',
                             '--message-format=json-render-diagnostics'], cwd=ROOT, text=True, stdout=subprocess.PIPE)
    artifacts = []
    for line in result.stdout.splitlines():
        message = json.loads(line)
        if message.get('reason') == 'compiler-message' and message['message'].get('rendered'):
            print(message['message']['rendered'], file=sys.stderr, end='')
        if (message.get('reason') == 'compiler-artifact' and message['target']['name'] == 'eclipse-signal'
                and message.get('executable')):
            artifacts.append(Path(message['executable']))
    result.check_returncode()
    if len(artifacts) != 1:
        raise RuntimeError('Cargo did not report one standalone server executable')
    return artifacts[0]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, default=ROOT / 'artifacts/packages')
    args = parser.parse_args()
    if platform.system() != 'Linux':
        parser.error('Build this package on Linux')
    revision = source_revision()
    compiler = os.environ.get('RUSTC', 'rustc')
    compiler_version = subprocess.check_output([compiler, '-vV'], text=True)
    target = re.search(r'^host: (.+)$', compiler_version, re.M).group(1)
    if '-linux-' not in target:
        raise RuntimeError('The selected Rust compiler must build for native Linux')
    binary = build(target)
    runtime = Path(subprocess.check_output([compiler, '--print', 'target-libdir', '--target', target], text=True).strip()) / runtime_name(binary)
    verify_elf(binary, runtime)
    name = f'eclipse-signal-linux-{platform.machine()}-{revision[:12]}'
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    destination = output / name
    archive = output / (name + '.tar.gz')
    if destination.exists() or destination.is_symlink():
        if destination.is_symlink() or not destination.is_dir():
            raise RuntimeError('Existing package destination must be a directory')
        verify_directory(destination, revision)
    with tempfile.TemporaryDirectory(prefix='.eclipse-signal-package-', dir=output) as temporary:
        staging = Path(temporary) / name
        staging.mkdir()
        files = {'eclipse-signal': binary, runtime.name: runtime}
        files.update({item: ROOT / item for item in PAYLOAD[1:5]})
        files.update({item: ROOT / 'tools/internet' / item for item in PAYLOAD[5:]})
        for filename, path in files.items():
            shutil.copyfile(path, staging / filename)
            (staging / filename).chmod(0o755 if filename == 'eclipse-signal' else 0o644)
        manifest = {'format_version': 1, 'source_commit': revision, 'platform': platform.machine(),
                    'target': target, 'rustc': compiler_version.splitlines()[0],
                    'build_glibc': list(platform.libc_ver()),
                    'files': {filename: digest(staging / filename) for filename in files}}
        (staging / 'version.json').write_text(json.dumps(manifest, indent=2) + '\n')
        (staging / 'version.json').chmod(0o644)
        verify_directory(staging, revision)
        # Use the package's ELF runpath, not the developer's Rust library paths.
        environment = {key: value for key, value in os.environ.items() if key not in ('LD_LIBRARY_PATH', 'LD_PRELOAD')}
        subprocess.run([staging / 'eclipse-signal', '--help'], cwd=staging, env=environment,
                       check=True, stdout=subprocess.DEVNULL, timeout=10)
        pending = Path(temporary) / archive.name
        with tarfile.open(pending, 'w:gz') as bundle:
            for filename in sorted([*files, 'version.json']):
                bundle.add(staging / filename, arcname=name + '/' + filename, recursive=False)
        verify_archive(pending, name, manifest)
        if source_revision() != revision:
            raise RuntimeError('Source changed during packaging; the archive was not published')
        destination.mkdir(exist_ok=True)
        for filename in [*files, 'version.json']:
            os.replace(staging / filename, destination / filename)
        verify_directory(destination, revision)
        os.replace(pending, archive)
    print(json.dumps({'source_commit': revision, 'directory': str(destination),
                      'archive': str(archive), 'sha256': digest(archive)}))


if __name__ == '__main__':
    main()
