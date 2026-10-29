#!/usr/bin/env python3
"""Verify and install this package into a versioned, per-user Linux directory."""
import argparse
import hashlib
import json
import re
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

LICENSES = ['LICENSE', 'LICENSE-MIT', 'LICENSE-APACHE', 'THIRD-PARTY-NOTICES.txt']

LEGACY_PAYLOAD = ['uu-gui', 'uu', 'README.txt', 'install.py', 'eclipse.png', *LICENSES]
V2_PAYLOAD = [*LEGACY_PAYLOAD,'libpyrowave-shared.so.1']
PAYLOAD = ['eclipse','eclipse-cli',*V2_PAYLOAD[2:]]
def executables(manifest):
    return ('eclipse','eclipse-cli') if manifest.get('format_version',1)>=4 else ('uu-gui','uu')
def required_files(manifest):
    version=manifest.get('format_version',1)
    if type(version) is not int or version not in (1,2,3,4):
        raise ValueError('Unsupported Linux package format version')
    if version>=3:
        runtime=manifest.get('plugin_runtime')
        if (not isinstance(runtime,list) or len(runtime)!=2 or runtime[0]!='libuu_plugin_sdk.so'
            or not isinstance(runtime[1],str) or not re.fullmatch(r'libstd-[0-9a-f]+\.so',runtime[1])
            or not re.fullmatch(r'[0-9a-f]{64}',manifest.get('plugin_sdk_id',''))):
            raise ValueError('Invalid plugin SDK runtime manifest')
        return [*(PAYLOAD if version==4 else V2_PAYLOAD),*runtime]
    return V2_PAYLOAD if version==2 else LEGACY_PAYLOAD


def verify_package(package):
    metadata = package / 'version.json'
    if metadata.is_symlink() or not metadata.is_file():
        raise ValueError('Package requires a regular version.json file')
    manifest = json.loads(metadata.read_text())
    if not isinstance(manifest, dict):
        raise ValueError('Invalid package version manifest')
    revision = manifest.get('source_commit')
    if not isinstance(revision, str) or len(revision) != 40 or any(c not in '0123456789abcdef' for c in revision):
        raise ValueError('Invalid source revision')
    hashes = manifest.get('sha256')
    required=required_files(manifest)
    if not isinstance(hashes, dict) or set(hashes) != set(required):
        raise ValueError('Invalid package hash manifest')
    for name in required:
        path = package / name
        if path.is_symlink() or not path.is_file() or hashlib.sha256(path.read_bytes()).hexdigest() != hashes[name]:
            raise ValueError(f'Integrity check failed: {name}')
    return manifest


def desktop_value(value):
    return str(value).replace('\\', '\\\\').replace('\n', '\\n').replace('\r', '\\r').replace('\t', '\\t')


def desktop_entry(base, legacy=False, gui='eclipse'):
    executable = str(base / 'current' / gui)
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
    return ('[Desktop Entry]\nType=Application\nName=Eclipse\nName[zh_CN]=Eclipse\n'
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
    gui,cli=executables(manifest)
    for name in [gui,cli,*(['libpyrowave-shared.so.1'] if manifest.get('format_version',1)>=2 else []),*manifest.get('plugin_runtime',[])]:
        deps = subprocess.check_output(['ldd', str(package / name)], text=True)
        if 'not found' in deps: raise RuntimeError(f'Missing shared libraries for {name}: {deps}')
    prefix = args.prefix.expanduser().resolve()
    if any(ord(c) < 32 for c in str(prefix)):
        raise ValueError('Installation prefix must not contain control characters')
    base = prefix / 'opt' / 'uu-remote-rust'
    desktop_contents = [(False, desktop_entry(base,gui=gui)), (True, desktop_entry(base, legacy=True,gui=gui))]
    releases = base / 'releases'
    releases.mkdir(parents=True, exist_ok=True)
    dest = releases / revision[:12]
    if not dest.exists():
        staging = Path(tempfile.mkdtemp(prefix='.install-', dir=releases))
        try:
            for name in [*required_files(manifest), 'version.json']:
                shutil.copy2(package / name, staging / name)
            for name in [gui,cli]: (staging / name).chmod(0o755)
            staging.rename(dest)
        finally:
            if staging.exists(): shutil.rmtree(staging)
    else:
        # Reusing a release must not promote a damaged or misidentified install.
        # Build timestamps may differ for a legitimate rebuild of the same source.
        existing = verify_package(dest)
        if existing['source_commit'] != revision or existing['sha256'] != manifest['sha256']:
            raise ValueError('Existing release differs; refusing to overwrite a running installation')
    def link(target, path):
        path.parent.mkdir(parents=True, exist_ok=True)
        pending = path.with_name(path.name + f'.tmp-{os.getpid()}')
        try:
            pending.symlink_to(target)
            pending.replace(path)
        finally: pending.unlink(missing_ok=True)
    link(dest, base / 'current')
    for name in ['eclipse','uu-client','uu-remote-rust']:link(base/'current'/gui,prefix/'bin'/name)
    for name in ['eclipse-cli','uu-rust']:link(base/'current'/cli,prefix/'bin'/name)
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
    print(json.dumps({'source_commit': revision, 'executable': str(base / 'current' / gui), 'desktop': str(desktop)}, ensure_ascii=False))

if __name__ == '__main__':
    main()
