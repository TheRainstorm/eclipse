#!/usr/bin/env python3
"""Build Windows x64 with MinGW on Linux or Windows and create a verified ZIP."""
import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
import re
import stat
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import zipfile

import vpl_runtime
import importlib.util

ROOT = Path(__file__).resolve().parents[2]
_pyro_spec=importlib.util.spec_from_file_location('eclipse_pyrowave_runtime',ROOT/'tools/pyrowave/build.py')
pyrowave_runtime=importlib.util.module_from_spec(_pyro_spec)
_pyro_spec.loader.exec_module(pyrowave_runtime)
_source_spec=importlib.util.spec_from_file_location('eclipse_release_source',ROOT/'tools/release/source.py')
release_source=importlib.util.module_from_spec(_source_spec)
_source_spec.loader.exec_module(release_source)
_sdk_spec=importlib.util.spec_from_file_location('eclipse_plugin_sdk',ROOT/'tools/release/plugin_sdk.py')
plugin_sdk=importlib.util.module_from_spec(_sdk_spec)
_sdk_spec.loader.exec_module(plugin_sdk)
OUTPUT = ROOT / 'artifacts/packages'
TARGET = 'x86_64-pc-windows-gnu'
LICENSES = ('LICENSE', 'LICENSE-MIT', 'LICENSE-APACHE', 'THIRD-PARTY-NOTICES.txt')
LEGACY_REQUIRED = ('uu-gui.exe', 'uu.exe', 'libgcc_s_seh-1.dll', 'libstdc++-6.dll',
            'README.txt', 'install.ps1', *LICENSES)
VPL_REQUIRED = (*LEGACY_REQUIRED, 'libvpl.dll')
V3_REQUIRED = (*VPL_REQUIRED,'libpyrowave-shared-1.dll')
REQUIRED = ('eclipse.exe', 'eclipse-cli.exe', *V3_REQUIRED[2:])

def required_files(manifest):
    version = manifest.get('format_version', 1)
    if type(version) is not int or version not in (1, 2, 3, 4, 5):
        raise ValueError('不支持的 Windows 包格式版本')
    if version >= 4:
        runtime = manifest.get('plugin_runtime')
        if (not isinstance(runtime, list) or len(runtime) != 2
                or runtime[0] != 'uu_plugin_sdk.dll'
                or not isinstance(runtime[1], str)
                or not re.fullmatch(r'std-[0-9a-f]+\.dll', runtime[1])
                or not re.fullmatch(r'[0-9a-f]{64}', manifest.get('plugin_sdk_id', ''))):
            raise ValueError('无效的插件 SDK 运行库清单')
        return (*(REQUIRED if version == 5 else V3_REQUIRED), *runtime)
    return V3_REQUIRED if version == 3 else VPL_REQUIRED if version==2 else LEGACY_REQUIRED
SOURCE_PATHS = ('crates', 'tools/release/source.py', 'tools/release/plugin_sdk.py', 'vendor', 'assets', 'Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml',
                '.cargo', 'tools/windows', 'tools/pyrowave','tools/licenses', 'licenses', *LICENSES)



def verify_package(path):
    with zipfile.ZipFile(path) as bundle:
        names = bundle.namelist()
        if names.count('version.json') != 1 or bundle.getinfo('version.json').file_size > 1024 * 1024:
            raise ValueError('ZIP 缺少唯一、有界的 version.json')
        manifest = json.loads(bundle.read('version.json'))
        if not isinstance(manifest, dict):
            raise ValueError('version.json 格式无效')
        required = required_files(manifest)
        # A legacy bundle remains installable, while new bundles must include
        # the dispatcher. Neither format allows arbitrary extra executables.
        if len(names) != len(required) + 1 or set(names) != set(required) | {'version.json'}:
            raise ValueError('ZIP 文件清单不正确，请使用 tools/windows/package.py 生成的包')
        revision = manifest.get('source_commit')
        if not isinstance(revision, str) or not re.fullmatch(r'[0-9a-f]{40}', revision):
            raise ValueError('version.json 缺少有效提交号')
        hashes = manifest.get('sha256')
        if not isinstance(hashes, dict) or set(hashes) != set(required):
            raise ValueError('version.json 校验清单不完整')
        for name in required:
            info = bundle.getinfo(name)
            if info.file_size > 512 * 1024 * 1024 or stat.S_IFMT(info.external_attr >> 16) == stat.S_IFLNK:
                raise ValueError(f'ZIP 文件类型或大小无效：{name}')
            with bundle.open(name) as stream:
                actual = hashlib.file_digest(stream, 'sha256').hexdigest()
            if actual != manifest['sha256'][name]:
                raise ValueError(f'文件校验失败：{name}')
    return manifest


