#!/usr/bin/env python3
"""Package built Windows binaries with a revision and integrity manifest."""
import hashlib
import json
from datetime import datetime, timezone
from pathlib import Path
import shutil
import subprocess

ROOT = Path(__file__).resolve().parents[2]
DEST = ROOT / 'dist/uu-gui-win64'
BIN = ROOT / 'target/x86_64-pc-windows-gnu/release'


def main():
    subprocess.run(['cargo', 'build', '--release', '-p', 'uu-gui', '-p', 'uu-cli',
                    '--target', 'x86_64-pc-windows-gnu', '--locked'], cwd=ROOT, check=True)
    revision = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip()
    # Do not label a package as a commit when its source differs from that commit.
    subprocess.run(['git', 'diff', '--exit-code', 'HEAD', '--', 'crates', 'Cargo.toml', 'Cargo.lock', 'tools/windows'],
                   cwd=ROOT, check=True, stdout=subprocess.DEVNULL)
    for name in ['uu-gui.exe', 'uu.exe']:
        shutil.copy2(BIN / name, DEST / name)
    shutil.copy2(ROOT / 'tools/windows/install.ps1', DEST / 'install.ps1')
    files = {name: hashlib.sha256((DEST / name).read_bytes()).hexdigest()
             for name in ['uu-gui.exe', 'uu.exe', 'libgcc_s_seh-1.dll', 'libstdc++-6.dll', 'README.txt', 'install.ps1']}
    manifest = {'source_commit': revision, 'built_at_utc': datetime.now(timezone.utc).isoformat(),
                'install_directory': r'%USERPROFILE%\uu-gui-demo', 'sha256': files}
    (DEST / 'version.json').write_text(json.dumps(manifest, indent=2) + '\n', encoding='utf-8')
    print(json.dumps({'source_commit': revision, 'sha256': files}, indent=2))


if __name__ == '__main__':
    main()
