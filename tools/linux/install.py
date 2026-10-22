#!/usr/bin/env python3
"""Verify and install this package into a versioned, per-user Linux directory."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

LICENSES = ['LICENSE', 'LICENSE-MIT', 'LICENSE-APACHE', 'THIRD-PARTY-NOTICES.txt']

PAYLOAD = ['uu-gui', 'uu', 'README.txt', 'install.py', 'eclipse.png', *LICENSES]


def verify_package(package):
    manifest = json.loads((package / 'version.json').read_text())
    revision = manifest.get('source_commit')
    if not isinstance(revision, str) or len(revision) != 40 or any(c not in '0123456789abcdef' for c in revision):
        raise ValueError('Invalid source revision')
    hashes = manifest.get('sha256')
    if not isinstance(hashes, dict) or set(hashes) != set(PAYLOAD):
        raise ValueError('Invalid package hash manifest')
    for name in PAYLOAD:
        path = package / name
        if path.is_symlink() or not path.is_file() or hashlib.sha256(path.read_bytes()).hexdigest() != hashes[name]:
            raise ValueError(f'Integrity check failed: {name}')
    return manifest


def desktop_value(value):
    return str(value).replace('\\', '\\\\').replace('\n', '\\n').replace('\r', '\\r').replace('\t', '\\t')


def desktop_entry(base, legacy=False):
    executable = str(base / 'current' / 'uu-gui')
    # Exec quoting is applied after the desktop string's own escape layer.
    # Percent is a field code even inside quotes; a literal percent is %%.
    def quoted_argument(value):
        quoted = ''.join('\\' + c if c in '\\"`$' else c for c in str(value)).replace('%', '%%')
        return '"' + quoted + '"'
    # GLib validates argv[0] before expanding %%; keep the program with a
    # possibly literal percent in argv[1]. env invokes it directly, no shell.
    launcher = shutil.which('env')
    if not launcher:
        raise RuntimeError('The standard env utility is required for the desktop launcher')
    command = desktop_value(quoted_argument(launcher) + ' ' + quoted_argument(executable))
    return ('[Desktop Entry]\nType=Application\nName=uu-client\nName[zh_CN]=uu-client\n'
            'Comment=Windows / Linux remote streaming client\n'
            f'Exec={command}\n'
            f'TryExec={desktop_value(executable)}\nIcon={desktop_value(base / "current" / "eclipse.png")}\n'
            'Terminal=false\nCategories=Network;RemoteAccess;\nStartupNotify=true\nStartupWMClass=uu-client\n'
            + ('NoDisplay=true\n' if legacy else ''))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--prefix', type=Path, default=Path.home() / '.local')
    parser.add_argument('--package', type=Path, default=Path(__file__).resolve().parent)
    args = parser.parse_args()
    package = args.package.resolve()
    manifest = verify_package(package)
    revision = manifest['source_commit']
    for name in ['uu-gui', 'uu']:
        deps = subprocess.check_output(['ldd', str(package / name)], text=True)
        if 'not found' in deps: raise RuntimeError(f'Missing shared libraries for {name}: {deps}')
    prefix = args.prefix.expanduser().resolve()
    if any(ord(c) < 32 for c in str(prefix)):
        raise ValueError('Installation prefix must not contain control characters')
    base = prefix / 'opt' / 'uu-remote-rust'
    desktop_contents = [(False, desktop_entry(base)), (True, desktop_entry(base, legacy=True))]
    releases = base / 'releases'
    releases.mkdir(parents=True, exist_ok=True)
    dest = releases / revision[:12]
    if not dest.exists():
        staging = Path(tempfile.mkdtemp(prefix='.install-', dir=releases))
        try:
            for name in [*PAYLOAD, 'version.json']:
                shutil.copy2(package / name, staging / name)
            for name in ['uu-gui', 'uu']: (staging / name).chmod(0o755)
            staging.rename(dest)
        finally:
            if staging.exists(): shutil.rmtree(staging)
    else:
        for name in PAYLOAD:
            if hashlib.sha256((dest / name).read_bytes()).hexdigest() != manifest['sha256'][name]:
                raise ValueError('Existing release differs; refusing to overwrite a running installation')
    def link(target, path):
        path.parent.mkdir(parents=True, exist_ok=True)
        pending = path.with_name(path.name + f'.tmp-{os.getpid()}')
        try:
            pending.symlink_to(target)
            pending.replace(path)
        finally: pending.unlink(missing_ok=True)
    link(dest, base / 'current')
    link(base / 'current' / 'uu-gui', prefix / 'bin' / 'uu-client')
    link(base / 'current' / 'uu-gui', prefix / 'bin' / 'uu-remote-rust')
    link(base / 'current' / 'uu', prefix / 'bin' / 'uu-rust')
    # The filename must match the window's Wayland app_id (uu-client).
    desktop = prefix / 'share' / 'applications' / 'uu-client.desktop'
    desktop.parent.mkdir(parents=True, exist_ok=True)
    for legacy, contents in desktop_contents:
        path = desktop.with_name('uu-remote-rust.desktop') if legacy else desktop
        pending = path.with_name(path.name + f'.tmp-{os.getpid()}')
        try:
            pending.write_text(contents, encoding='utf-8')
            pending.replace(path)
        finally:
            pending.unlink(missing_ok=True)
    subprocess.run(['update-desktop-database', str(desktop.parent)], check=False) if shutil.which('update-desktop-database') else None
    print(json.dumps({'source_commit': revision, 'executable': str(base / 'current' / 'uu-gui'), 'desktop': str(desktop)}, ensure_ascii=False))

if __name__ == '__main__':
    main()