def compiler_tool(name):
    return name if sys.platform == 'win32' else 'x86_64-w64-mingw32-' + name


def sha256(path):
    with Path(path).open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def clean_revision():
    return release_source.revision(ROOT, SOURCE_PATHS)


def check_tools():
    for name in ('cargo', 'rustup', 'git', 'nasm', 'cmake', *(compiler_tool(tool)
                 for tool in ('gcc', 'g++', 'objdump', 'windres', 'strip'))):
        if not shutil.which(name):
            raise RuntimeError(f'缺少 {name}；依赖安装方法见 docs/windows-build.md')
    targets = subprocess.check_output(['rustup', 'target', 'list', '--installed'], text=True)
    if TARGET not in targets.splitlines():
        raise RuntimeError(f'请先执行 rustup target add {TARGET}')


def runtime_dll(compiler, name):
    value = subprocess.check_output([compiler, '-print-file-name=' + name], text=True).strip()
    path = Path(value)
    if not path.is_file():
        raise RuntimeError(f'{compiler} 找不到运行库 {name}')
    return path


def verify_imports(directory):
    # MinGW POSIX variants can also require libwinpthread. Never ship an incomplete bundle.
    for path in directory.iterdir():
        if path.suffix.lower() not in ('.exe', '.dll'):
            continue
        imports = subprocess.check_output([compiler_tool('objdump'), '-p', str(path)], text=True)
        for line in imports.splitlines():
            if 'DLL Name:' not in line:
                continue
            name = line.split('DLL Name:', 1)[1].strip()
            if name.lower().startswith(('lib', 'std-', 'uu_plugin_sdk')) and not (directory / name).is_file():
                raise RuntimeError(f'{path.name} 缺少 {name}；请使用 MinGW win32 线程模型工具链，见构建文档')


