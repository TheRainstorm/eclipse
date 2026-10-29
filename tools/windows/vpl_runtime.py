#!/usr/bin/env python3
"""Build the pinned MIT Intel VPL dispatcher, without installing GPU drivers."""
import argparse
import hashlib
import os
from pathlib import Path, PurePosixPath
import shutil
import subprocess
import sys
import tarfile
import tempfile
import urllib.request

COMMIT = '674d015bcb294bc39fa276e99a652ea045423e82'
ARCHIVE_SHA256 = '9d5d3c221b9d41468e04add6855cf5eec6df06dae7be649f84e1f846c98e38fb'
URL = f'https://codeload.github.com/intel/libvpl/tar.gz/{COMMIT}'
MAX_ARCHIVE = 32 * 1024 * 1024
MAX_SOURCE = 128 * 1024 * 1024


def sha256(path):
    with Path(path).open('rb') as source:
        return hashlib.file_digest(source, 'sha256').hexdigest()


def verified_archive(cache, supplied=None):
    cache.mkdir(parents=True, exist_ok=True)
    if supplied is not None:
        supplied = Path(supplied)
        if not supplied.is_file() or sha256(supplied) != ARCHIVE_SHA256:
            raise RuntimeError('Intel VPL 源码归档校验失败，未使用该文件')
        return supplied
    archive = cache / f'{COMMIT}.tar.gz'
    if archive.exists():
        if sha256(archive) != ARCHIVE_SHA256:
            raise RuntimeError('Intel VPL 缓存归档校验失败，请移开该文件后重试')
        return archive
    descriptor, pending = tempfile.mkstemp(prefix='.vpl-download-', dir=cache)
    try:
        with os.fdopen(descriptor, 'wb') as output, urllib.request.urlopen(URL, timeout=45) as response:
            total = 0
            while data := response.read(1024 * 1024):
                total += len(data)
                if total > MAX_ARCHIVE:
                    raise RuntimeError('Intel VPL 源码归档超过大小限制')
                output.write(data)
        if sha256(pending) != ARCHIVE_SHA256:
            raise RuntimeError('Intel VPL 下载校验失败，未解压或执行')
        os.replace(pending, archive)
    finally:
        Path(pending).unlink(missing_ok=True)
    return archive


def source_entries(bundle):
    entries = []
    total = 0
    seen = set()
    for index, member in enumerate(bundle):
        total += member.size
        if index >= 20000 or total > MAX_SOURCE:
            raise RuntimeError('Intel VPL 源码归档超过解压限制')
        path = PurePosixPath(member.name)
        if (not path.parts or path.is_absolute() or '..' in path.parts
                or '\\' in member.name or ':' in member.name
                or path.parts[0] != 'libvpl-' + COMMIT
                or not (member.isdir() or member.isfile())):
            raise RuntimeError('Intel VPL 源码归档包含无效路径或链接')
        if path in seen:
            raise RuntimeError('Intel VPL 源码归档包含重复路径')
        seen.add(path)
        if len(path.parts) > 1:
            entries.append((member, Path(*path.parts[1:])))
    return entries


def verified_source(cache, archive):
    source = cache / ('source-' + COMMIT[:12])
    if source.is_symlink():
        raise RuntimeError('Intel VPL 源码缓存不能是链接')
    with tarfile.open(archive, 'r:gz') as bundle:
        entries = source_entries(bundle)
        if source.exists():
            expected_files = {relative for member, relative in entries if member.isfile()}
            actual_files = {path.relative_to(source) for path in source.rglob('*') if path.is_file() or path.is_symlink()}
            if actual_files != expected_files:
                raise RuntimeError('Intel VPL 缓存源码清单发生变化，请移开该缓存目录后重试')
            for member, relative in entries:
                path = source / relative
                if not path.resolve().is_relative_to(source.resolve()) or path.is_symlink():
                    raise RuntimeError('Intel VPL 源码缓存路径无效')
                if member.isfile():
                    expected = hashlib.file_digest(bundle.extractfile(member), 'sha256').hexdigest()
                    if not path.is_file() or sha256(path) != expected:
                        raise RuntimeError('Intel VPL 缓存源码发生变化，请移开该缓存目录后重试')
            return source
        with tempfile.TemporaryDirectory(prefix='.vpl-source-', dir=cache) as temp:
            staging = Path(temp) / 'source'
            staging.mkdir()
            for member, relative in entries:
                path = staging / relative
                if member.isdir():
                    path.mkdir(parents=True, exist_ok=True)
                else:
                    path.parent.mkdir(parents=True, exist_ok=True)
                    with bundle.extractfile(member) as data, path.open('xb') as output:
                        shutil.copyfileobj(data, output)
                    path.chmod(0o755 if member.mode & 0o111 else 0o644)
            staging.rename(source)
    return source


