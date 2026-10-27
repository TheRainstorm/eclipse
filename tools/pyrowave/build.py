#!/usr/bin/env python3
"""Build the pinned MIT PyroWave C runtime, without optional Granite dependencies."""
import argparse
import hashlib
import json
import os
import platform
from pathlib import Path
import shutil
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
LOCK = json.loads(Path(__file__).with_name('sources.json').read_text())
WINDOWS = 'x86_64-pc-windows-gnu'
NATIVE = WINDOWS if os.name == 'nt' else platform.machine() + '-unknown-linux-gnu'
def library_name(target):
    return 'libpyrowave-shared-1.dll' if target == WINDOWS else 'libpyrowave-shared.so.1'


def run(args, cwd=None):
    subprocess.run([str(v) for v in args], cwd=cwd, check=True, timeout=600)


def revision(path):
    return subprocess.check_output(['git', '-C', str(path), 'rev-parse', 'HEAD'], text=True).strip()


def checkout(path, source):
    if not path.exists():
        run(['git', 'clone', '--filter=blob:none', '--no-checkout', source['url'], path])
    if not (path / '.git').exists():
        raise ValueError(f'Not a git checkout: {path}')
    changed=subprocess.check_output(['git','-C',str(path),'diff','HEAD','--name-only'],text=True).strip()
    if changed:
        # A --no-checkout clone has an empty index. Only allow this initial
        # state, never discard staged or unstaged changes in an existing tree.
        indexed = subprocess.check_output(['git', '-C', str(path), 'ls-files'], text=True)
        if indexed.strip():raise ValueError(f'Refusing to change a dirty source checkout: {path}')
    try:
        run(['git', '-C', path, 'cat-file', '-e', source['commit'] + '^{commit}'])
    except subprocess.CalledProcessError:
        run(['git', '-C', path, 'fetch', 'origin', source['commit']])
    if revision(path) != source['commit'] or not (path / 'CMakeLists.txt').exists():
        run(['git', '-C', path, 'checkout', '--detach', source['commit']])


def build(jobs=6, source=None, output=None, target=None):
    # A package never silently omits this runtime. The validated cache is keyed
    # by the complete dependency lock, compiler and platform rather than HEAD.
    target = target or NATIVE
    if target not in (NATIVE, WINDOWS):
        raise ValueError('Only the native platform or Windows GNU cross target is supported')
    source = Path(source) if source else ROOT / 'artifacts' / 'pyrowave-source'
    output = Path(output) if output else ROOT / 'target' / 'pyrowave-runtime' / target
    output.mkdir(parents=True, exist_ok=True)
    cross = target == WINDOWS and os.name != 'nt'
    compiler = 'x86_64-w64-mingw32-g++' if cross else os.environ.get('CXX', 'c++')
    toolchain = subprocess.check_output([compiler, '--version'], text=True).splitlines()[0]
    fingerprint = {'lock': LOCK, 'compiler': toolchain, 'platform': sys.platform,
                   'machine': platform.machine(), 'target':target,'fp32_math': True}
    if cross:
        fingerprint['system_processor']='x86_64'
        fingerprint['granite_swapchain']=False
    stamp = output / 'runtime.json'
    library = output / library_name(target)
    if stamp.is_file() and library.is_file():
        cached = json.loads(stamp.read_text())
        if cached.get('inputs') == fingerprint and cached.get('sha256') == hashlib.sha256(library.read_bytes()).hexdigest():
            return library
    checkout(source, LOCK['sources']['pyrowave'])
    granite = source / 'Granite'
    checkout(granite, LOCK['sources']['Granite'])
    run(['git', '-C', granite, 'submodule', 'update', '--init', *LOCK['submodules']])
    for name, expected in LOCK['submodules'].items():
        if revision(granite / name) != expected or subprocess.check_output(['git','-C',str(granite/name),'diff','HEAD','--name-only'],text=True).strip():
            raise ValueError(f'Unexpected pinned dependency revision: {name}')
    build_dir = output / 'build'
    configure = ['cmake', '-S', source, '-B', build_dir,
                 '-DCMAKE_BUILD_TYPE=Release', '-DPYROWAVE_UTILS=OFF', '-DPYROWAVE_DEVEL=OFF',
                 '-DPYROWAVE_FP32_MATH=ON', '-DPYROWAVE_FP32_STORAGE=OFF']
    if cross:
        # The runtime uses compute and external images, never Granite's D3D12
        # presenter. MinGW cross SDKs may not declare ID3D12Device4; the Eclipse
        # D3D11/fence imports remain enabled independently of this WSI option.
        configure += ['-DCMAKE_SYSTEM_NAME=Windows','-DCMAKE_SYSTEM_PROCESSOR=x86_64',
                      '-DGRANITE_VULKAN_DXGI_INTEROP=OFF','-DCMAKE_C_COMPILER=x86_64-w64-mingw32-gcc',
                      '-DCMAKE_CXX_COMPILER=' + compiler]
    elif os.name == 'nt':
        configure += ['-G', 'MinGW Makefiles']
    run(configure)
    run(['cmake', '--build', build_dir, '--target', 'pyrowave-shared', 'pyrowave-device-validation', '-j', jobs])
    candidates = [v for v in build_dir.rglob(library.name) if v.is_file()]
    if len(candidates) != 1:
        raise ValueError(f'Expected one {library.name}, found {len(candidates)}')
    shutil.copyfile(candidates[0], library)
    stamp.write_text(json.dumps({'inputs': fingerprint, 'sha256': hashlib.sha256(library.read_bytes()).hexdigest()}, indent=2) + '\n')
    return library


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--jobs', type=int, default=6)
    parser.add_argument('--source', type=Path)
    parser.add_argument('--output', type=Path)
    parser.add_argument('--target',default=NATIVE)
    args = parser.parse_args()
    if not 1 <= args.jobs <= 64:
        parser.error('--jobs must be between 1 and 64')
    print(json.dumps({'library': str(build(args.jobs, args.source, args.output,args.target))}))


if __name__ == '__main__':
    main()
