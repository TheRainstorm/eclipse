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


def files(manifest):
    plugins = manifest.get('bundled_plugins', {})
    if not isinstance(plugins, dict) or len(plugins) > 16:
        raise ValueError('Invalid bundled plugin inventory')
    result = []
    suffix = '.dll' if manifest.get('target', '').endswith('windows-gnu') else '.so'
    for identifier, library in plugins.items():
        if (not isinstance(identifier, str) or not re.fullmatch(r'[a-z0-9][a-z0-9-]{0,63}', identifier)
                or not isinstance(library, str)
                or not re.fullmatch(r'[A-Za-z0-9_-]+' + re.escape(suffix), library)):
            raise ValueError('Invalid bundled plugin path')
        result.extend('plugins/' + identifier + '/' + name
                      for name in ('plugin.json', 'build.json', library, *NOTICES))
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
                or build.get('source_commit') != manifest['source_commit']
                or build.get('application_source_commit') != manifest['source_commit']
                or build.get('target') != manifest.get('target')):
            raise ValueError('Bundled plugin source or SDK differs from application')


def stage(application, manifest, target, features, output):
    """Reuse package_plugin's SDK/import probe and retain its standalone asset."""
    application, output = Path(application), Path(output)
    definition = json.loads((ROOT / 'extensions/wallpaper-collects/plugin.json').read_text())
    subprocess.run([sys.executable, str(ROOT / 'tools/release/package_plugin.py'),
                    '--manifest', str(ROOT / 'extensions/wallpaper-collects/plugin.json'),
                    '--target', target, '--application', str(application),
                    '--output-dir', str(output), '--features', *features], check=True, cwd=ROOT, stdout=sys.stderr)
    source = manifest['source_commit']
    archive = output / f"eclipse-plugin-{definition['id']}-{target}-{source[:12]}.zip"
    library = definition['library'] + '.dll' if 'windows' in target else 'lib' + definition['library'] + '.so'
    manifest['bundled_plugins'] = {definition['id']: library}
    manifest['target'] = target
    expected = files(manifest)
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
    verify(manifest, lambda name: (application / name).read_bytes())
    return archive


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
