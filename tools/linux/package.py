#!/usr/bin/env python3
"""Build the native Linux client with VAAPI and write a revision/integrity manifest."""
from datetime import datetime, timezone
import hashlib
import os
import json
from pathlib import Path
import platform
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]
LICENSES = ['LICENSE', 'LICENSE-MIT', 'LICENSE-APACHE', 'THIRD-PARTY-NOTICES.txt']

SOURCE_PATHS = ['crates', 'vendor', 'assets', 'Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml',
                '.cargo', 'tools/linux', 'tools/licenses', 'licenses', *LICENSES]


def source_revision():
    status = subprocess.check_output(['git', 'status', '--porcelain', '--untracked-files=all', '--', *SOURCE_PATHS], cwd=ROOT, text=True)
    if status.strip(): raise RuntimeError('Commit all package sources first:\n' + status)
    return subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip()


def main():
    revision = source_revision()
    metadata = json.loads(subprocess.check_output(['cargo', 'metadata', '--no-deps', '--format-version', '1', '--locked'], cwd=ROOT, text=True))
    binaries = Path(metadata['target_directory']) / 'release'
    subprocess.run(['cargo', 'build', '--release', '-p', 'uu-gui', '-p', 'uu-cli', '--features', 'vaapi', '--locked'], cwd=ROOT, check=True)
    if source_revision() != revision: raise RuntimeError('Source revision changed during the build; package was not created')
    dest = ROOT / 'artifacts' / 'packages' / f'uu-client-linux-{platform.machine()}-{revision[:12]}'
    dest.mkdir(parents=True, exist_ok=True)
    for name in ['uu-gui', 'uu']: shutil.copy2(binaries / name, dest / name)
    shutil.copy2(ROOT / 'tools/linux/install.py', dest / 'install.py')
    shutil.copy2(ROOT / 'assets/branding/eclipse-512.png', dest / 'eclipse.png')
    for name in LICENSES: shutil.copy2(ROOT / name, dest / name)
    shutil.copy2(ROOT / 'tools/linux/README.txt', dest / 'README.txt')
    files = {name: hashlib.sha256((dest / name).read_bytes()).hexdigest() for name in ['uu-gui', 'uu', 'install.py', 'README.txt', 'eclipse.png', *LICENSES]}
    manifest = {'source_commit': revision, 'built_at_utc': datetime.now(timezone.utc).isoformat(), 'platform': f'linux-{platform.machine()}', 'features': ['vaapi', 'intel-vpl', 'opus', 'wayland-portal', 'x11', 'gamepad'], 'build_glibc': platform.libc_ver(), 'sha256': files}
    (dest / 'version.json').write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + '\n')
    archive = str(dest) + '.tar.gz'
    with tempfile.TemporaryDirectory(prefix='.linux-package-', dir=dest.parent) as temp:
        pending = shutil.make_archive(str(Path(temp) / dest.name), 'gztar', root_dir=dest.parent, base_dir=dest.name)
        if source_revision() != revision: raise RuntimeError('Source changed while packaging; archive was not published')
        os.replace(pending, archive)
    print(json.dumps({'source_commit': revision, 'directory': str(dest), 'archive': archive}))

if __name__ == '__main__':
    main()
