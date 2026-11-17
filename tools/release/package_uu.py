"""Build the pinned independent UU source against this application's SDK."""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import tomllib
import zipfile

import package_plugin
import plugin_sdk

ROOT = Path(__file__).resolve().parents[2]
LOCK = Path(__file__).with_name('uu-plugin.json')
NOTICES = ('README.md', 'LICENSE', 'LICENSE-MIT', 'LICENSE-APACHE', 'THIRD-PARTY-NOTICES.txt')


def pin():
    value = json.loads(LOCK.read_text())
    if (value.get('repository') != 'https://github.com/TheRainstorm/uu-client-plugins.git'
            or not re.fullmatch(r'[0-9a-f]{40}', value.get('revision', ''))):
        raise ValueError('Invalid pinned UU source')
    return value


def git(directory, *args):
    return subprocess.check_output(['git', *args], cwd=directory, text=True).strip()


def checkout(source, cache, seed=None):
    cache = Path(cache); cache.mkdir(parents=True, exist_ok=True)
    destination = cache / source['revision']
    if not destination.exists():
        with tempfile.TemporaryDirectory(prefix='.uu-source-', dir=cache) as temporary:
            temporary = Path(temporary)
            git(temporary, 'init', '-q')
            git(temporary, 'remote', 'add', 'origin', source['repository'])
            origin = str(Path(seed).expanduser().resolve()) if seed else 'origin'
            try:
                subprocess.run(['git', 'fetch', '--quiet', '--no-tags', '--depth=1', origin, source['revision']], cwd=temporary, check=True)
            except subprocess.CalledProcessError as error:
                raise RuntimeError('Pinned UU commit ' + source['revision'] + ' is unavailable. Publish that commit first, or set ECLIPSE_UU_PLUGIN_SOURCE for a local verification seed.') from error
            git(temporary, 'checkout', '--quiet', '--detach', 'FETCH_HEAD')
            os.rename(temporary, destination)
    if git(destination, 'rev-parse', 'HEAD') != source['revision'] or git(destination, 'status', '--porcelain', '--untracked-files=all'):
        raise RuntimeError('Cached UU source differs from its pinned commit')
    return destination


def prepare_workspace(engine, source, target):
    spec = importlib.util.spec_from_file_location('pinned_uu_sdk_workspace', source/'tools/sdk_workspace.py')
    helper = importlib.util.module_from_spec(spec); spec.loader.exec_module(helper)
    workspace, environment = helper.prepare(engine, source, target)
    # Ask Cargo in the real engine context before applying the mirror's env.
    # This respects both CARGO_TARGET_DIR and .cargo target-dir configuration.
    metadata = json.loads(subprocess.check_output(
        ['cargo','metadata','--no-deps','--format-version','1','--locked'], cwd=engine, text=True))
    environment['CARGO_TARGET_DIR'] = metadata['target_directory']
    # The independent helper can predate new engine members, such as extensions.
    for member in tomllib.loads((engine/'Cargo.toml').read_text())['workspace']['members']:
        for path in engine.glob(member):
            destination = workspace/path.relative_to(engine)
            if not destination.exists():
                shutil.copytree(path, destination, ignore=shutil.ignore_patterns('target', '__pycache__'))
    # Resolve changes only in this disposable workspace, then prove that every
    # shared SDK dependency and feature still matches the production graph.
    subprocess.run(['cargo', 'metadata', '--format-version', '1', '--filter-platform', target],
                   cwd=workspace, env=environment, stdout=subprocess.DEVNULL, check=True)
    return workspace, environment


