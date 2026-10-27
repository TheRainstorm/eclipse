#!/usr/bin/env python3
"""Build the native Linux client with VAAPI and write a revision/integrity manifest."""
import argparse
import importlib.util
from datetime import datetime, timezone
import hashlib
import os
import json
from pathlib import Path
import platform
import shutil
import subprocess
import tempfile
import tarfile
import sys
_spec = importlib.util.spec_from_file_location('uu_linux_package_verifier', Path(__file__).with_name('install.py'))
_installer = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_installer)
PAYLOAD, verify_package = _installer.PAYLOAD, _installer.verify_package

ROOT = Path(__file__).resolve().parents[2]
_pyro_spec=importlib.util.spec_from_file_location('eclipse_pyrowave_runtime',ROOT/'tools/pyrowave/build.py')
pyrowave_runtime=importlib.util.module_from_spec(_pyro_spec)
_pyro_spec.loader.exec_module(pyrowave_runtime)
_source_spec=importlib.util.spec_from_file_location('eclipse_release_source',ROOT/'tools/release/source.py')
release_source=importlib.util.module_from_spec(_source_spec)
_source_spec.loader.exec_module(release_source)
LICENSES = ['LICENSE', 'LICENSE-MIT', 'LICENSE-APACHE', 'THIRD-PARTY-NOTICES.txt']

SOURCE_PATHS = ['crates', 'plugins', '.gitmodules', 'tools/release/source.py', 'vendor', 'assets', 'Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml',
                '.cargo', 'tools/linux','tools/pyrowave', 'tools/licenses', 'licenses', *LICENSES]
FEATURES = ['vaapi', 'intel-vpl', 'pyrowave','opus', 'wayland-portal', 'wayland-screencopy', 'x11', 'gamepad']


def reuse_archive(archive, destination, revision):
    if not archive.is_file():
        return None
    with tempfile.TemporaryDirectory(prefix='.verified-linux-cache-', dir=archive.parent) as temporary:
        staging = Path(temporary)
        with tarfile.open(archive, 'r:gz') as bundle:
            members = bundle.getmembers()
            if len({member.name for member in members}) != len(members):
                raise ValueError('缓存包含重复的归档路径')
            expected = {destination.name + '/' + name for name in [*PAYLOAD, 'version.json']}
            actual = set()
            for member in members:
                if member.name.rstrip('/') == destination.name and member.isdir():
                    continue
                limit = 1024 * 1024 if Path(member.name).name == 'version.json' else 512 * 1024 * 1024
                if member.name not in expected or not member.isfile() or member.size > limit:
                    raise ValueError('缓存包含无效的文件类型、路径或大小')
                actual.add(member.name)
                target = staging / Path(member.name).name
                with bundle.extractfile(member) as source, target.open('wb') as output:
                    shutil.copyfileobj(source, output)
                target.chmod(0o755 if target.name in ('uu', 'uu-gui') else 0o644)
            if actual != expected:
                raise ValueError('缓存包文件清单不完整')
        manifest = verify_package(staging)
        if manifest['source_commit'] != revision or manifest.get('platform') != f'linux-{platform.machine()}' or manifest.get('features') != FEATURES:
            return None
        built = manifest.get('build_glibc')
        current = platform.libc_ver()
        if not isinstance(built, list) or len(built) != 2 or built[0] != current[0]:
            return None
        try:
            if tuple(map(int, built[1].split('.'))) > tuple(map(int, current[1].split('.'))):
                return None
        except ValueError:
            if built[1] != current[1]:
                return None
        if source_revision() != revision:
            raise RuntimeError('核验缓存期间源码版本发生变化')
        if destination.is_symlink():
            raise ValueError('解包目录不能是符号链接')
        destination.mkdir(parents=True, exist_ok=True)
        for name in [*PAYLOAD, 'version.json']:
            os.replace(staging / name, destination / name)
        return {'source_commit': revision, 'directory': str(destination), 'archive': str(archive), 'cached': True}


def source_revision():
    return release_source.revision(ROOT, SOURCE_PATHS)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--rebuild', action='store_true', help='执行构建流程而非复用已核验同提交的安装包；仍使用 Cargo 缓存')
    args = parser.parse_args(argv)
    revision = source_revision()
    dest = ROOT / 'artifacts' / 'packages' / f'uu-client-linux-{platform.machine()}-{revision[:12]}'
    archive = Path(str(dest) + '.tar.gz')
    if not args.rebuild:
        try:
            cached = reuse_archive(archive, dest, revision)
            if cached:
                print(json.dumps(cached, ensure_ascii=False))
                return
        except (ValueError, OSError, tarfile.TarError) as error:
            print(f'缓存包校验未通过，将执行构建：{error}', file=sys.stderr)
    metadata = json.loads(subprocess.check_output(['cargo', 'metadata', '--no-deps', '--format-version', '1', '--locked'], cwd=ROOT, text=True))
    binaries = Path(metadata['target_directory']) / 'release'
    subprocess.run(['cargo', 'build', '--release', '-p', 'uu-gui', '-p', 'uu-cli', '--features', 'vaapi', '--locked'], cwd=ROOT, check=True)
    pyrowave=pyrowave_runtime.build(jobs=min(6,int(os.environ.get('CARGO_BUILD_JOBS','6'))),output=Path(metadata['target_directory'])/'pyrowave-runtime'/pyrowave_runtime.NATIVE)
    if source_revision() != revision: raise RuntimeError('Source revision changed during the build; package was not created')
    dest.mkdir(parents=True, exist_ok=True)
    for name in ['uu-gui', 'uu']: shutil.copy2(binaries / name, dest / name)
    shutil.copy2(pyrowave,dest/'libpyrowave-shared.so.1')
    shutil.copy2(ROOT / 'tools/linux/install.py', dest / 'install.py')
    shutil.copy2(ROOT / 'assets/branding/eclipse-512.png', dest / 'eclipse.png')
    for name in LICENSES: shutil.copy2(ROOT / name, dest / name)
    shutil.copy2(ROOT / 'tools/linux/README.txt', dest / 'README.txt')
    files = {name: hashlib.sha256((dest / name).read_bytes()).hexdigest() for name in PAYLOAD}
    manifest = {'format_version':2,'source_commit': revision, 'built_at_utc': datetime.now(timezone.utc).isoformat(), 'platform': f'linux-{platform.machine()}', 'features': FEATURES, 'build_glibc': platform.libc_ver(),'runtime_sources':{'pyrowave':pyrowave_runtime.LOCK}, 'sha256': files}
    (dest / 'version.json').write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + '\n')
    archive = str(dest) + '.tar.gz'
    with tempfile.TemporaryDirectory(prefix='.linux-package-', dir=dest.parent) as temp:
        pending = shutil.make_archive(str(Path(temp) / dest.name), 'gztar', root_dir=dest.parent, base_dir=dest.name)
        if source_revision() != revision: raise RuntimeError('Source changed while packaging; archive was not published')
        os.replace(pending, archive)
    print(json.dumps({'source_commit': revision, 'directory': str(dest), 'archive': archive}))

if __name__ == '__main__':
    main()
