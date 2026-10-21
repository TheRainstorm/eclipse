#!/usr/bin/env python3
"""Build Windows x64 with MinGW on Linux or Windows and create a verified ZIP."""
import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import zipfile

ROOT = Path(__file__).resolve().parents[2]
OUTPUT = ROOT / 'artifacts/packages'
TARGET = 'x86_64-pc-windows-gnu'
LICENSES = ('LICENSE', 'LICENSE-MIT', 'LICENSE-APACHE', 'THIRD-PARTY-NOTICES.txt')
REQUIRED = ('uu-gui.exe', 'uu.exe', 'libgcc_s_seh-1.dll', 'libstdc++-6.dll',
            'README.txt', 'install.ps1', *LICENSES)
SOURCE_PATHS = ('crates', 'vendor', 'assets', 'Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml',
                '.cargo', 'tools/windows', 'tools/licenses', 'licenses', *LICENSES)


def compiler_tool(name):
    return name if sys.platform == 'win32' else 'x86_64-w64-mingw32-' + name


def sha256(path):
    with Path(path).open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def clean_revision():
    status = subprocess.check_output(
        ['git', 'status', '--porcelain', '--untracked-files=all', '--', *SOURCE_PATHS],
        cwd=ROOT, text=True)
    if status.strip():
        raise RuntimeError('请先提交构建源码和 tools/windows 中的改动，再打包：\n' + status)
    return subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip()


def check_tools():
    for name in ('cargo', 'rustup', 'git', 'nasm', 'cmake', *(compiler_tool(tool)
                 for tool in ('gcc', 'g++', 'objdump', 'windres'))):
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
            if name.lower().startswith('lib') and not (directory / name).is_file():
                raise RuntimeError(f'{path.name} 缺少 {name}；请使用 MinGW win32 线程模型工具链，见构建文档')


def build_package(output):
    check_tools()
    revision = clean_revision()
    metadata = json.loads(subprocess.check_output(
        ['cargo', 'metadata', '--no-deps', '--format-version', '1', '--locked'],
        cwd=ROOT, text=True, encoding='utf-8'))
    binaries = Path(metadata['target_directory']) / TARGET / 'release'
    subprocess.run(['cargo', 'build', '--release', '-p', 'uu-gui', '-p', 'uu-cli',
                    '--target', TARGET, '--locked'], cwd=ROOT, check=True)
    if clean_revision() != revision:
        raise RuntimeError('构建期间提交号发生变化，请重新打包')
    output.mkdir(parents=True, exist_ok=True)
    name = f'uu-client-windows-x64-{revision[:12]}'
    archive = output / (name + '.zip')
    with tempfile.TemporaryDirectory(prefix='.windows-package-', dir=output) as temp:
        staging = Path(temp) / name
        staging.mkdir()
        for filename in ('uu-gui.exe', 'uu.exe'):
            shutil.copy2(binaries / filename, staging / filename)
        for filename, compiler in [('libgcc_s_seh-1.dll', compiler_tool('gcc')),
                                   ('libstdc++-6.dll', compiler_tool('g++'))]:
            shutil.copy2(runtime_dll(compiler, filename), staging / filename)
        for filename in ('README.txt', 'install.ps1'):
            shutil.copy2(ROOT / 'tools/windows' / filename, staging / filename)
        for filename in LICENSES:
            shutil.copy2(ROOT / filename, staging / filename)
        verify_imports(staging)
        manifest = {'source_commit': revision, 'built_at_utc': datetime.now(timezone.utc).isoformat(),
                    'target': TARGET,
                    'sha256': {filename: sha256(staging / filename) for filename in REQUIRED}}
        (staging / 'version.json').write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
        zip_path = Path(temp) / archive.name
        with zipfile.ZipFile(zip_path, 'w', compression=zipfile.ZIP_DEFLATED) as bundle:
            for path in sorted(staging.iterdir()):
                bundle.write(path, path.name)
        os.replace(zip_path, archive)
        result = {'source_commit': revision, 'archive': archive.name, 'sha256': sha256(archive)}
        pointer = Path(temp) / 'windows-latest.json'
        pointer.write_text(json.dumps(result, indent=2) + '\n', encoding='utf-8')
        os.replace(pointer, output / pointer.name)
    print(json.dumps({**result, 'archive': str(archive)}, ensure_ascii=False, indent=2))


def main():
    parser = argparse.ArgumentParser(description='在 Linux 或 Windows 使用 MinGW 编译打包 Windows x64 客户端（Python 3.11+）')
    parser.add_argument('--output-dir', type=Path, default=OUTPUT, help='产物目录（默认 artifacts/packages）')
    args = parser.parse_args()
    try:
        build_package(args.output_dir.resolve())
    except (OSError, RuntimeError, subprocess.CalledProcessError) as error:
        print(f'打包失败：{error}', file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