def compiler_tool(name):
    return name if sys.platform == 'win32' else 'x86_64-w64-mingw32-' + name


def build_dispatcher(cache, supplied=None, jobs=None):
    cache = Path(cache).resolve()
    archive = verified_archive(cache, supplied)
    source = verified_source(cache, archive)
    version = subprocess.check_output([compiler_tool('g++'), '-dumpfullversion', '-dumpversion'], text=True).strip()
    if not version or any(not(c.isascii() and (c.isalnum() or c in '.-_')) for c in version):
        raise RuntimeError('无法识别 MinGW 编译器版本')
    build = cache / ('build-' + COMMIT[:12] + '-' + version)
    generator = 'Ninja' if shutil.which('ninja') else ('MinGW Makefiles' if sys.platform == 'win32' else 'Unix Makefiles')
    command = ['cmake', '-S', str(source), '-B', str(build), '-G', generator,
               '-DCMAKE_BUILD_TYPE=Release', '-DBUILD_SHARED_LIBS=ON', '-DBUILD_TESTS=OFF',
               '-DBUILD_EXAMPLES=OFF', '-DBUILD_EXPERIMENTAL=OFF', '-DINSTALL_EXAMPLES=OFF',
               '-DCMAKE_C_COMPILER=' + compiler_tool('gcc'),
               '-DCMAKE_CXX_COMPILER=' + compiler_tool('g++'),
               '-DCMAKE_RC_COMPILER=' + compiler_tool('windres')]
    # CMake caches contain absolute source/build paths. A restored CI cache can
    # move with a repository rename; regenerate only that derived build tree.
    cmake_cache = build / 'CMakeCache.txt'
    if cmake_cache.is_file():
        values = dict(line.split('=', 1) for line in cmake_cache.read_text(errors='replace').splitlines()
                      if line.startswith(('CMAKE_HOME_DIRECTORY:INTERNAL=', 'CMAKE_CACHEFILE_DIR:INTERNAL=')))
        expected = {'CMAKE_HOME_DIRECTORY:INTERNAL': source, 'CMAKE_CACHEFILE_DIR:INTERNAL': build}
        if any(key not in values or Path(values[key]).resolve() != path.resolve() for key, path in expected.items()):
            command.append('--fresh')
    if sys.platform != 'win32':
        command.append('-DCMAKE_SYSTEM_NAME=Windows')
    subprocess.run(command, check=True)
    subprocess.run(['cmake', '--build', str(build), '--target', 'VPL', '--parallel',
                    str(jobs or min(8, os.cpu_count() or 2))], check=True)
    library = build / 'libvpl.dll'
    if not library.is_file():
        raise RuntimeError('Intel VPL 构建没有生成 libvpl.dll')
    exports = subprocess.check_output([compiler_tool('objdump'), '-p', str(library)], text=True)
    for symbol in ('MFXLoad', 'MFXCreateSession', 'MFXCreateConfig', 'MFXVideoENCODE_EncodeFrameAsync', 'MFXVideoDECODE_DecodeFrameAsync'):
        if not any(line.split()[-1:] == [symbol] for line in exports.splitlines()):
            raise RuntimeError('Intel VPL dispatcher 缺少必需导出：' + symbol)
    return library


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--cache-dir', type=Path, default=Path('target/vpl-runtime'))
    parser.add_argument('--archive', type=Path, help='Offline source archive with the pinned SHA-256')
    parser.add_argument('--jobs', type=int)
    args = parser.parse_args()
    if args.jobs is not None and not 1 <= args.jobs <= 64:
        parser.error('--jobs must be in 1..64')
    print(build_dispatcher(args.cache_dir, args.archive, args.jobs))


if __name__ == '__main__':
    main()