def build_package(output, vpl_archive=None, rebuild=False):
    revision = clean_revision()
    archive = output / f'uu-client-windows-x64-{revision[:12]}.zip'
    if not rebuild and vpl_archive is None and archive.is_file():
        try:
            manifest = verify_package(archive)
            if manifest['source_commit'] == revision and manifest.get('target') == TARGET and manifest.get('format_version') == 5:
                if clean_revision() != revision:
                    raise RuntimeError('核验缓存期间源码版本发生变化')
                result = {'source_commit': revision, 'archive': archive.name, 'sha256': sha256(archive)}
                with tempfile.TemporaryDirectory(prefix='.cached-package-', dir=output) as temp:
                    pointer = Path(temp) / 'windows-latest.json'
                    pointer.write_text(json.dumps(result, indent=2) + '\n', encoding='utf-8')
                    os.replace(pointer, output / pointer.name)
                print(json.dumps({**result, 'archive': str(archive), 'cached': True}, ensure_ascii=False, indent=2))
                return result
        except (ValueError, OSError, zipfile.BadZipFile) as error:
            print(f'缓存包校验未通过，将执行构建：{error}', file=sys.stderr)
    check_tools()
    metadata = json.loads(subprocess.check_output(
        ['cargo', 'metadata', '--no-deps', '--format-version', '1', '--locked'],
        cwd=ROOT, text=True, encoding='utf-8'))
    binaries = Path(metadata['target_directory']) / TARGET / 'release'
    sdk, standard, sdk_metadata = plugin_sdk.build(ROOT, TARGET, ['uu-gui', 'uu-cli'])
    dispatcher = vpl_runtime.build_dispatcher(Path(metadata['target_directory']) / 'vpl-runtime', vpl_archive)
    pyrowave = pyrowave_runtime.build(jobs=min(6,int(os.environ.get('CARGO_BUILD_JOBS','6'))),
        output=Path(metadata['target_directory'])/'pyrowave-runtime'/TARGET,target=TARGET)
    if clean_revision() != revision:
        raise RuntimeError('构建期间提交号发生变化，请重新打包')
    output.mkdir(parents=True, exist_ok=True)
    name = f'uu-client-windows-x64-{revision[:12]}'
    archive = output / (name + '.zip')
    with tempfile.TemporaryDirectory(prefix='.windows-package-', dir=output) as temp:
        staging = Path(temp) / name
        staging.mkdir()
        for filename in ('eclipse.exe', 'eclipse-cli.exe'):
            shutil.copy2(binaries / filename, staging / filename)
        for runtime in (sdk, standard):
            shutil.copy2(runtime, staging / runtime.name)
        for filename, compiler in [('libgcc_s_seh-1.dll', compiler_tool('gcc')),
                                   ('libstdc++-6.dll', compiler_tool('g++'))]:
            shutil.copy2(runtime_dll(compiler, filename), staging / filename)
        shutil.copy2(dispatcher, staging / 'libvpl.dll')
        shutil.copy2(pyrowave,staging/'libpyrowave-shared-1.dll')
        for filename in ('README.txt', 'install.ps1'):
            shutil.copy2(ROOT / 'tools/windows' / filename, staging / filename)
        for filename in LICENSES:
            shutil.copy2(ROOT / filename, staging / filename)
        for binary in staging.iterdir():
            if binary.suffix.lower() in ('.exe', '.dll'): plugin_sdk.strip_windows_binary(binary)
        verify_imports(staging)
        manifest = {'format_version': 5, 'source_commit': revision, 'built_at_utc': datetime.now(timezone.utc).isoformat(),
                    'target': TARGET,
                    'plugin_sdk_id': sdk_metadata['sdk_id'], 'plugin_runtime': [sdk.name, standard.name],
                    'runtime_sources': {'intel_vpl': vpl_runtime.COMMIT,'pyrowave':pyrowave_runtime.LOCK},
                    'sha256': {filename: sha256(staging / filename) for filename in (*REQUIRED, sdk.name, standard.name)}}
        (staging / 'version.json').write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
        zip_path = Path(temp) / archive.name
        with zipfile.ZipFile(zip_path, 'w', compression=zipfile.ZIP_DEFLATED) as bundle:
            for path in sorted(staging.iterdir()):
                bundle.write(path, path.name)
        if clean_revision() != revision:
            raise RuntimeError('打包期间源码或提交发生变化，未发布产物')
        os.replace(zip_path, archive)
        result = {'source_commit': revision, 'archive': archive.name, 'sha256': sha256(archive)}
        pointer = Path(temp) / 'windows-latest.json'
        pointer.write_text(json.dumps(result, indent=2) + '\n', encoding='utf-8')
        os.replace(pointer, output / pointer.name)
    print(json.dumps({**result, 'archive': str(archive)}, ensure_ascii=False, indent=2))


def main():
    parser = argparse.ArgumentParser(description='在 Linux 或 Windows 使用 MinGW 编译打包 Windows x64 客户端（Python 3.11+）')
    parser.add_argument('--output-dir', type=Path, default=OUTPUT, help='产物目录（默认 artifacts/packages）')
    parser.add_argument('--rebuild', action='store_true', help='执行构建流程而非复用已核验同提交的安装包；仍使用 Cargo 缓存')
    parser.add_argument('--vpl-archive', type=Path, help='离线 Intel VPL 固定版本源码归档；不提供时自动下载并验证')
    args = parser.parse_args()
    try:
        build_package(args.output_dir.resolve(), args.vpl_archive, args.rebuild)
    except (OSError, RuntimeError, subprocess.CalledProcessError) as error:
        print(f'打包失败：{error}', file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
