#!/usr/bin/env python3
"""Guard service signing credentials without printing matched values (Python 3.11+)."""
import argparse
import base64
import json
from pathlib import Path, PurePosixPath
import re
import subprocess
import tarfile
import zipfile

# A targeted guard, not a replacement for a full secret scanner or provider rotation.
RUST_KEY = re.compile(rb'(?m)^\s*(?:pub\s+)?const\s+(?:NATIVE_SIGN_KEY|WEB_SIGN_KEY|SERVICE_SIGN_KEY)\b[^\n]*?b?"([^"\r\n]+)"')
PLACEHOLDERS = (b'REMOVED_', b'test-', b'test_', b'fake-', b'example-', b'<')
MAX_ARTIFACT_BYTES = 512 * 1024 * 1024


def variants(values):
    output = set()
    for raw in values:
        value = raw.removeprefix(b'\xef\xbb\xbf').rstrip(b'\r\n')
        if len(value) < 16:
            raise ValueError('Forbidden-value files must contain a credential of at least 16 bytes')
        output.update((value, value.hex().encode(), value.hex().upper().encode(), base64.b64encode(value)))
        if value.isascii():
            output.update((value.decode().encode('utf-16le'), value.decode().encode('utf-16be')))
    return output


def inspect(name, data, forbidden=(), source=False):
    issues = []
    base = PurePosixPath(name).name
    if base in ('signing-key', 'state.json', '.env') or base.startswith('signing-key.'):
        issues.append('private credential/configuration file')
    if source and base.lower().endswith(('.exe', '.dll')):
        issues.append('compiled binary committed to source')
    if any(value in data for value in forbidden):
        issues.append('forbidden credential value or encoding')
    if any(not match.group(1).startswith(PLACEHOLDERS) for match in RUST_KEY.finditer(data)):
        issues.append('embedded service signing constant')
    return [dict(path=name, reason=reason) for reason in issues]


def history(root, forbidden):
    names = {}
    for record in subprocess.check_output(['git', 'rev-list', '--objects', '--all'], cwd=root).splitlines():
        obj, _, name = record.partition(b' ')
        names.setdefault(obj, name.decode('utf-8', errors='replace'))
    result = []
    proc = subprocess.Popen(['git', 'cat-file', '--batch'], cwd=root, stdin=subprocess.PIPE, stdout=subprocess.PIPE)
    try:
        for obj, name in names.items():
            proc.stdin.write(obj + b'\n'); proc.stdin.flush()
            header = proc.stdout.readline().split()
            if len(header) != 3:
                raise ValueError('Cannot read a Git object')
            data = proc.stdout.read(int(header[2])); proc.stdout.read(1)
            kind = header[1]
            if kind == b'blob':
                result += inspect(name, data, forbidden)
            elif any(value in data for value in forbidden):
                result.append(dict(path=obj.decode(), reason='forbidden credential in Git metadata'))
    finally:
        proc.stdin.close(); proc.stdout.close(); proc.wait()
    return result


def artifact(path, forbidden):
    result = []
    def check(name, data):
        if name.startswith('/') or '..' in PurePosixPath(name).parts:
            result.append(dict(path=name, reason='unsafe archive path'))
        result.extend(inspect(name, data, forbidden))
    if zipfile.is_zipfile(path):
        with zipfile.ZipFile(path) as archive:
            if sum(info.file_size for info in archive.infolist()) > MAX_ARTIFACT_BYTES:
                raise ValueError('Artifact exceeds scan size limit')
            for info in archive.infolist():
                if not info.is_dir(): check(info.filename, archive.read(info))
    elif tarfile.is_tarfile(path):
        with tarfile.open(path) as archive:
            total = 0
            for member in archive:
                total += member.size
                if total > MAX_ARTIFACT_BYTES: raise ValueError('Artifact exceeds scan size limit')
                if member.isfile():
                    with archive.extractfile(member) as stream: check(member.name, stream.read())
                elif not member.isdir(): result.append(dict(path=member.name, reason='unexpected archive link or special file'))
    else:
        check(path.name, path.read_bytes())
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument('--history', action='store_true', help='Also scan all reachable Git objects')
    parser.add_argument('--forbid-file', type=Path, action='append', default=[], help='Private file containing one credential; never print its contents')
    parser.add_argument('--artifact', type=Path, action='append', default=[], help='Scan a ZIP, tarball or executable')
    args = parser.parse_args()
    forbidden = variants([file.read_bytes() for file in args.forbid_file])
    names = subprocess.check_output(['git', 'ls-files', '-z'], cwd=args.root).split(b'\0')
    issues = []
    for name in names:
        if not name: continue
        path = name.decode('utf-8')
        issues += inspect(path, (args.root / path).read_bytes(), forbidden, source=True)
    if args.history: issues += history(args.root, forbidden)
    for path in args.artifact: issues += artifact(path, forbidden)
    if issues:
        print(json.dumps({'result':'failed', 'findings':issues}, ensure_ascii=False, indent=2))
        return 1
    print('Credential guard passed; no matched credential values were printed.')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
