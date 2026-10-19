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


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--prefix', type=Path, default=Path.home() / '.local')
    parser.add_argument('--package', type=Path, default=Path(__file__).resolve().parent)
    args = parser.parse_args()
    package = args.package.resolve()
    manifest = json.loads((package / 'version.json').read_text())
    revision = manifest['source_commit']
    if len(revision) != 40 or any(c not in '0123456789abcdef' for c in revision):
        raise ValueError('Invalid source revision')
    for name in ['uu-gui', 'uu', 'eclipse.png']:
        if hashlib.sha256((package / name).read_bytes()).hexdigest() != manifest['sha256'][name]:
            raise ValueError(f'Integrity check failed: {name}')
    for name in ['uu-gui', 'uu']:
        subprocess.run(['ldd', str(package / name)], check=True, capture_output=True)
        deps = subprocess.check_output(['ldd', str(package / name)], text=True)
        if 'not found' in deps: raise RuntimeError(f'Missing shared libraries for {name}: {deps}')
    prefix = args.prefix.expanduser().resolve()
    base = prefix / 'opt' / 'uu-remote-rust'
    releases = base / 'releases'
    releases.mkdir(parents=True, exist_ok=True)
    dest = releases / revision[:12]
    if not dest.exists():
        staging = Path(tempfile.mkdtemp(prefix='.install-', dir=releases))
        try:
            for name in ['uu-gui', 'uu', 'version.json', 'README.txt', 'install.py', 'eclipse.png']:
                shutil.copy2(package / name, staging / name)
            for name in ['uu-gui', 'uu']: (staging / name).chmod(0o755)
            staging.rename(dest)
        finally:
            if staging.exists(): shutil.rmtree(staging)
    else:
        for name in ['uu-gui', 'uu', 'eclipse.png']:
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
    desktop = prefix / 'share' / 'applications' / 'uu-remote-rust.desktop'
    desktop.parent.mkdir(parents=True, exist_ok=True)
    executable = str(base / 'current' / 'uu-gui').replace('\\', '\\\\').replace('"', '\\"').replace('`', '\\`').replace('$', '\\$')
    desktop.write_text('[Desktop Entry]\nType=Application\nName=uu-client\nName[zh_CN]=uu-client\nComment=Windows / Linux remote streaming client\n' + f'Exec="{executable}"\nTryExec={base / "current" / "uu-gui"}\nIcon={base / "current" / "eclipse.png"}\nTerminal=false\nCategories=Network;RemoteAccess;\nStartupNotify=true\nStartupWMClass=uu-client\n')
    subprocess.run(['update-desktop-database', str(desktop.parent)], check=False) if shutil.which('update-desktop-database') else None
    print(json.dumps({'source_commit': revision, 'executable': str(base / 'current' / 'uu-gui'), 'desktop': str(desktop)}, ensure_ascii=False))

if __name__ == '__main__':
    main()
