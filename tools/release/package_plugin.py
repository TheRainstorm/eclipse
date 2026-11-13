#!/usr/bin/env python3
"""Build an optional first-party plugin against the application's exact SDK."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import zipfile

import plugin_sdk

ROOT = Path(__file__).resolve().parents[2]
INPUTS = ['crates', 'extensions', 'vendor', '.cargo', 'Cargo.toml', 'Cargo.lock',
          'rust-toolchain.toml', 'tools/release', 'tools/native/plugin_probe.rs']


def revision():
    dirty = subprocess.check_output(['git', 'status', '--porcelain', '--untracked-files=all', '--', *INPUTS], cwd=ROOT, text=True)
    if dirty.strip():
        raise RuntimeError('commit plugin and SDK build inputs before packaging')
    return subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip()


def probe(library, sdk, standard, metadata, target, plugin_id, build_sdk=None):
    if 'windows' in target:
        count = plugin_sdk.verify_windows_imports(library, sdk)
        print(f'Windows application SDK imports verified: {count}', flush=True)
        return
    with tempfile.TemporaryDirectory(prefix='eclipse-plugin-probe-') as temporary:
        temporary = Path(temporary)
        directory = temporary / 'plugins' / plugin_id
        directory.mkdir(parents=True)
        shutil.copy2(library, directory / library.name)
        manifest = dict(api_version=metadata['api_version'], sdk_id=metadata['sdk_id'],
                        id=plugin_id, name=plugin_id, version='probe', library=library.name,
                        library_sha256=hashlib.sha256(library.read_bytes()).hexdigest(), data_files=[])
        (directory / 'plugin.json').write_text(json.dumps(manifest))
        # Cargo's metadata remains in the build tree. Compile against that
        # graph, then load the application's actual SDK at runtime.
        build_sdk = build_sdk or sdk
        dependencies = build_sdk.parent if build_sdk.parent.name == 'deps' else build_sdk.parent / 'deps'
        release = dependencies.parent
        host_dependencies = release.parent.parent / 'release/deps' if release.parent.name == target else dependencies
        executable = temporary / 'probe'
        subprocess.run(['rustc', '--edition=2021', '-C', 'prefer-dynamic', '--target', target,
                        '-L', 'dependency=' + str(dependencies), '-L', 'dependency=' + str(host_dependencies),
                        '--extern', 'uu_plugin_sdk=' + str(build_sdk), str(ROOT / 'tools/native/plugin_probe.rs'),
                        '-o', str(executable)], check=True, cwd=ROOT)
        environment = os.environ.copy()
        environment['LD_LIBRARY_PATH'] = str(sdk.parent) + ':' + str(standard.parent)
        subprocess.run([str(executable), str(temporary / 'plugins'), str(temporary / 'profile'), plugin_id],
                       env=environment, check=True, cwd=ROOT)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--manifest', type=Path, required=True)
    parser.add_argument('--target', required=True)
    parser.add_argument('--features', nargs='*', default=[])
    parser.add_argument('--application', type=Path, help='check against an already staged application directory')
    parser.add_argument('--output-dir', type=Path, default=ROOT / 'artifacts/plugins')
    parser.add_argument('--validate-only', action='store_true')
    args = parser.parse_args()
    source = revision()
    definition = json.loads(args.manifest.read_text())
    sdk, standard, metadata = plugin_sdk.build(ROOT, args.target, ['uu-gui', 'uu-cli'], args.features)
    application_source = None
    application_sdk, application_standard = sdk, standard
    if args.application:
        installed = json.loads((args.application / 'version.json').read_text())
        if installed.get('plugin_sdk_id') != metadata['sdk_id']:
            raise RuntimeError('application package does not match this plugin SDK')
        application_source = installed.get('source_commit')
        application_sdk, application_standard = args.application / sdk.name, args.application / standard.name
    _, _, extension_metadata = plugin_sdk.build(ROOT, args.target, ['uu-gui', 'uu-cli', definition['package']], args.features)
    if extension_metadata['sdk_id'] != metadata['sdk_id']:
        raise RuntimeError('plugin dependencies change the application SDK contract')
    workspace = json.loads(subprocess.check_output(['cargo', 'metadata', '--locked', '--format-version', '1', '--no-deps'], cwd=ROOT, text=True))
    package = next(p for p in workspace['packages'] if p['name'] == definition['package'])
    filename = definition['library'] + '.dll' if 'windows' in args.target else 'lib' + definition['library'] + '.so'
    original = Path(workspace['target_directory']) / args.target / 'release' / filename
    with tempfile.TemporaryDirectory(prefix='eclipse-plugin-package-') as temporary:
        library = Path(temporary) / filename
        shutil.copy2(original, library)
        if 'windows' in args.target:
            plugin_sdk.strip_windows_binary(library)
        probe(library, application_sdk, application_standard, metadata, args.target, definition['id'], sdk)
        if revision() != source:
            raise RuntimeError('source changed during plugin build')
        if args.validate_only:
            return
        manifest = dict(api_version=metadata['api_version'], sdk_id=metadata['sdk_id'],
                        id=definition['id'], name=definition['name'],
                        version=package['version'] + '-dev.' + source[:12], library=filename,
                        library_sha256=hashlib.sha256(library.read_bytes()).hexdigest(),
                        data_files=definition.get('data_files', []))
        args.output_dir.mkdir(parents=True, exist_ok=True)
        name = f"eclipse-plugin-{definition['id']}-{args.target}-{source[:12]}.zip"
        archive = args.output_dir / name
        pending = args.output_dir / (name + '.tmp')
        prefix = 'plugins/' + definition['id'] + '/'
        with zipfile.ZipFile(pending, 'w', compression=zipfile.ZIP_DEFLATED) as bundle:
            bundle.writestr(prefix + 'plugin.json', json.dumps(manifest, ensure_ascii=False, indent=2) + '\n')
            bundle.writestr(prefix + 'build.json', json.dumps(dict(source_commit=source, application_source_commit=application_source, target=args.target, sdk_id=metadata['sdk_id'])) + '\n')
            bundle.write(library, prefix + filename)
            for file in [args.manifest.parent / 'README.md', ROOT / 'LICENSE-MIT', ROOT / 'LICENSE-APACHE', ROOT / 'THIRD-PARTY-NOTICES.txt']:
                bundle.write(file, prefix + file.name)
        os.replace(pending, archive)
        print(json.dumps(dict(archive=str(archive), source_commit=source, sdk_id=metadata['sdk_id'], sha256=hashlib.sha256(archive.read_bytes()).hexdigest()), ensure_ascii=False), flush=True)


if __name__ == '__main__':
    main()