def build(application, build_sdk, target, features, output, cache, seed=None):
    manifest = json.loads((application/'version.json').read_text())
    if (manifest.get('target') != target or not re.fullmatch(r'[0-9a-f]{64}', manifest.get('plugin_sdk_id', ''))
            or not re.fullmatch(r'[0-9a-f]{40}', manifest.get('source_commit', ''))):
        raise ValueError('Application SDK or target is missing')
    source = pin(); plugin = checkout(source, cache, seed)
    workspace, environment = prepare_workspace(ROOT, plugin, target)
    plugin_sdk.verify_consumer(ROOT, workspace, target, features)
    _, _, consumer = plugin_sdk.build(workspace, target, ['uu-gui', 'uu-cli', 'uu-compat'], features,
                                      environment, library_only=True)
    if consumer['sdk_id'] != manifest['plugin_sdk_id']:
        raise RuntimeError('UU source changes the application SDK contract; refusing a fallback')
    library = 'uu_compat.dll' if 'windows' in target else 'libuu_compat.so'
    built = Path(environment['CARGO_TARGET_DIR'])/target/'release'/library
    metadata = dict(api_version=1, sdk_id=manifest['plugin_sdk_id'], target=target)
    output.mkdir(parents=True, exist_ok=True)
    archive = output/f"eclipse-plugin-uu-{target}-{manifest['source_commit'][:12]}.zip"
    with tempfile.TemporaryDirectory(prefix='.uu-package-', dir=output) as temporary:
        temporary = Path(temporary); shipped = temporary/library
        shutil.copy2(built, shipped)
        if 'windows' in target:
            plugin_sdk.strip_windows_binary(shipped)
        runtime = [application/name for name in manifest['plugin_runtime']]
        package_plugin.probe(shipped, runtime[0], runtime[1], metadata, target, 'uu', build_sdk)
        if git(plugin, 'rev-parse', 'HEAD') != source['revision'] or git(plugin, 'status', '--porcelain', '--untracked-files=all'):
            raise RuntimeError('UU source changed while building')
        definition = dict(api_version=1, sdk_id=metadata['sdk_id'], id='uu', name='UU compatibility',
                          version=tomllib.loads((workspace/'Cargo.toml').read_text())['workspace']['package']['version']+'-source.'+source['revision'][:12],
                          library=library, library_sha256=hashlib.sha256(shipped.read_bytes()).hexdigest(),
                          data_files=['state.json', 'signing-key', 'host.json', 'client-settings.json'])
        provenance = dict(source_commit=source['revision'], source_repository=source['repository'],
                          application_source_commit=manifest['source_commit'], target=target, sdk_id=metadata['sdk_id'])
        pending = temporary/archive.name
        with zipfile.ZipFile(pending, 'w', compression=zipfile.ZIP_DEFLATED) as bundle:
            prefix = 'plugins/uu/'
            bundle.writestr(prefix+'plugin.json', json.dumps(definition, indent=2)+'\n')
            bundle.writestr(prefix+'build.json', json.dumps(provenance, indent=2)+'\n')
            bundle.write(shipped, prefix+library)
            for name in NOTICES:
                bundle.write(plugin/name, prefix+name)
            bundle.write(ROOT/'THIRD-PARTY-NOTICES.txt', prefix+'ENGINE-THIRD-PARTY-NOTICES.txt')
        os.replace(pending, archive)
    return archive


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--application', type=Path)
    parser.add_argument('--build-sdk', type=Path)
    parser.add_argument('--target')
    parser.add_argument('--features', nargs='*', default=[])
    parser.add_argument('--output-dir', type=Path)
    parser.add_argument('--source-cache', type=Path, default=ROOT/'artifacts/source-cache/uu')
    parser.add_argument('--source-seed', type=Path, default=os.environ.get('ECLIPSE_UU_PLUGIN_SOURCE'))
    parser.add_argument('--check-source', action='store_true', help='Verify the pinned source is available without compiling')
    args = parser.parse_args()
    if args.check_source:
        source = pin(); checkout(source,args.source_cache,args.source_seed)
        print(json.dumps(source)); return
    if not all((args.application,args.build_sdk,args.target,args.output_dir)):
        parser.error('--application, --build-sdk, --target and --output-dir are required for a build')
    archive = build(args.application, args.build_sdk, args.target, args.features, args.output_dir, args.source_cache, args.source_seed)
    print(json.dumps(dict(archive=str(archive))))


if __name__ == '__main__':
    main()
