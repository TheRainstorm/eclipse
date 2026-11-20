"""Bundle first-party plugins using the application's existing SDK contract."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import os
import subprocess
import sys
import zipfile

ROOT = Path(__file__).resolve().parents[2]
NOTICES = ('README.md', 'LICENSE-MIT', 'LICENSE-APACHE', 'THIRD-PARTY-NOTICES.txt')
EXTRA_NOTICES = {'LICENSE', 'ENGINE-THIRD-PARTY-NOTICES.txt'}


def source_revision(manifest, identifier):
    sources = manifest.get('bundled_plugin_sources', {})
    if not isinstance(sources, dict) or not set(sources).issubset(manifest.get('bundled_plugins', {})):
        raise ValueError('Invalid bundled plugin sources')
    source = sources.get(identifier)
    if source is None:
        return manifest['source_commit']
    if (not isinstance(source, dict) or not re.fullmatch(r'[0-9a-f]{40}', source.get('revision', ''))
            or source.get('repository') != 'https://github.com/TheRainstorm/uu-client-plugins.git'):
        raise ValueError('Invalid bundled plugin source revision')
    return source['revision']


def files(manifest):
    plugins = manifest.get('bundled_plugins', {})
    if not isinstance(plugins, dict) or len(plugins) > 16:
        raise ValueError('Invalid bundled plugin inventory')
    notices = manifest.get('bundled_plugin_notices', {})
    if not isinstance(notices, dict) or not set(notices).issubset(plugins):
        raise ValueError('Invalid bundled plugin notices')
    result = []
    suffix = '.dll' if manifest.get('target', '').endswith('windows-gnu') else '.so'
    for identifier, library in plugins.items():
        if (not isinstance(identifier, str) or not re.fullmatch(r'[a-z0-9][a-z0-9-]{0,63}', identifier)
                or not isinstance(library, str)
                or not re.fullmatch(r'[A-Za-z0-9_-]+' + re.escape(suffix), library)):
            raise ValueError('Invalid bundled plugin path')
        extra = notices.get(identifier, [])
        if not isinstance(extra, list) or any(not isinstance(name, str) for name in extra) or len(extra) != len(set(extra)) or not set(extra).issubset(EXTRA_NOTICES):
            raise ValueError('Invalid bundled plugin notice path')
        source_revision(manifest, identifier)
        result.extend('plugins/' + identifier + '/' + name
                      for name in ('plugin.json', 'build.json', library, *NOTICES, *extra))
    return result


def verify(manifest, read):
    for identifier, library in manifest.get('bundled_plugins', {}).items():
        prefix = 'plugins/' + identifier + '/'
        plugin = json.loads(read(prefix + 'plugin.json'))
        build = json.loads(read(prefix + 'build.json'))
        if (plugin.get('id') != identifier or plugin.get('api_version') != 1
                or plugin.get('sdk_id') != manifest.get('plugin_sdk_id')
                or plugin.get('library') != library
                or plugin.get('library_sha256') != manifest['sha256'][prefix + library]
                or build.get('sdk_id') != manifest.get('plugin_sdk_id')
                or build.get('source_commit') != source_revision(manifest, identifier)
                or (identifier in manifest.get('bundled_plugin_sources', {})
                    and build.get('source_repository') != manifest['bundled_plugin_sources'][identifier]['repository'])
                or build.get('application_source_commit') != manifest['source_commit']
                or build.get('target') != manifest.get('target')):
            raise ValueError('Bundled plugin source or SDK differs from application')


def stage(application, manifest, target, features, output, build_sdk, *, environment=None):
    """Reuse package_plugin's SDK/import probe and retain its standalone asset."""
    application, output = Path(application), Path(output)
    definition = json.loads((ROOT / 'extensions/wallpaper-collects/plugin.json').read_text())
    subprocess.run([sys.executable, str(ROOT / 'tools/release/package_plugin.py'),
                    '--manifest', str(ROOT / 'extensions/wallpaper-collects/plugin.json'),
                    '--target', target, '--application', str(application),
                    '--output-dir', str(output), '--features', *features], check=True, cwd=ROOT, stdout=sys.stderr,
                   env=environment)
    source = manifest['source_commit']
    archive = output / f"eclipse-plugin-{definition['id']}-{target}-{source[:12]}.zip"
    library = definition['library'] + '.dll' if 'windows' in target else 'lib' + definition['library'] + '.so'
    manifest['bundled_plugins'] = {definition['id']: library}
    manifest.pop('bundled_plugin_sources', None)
    manifest.pop('bundled_plugin_notices', None)
    manifest['target'] = target
    include(application, manifest, archive, definition['id'])
    source = json.loads((ROOT/'tools/release/uu-plugin.json').read_text())
    subprocess.run([sys.executable, str(ROOT/'tools/release/package_uu.py'),
                    '--application', str(application), '--build-sdk', str(build_sdk),
                    '--target', target, '--output-dir', str(output), '--features', *features],
                   check=True, cwd=ROOT, stdout=sys.stderr, env=environment)
    manifest['bundled_plugins']['uu'] = 'uu_compat.dll' if 'windows' in target else 'libuu_compat.so'
    manifest['bundled_plugin_sources'] = {'uu': source}
    manifest['bundled_plugin_notices'] = {'uu': ['LICENSE', 'ENGINE-THIRD-PARTY-NOTICES.txt']}
    uu = output/f"eclipse-plugin-uu-{target}-{manifest['source_commit'][:12]}.zip"
    include(application, manifest, uu, 'uu')
    verify(manifest, lambda name: (application/name).read_bytes())
    return archive


def include(application, manifest, archive, identifier):
    expected = [name for name in files(manifest) if name.startswith('plugins/'+identifier+'/')]
    with zipfile.ZipFile(archive) as bundle:
        if len(bundle.namelist()) != len(expected) or set(bundle.namelist()) != set(expected):
            raise ValueError('Incomplete bundled plugin archive')
        for name in expected:
            info = bundle.getinfo(name)
            if info.file_size > 256 * 1024 * 1024:
                raise ValueError('Bundled plugin file is too large')
            data = bundle.read(name)
            manifest['sha256'][name] = hashlib.sha256(data).hexdigest()
            path = application / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)


def export(manifest, read, output):
    """Recover optional standalone assets from a verified cached full package."""
    names = files(manifest)
    if not names:
        raise ValueError('Complete package has no bundled wallpaper plugin')
    verify(manifest, read)
    output = Path(output); output.mkdir(parents=True, exist_ok=True)
    for identifier in manifest['bundled_plugins']:
        prefix = 'plugins/' + identifier + '/'
        archive = output / f"eclipse-plugin-{identifier}-{manifest['target']}-{manifest['source_commit'][:12]}.zip"
        pending = archive.with_suffix('.zip.tmp')
        try:
            with zipfile.ZipFile(pending, 'w', compression=zipfile.ZIP_DEFLATED) as bundle:
                for name in names:
                    if name.startswith(prefix):
                        data = read(name)
                        if hashlib.sha256(data).hexdigest() != manifest['sha256'][name]:
                            raise ValueError('Bundled plugin hash differs: ' + name)
                        bundle.writestr(name, data)
            os.replace(pending, archive)
        finally:
            pending.unlink(missing_ok=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    source = parser.add_mutually_exclusive_group(required=True)
    source.add_argument('--application', type=Path)
    source.add_argument('--windows-archive', type=Path)
    parser.add_argument('--output-dir', type=Path, required=True)
    args = parser.parse_args()
    if args.windows_archive:
        with zipfile.ZipFile(args.windows_archive) as bundle:
            export(json.loads(bundle.read('version.json')), bundle.read, args.output_dir)
    else:
        manifest = json.loads((args.application / 'version.json').read_text())
        export(manifest, lambda name: (args.application / name).read_bytes(), args.output_dir)


if __name__ == '__main__':
    main()
