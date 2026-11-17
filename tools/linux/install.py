#!/usr/bin/env python3
"""Verify and install this package into a versioned, per-user Linux directory."""
import argparse
import hashlib
import json
import re
import os
from pathlib import Path, PurePosixPath
import shutil
import subprocess
import tempfile

LICENSES = ['LICENSE', 'LICENSE-MIT', 'LICENSE-APACHE', 'THIRD-PARTY-NOTICES.txt']

LEGACY_PAYLOAD = ['uu-gui', 'uu', 'README.txt', 'install.py', 'eclipse.png', *LICENSES]
V2_PAYLOAD = [*LEGACY_PAYLOAD,'libpyrowave-shared.so.1']
PAYLOAD = ['eclipse','eclipse-cli',*V2_PAYLOAD[2:]]


def bundled_plugin_source(manifest, identifier):
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


def bundled_plugin_files(manifest):
    plugins = manifest.get('bundled_plugins', {})
    if not isinstance(plugins, dict) or len(plugins) > 16:
        raise ValueError('Invalid bundled plugin inventory')
    notices = manifest.get('bundled_plugin_notices', {})
    if not isinstance(notices, dict) or not set(notices).issubset(plugins):
        raise ValueError('Invalid bundled plugin notices')
    files = []
    for identifier, library in plugins.items():
        if (not isinstance(identifier, str) or not re.fullmatch(r'[a-z0-9][a-z0-9-]{0,63}', identifier)
                or not isinstance(library, str) or not re.fullmatch(r'[A-Za-z0-9_-]+\.so', library)):
            raise ValueError('Invalid bundled plugin path')
        extra = notices.get(identifier, [])
        if not isinstance(extra, list) or any(not isinstance(name,str) for name in extra) or len(extra) != len(set(extra)) or not set(extra).issubset({'LICENSE','ENGINE-THIRD-PARTY-NOTICES.txt'}):
            raise ValueError('Invalid bundled plugin notice path')
        bundled_plugin_source(manifest,identifier)
        files.extend('plugins/' + identifier + '/' + name for name in
                     ('plugin.json', 'build.json', library, 'README.md', 'LICENSE-MIT', 'LICENSE-APACHE', 'THIRD-PARTY-NOTICES.txt', *extra))
    return files


def verify_bundled_plugins(manifest, read):
    for identifier, library in manifest.get('bundled_plugins', {}).items():
        prefix = 'plugins/' + identifier + '/'
        plugin, build = (json.loads(read(prefix + name)) for name in ('plugin.json', 'build.json'))
        if (plugin.get('id') != identifier or plugin.get('api_version') != 1
                or plugin.get('sdk_id') != manifest.get('plugin_sdk_id') or plugin.get('library') != library
                or plugin.get('library_sha256') != manifest['sha256'][prefix + library]
                or build.get('sdk_id') != manifest.get('plugin_sdk_id')
                or build.get('source_commit') != bundled_plugin_source(manifest,identifier)
                or (identifier in manifest.get('bundled_plugin_sources', {})
                    and build.get('source_repository') != manifest['bundled_plugin_sources'][identifier]['repository'])
                or build.get('application_source_commit') != manifest['source_commit']
                or build.get('target') != manifest.get('target')):
            raise ValueError('Bundled plugin source or SDK differs from application')


def executables(manifest):
    return ('eclipse','eclipse-cli') if manifest.get('format_version',1)>=4 else ('uu-gui','uu')
def required_files(manifest):
    version=manifest.get('format_version',1)
    if type(version) is not int or version not in (1,2,3,4):
        raise ValueError('Unsupported Linux package format version')
    if version>=3:
        runtime=manifest.get('plugin_runtime')
        if (not isinstance(runtime,list) or len(runtime)!=2 or runtime[0]!='libuu_plugin_sdk.so'
            or not isinstance(runtime[1],str) or not re.fullmatch(r'libstd-[0-9a-f]+\.so',runtime[1])
            or not re.fullmatch(r'[0-9a-f]{64}',manifest.get('plugin_sdk_id',''))):
            raise ValueError('Invalid plugin SDK runtime manifest')
        extra = []
        if 'uninstaller' in manifest:
            if version != 4 or manifest['uninstaller'] != 'uninstall.py':
                raise ValueError('Invalid Linux uninstaller entry')
            extra.append('uninstall.py')
        return [*(PAYLOAD if version==4 else V2_PAYLOAD),*runtime,*extra,*bundled_plugin_files(manifest)]
    return V2_PAYLOAD if version==2 else LEGACY_PAYLOAD


def verify_package(package):
    metadata = package / 'version.json'
    if metadata.is_symlink() or not metadata.is_file():
        raise ValueError('Package requires a regular version.json file')
    manifest = json.loads(metadata.read_text())
    if not isinstance(manifest, dict):
        raise ValueError('Invalid package version manifest')
    revision = manifest.get('source_commit')
    if not isinstance(revision, str) or len(revision) != 40 or any(c not in '0123456789abcdef' for c in revision):
        raise ValueError('Invalid source revision')
    hashes = manifest.get('sha256')
    required=required_files(manifest)
    if not isinstance(hashes, dict) or set(hashes) != set(required):
        raise ValueError('Invalid package hash manifest')
    for name in required:
        path = package / name
        if path.is_symlink() or not path.is_file() or hashlib.sha256(path.read_bytes()).hexdigest() != hashes[name]:
            raise ValueError(f'Integrity check failed: {name}')
        if name.startswith('plugins/') and ((package/'plugins').is_symlink() or path.parent.is_symlink()):
            raise ValueError(f'Integrity check failed: {name}')
    verify_bundled_plugins(manifest, lambda name: (package / name).read_bytes())
    return manifest


def verify_installation(package):
    """A development bundle binds the main payload and matching extensions.

    Ordinary release packages keep their original contract and install identity.
    Account/configuration files are never part of either payload.
    """
    manifest = verify_package(package)
    files = [*required_files(manifest), 'version.json']
    metadata = package / 'development.json'
    if not metadata.exists() and not metadata.is_symlink():
        return manifest, None, files
    if metadata.is_symlink() or not metadata.is_file() or metadata.stat().st_size > 1024 * 1024:
        raise ValueError('Invalid development manifest')
    development = json.loads(metadata.read_text())
    if not isinstance(development, dict) or development.get('format_version') != 1 or manifest.get('format_version') != 4:
        raise ValueError('Unsupported Linux development bundle')
    for key in ('main_commit', 'plugin_commit', 'declared_engine_commit'):
        if not isinstance(development.get(key), str) or not re.fullmatch(r'[0-9a-f]{40}', development[key]):
            raise ValueError('Invalid development source identity')
    if development['main_commit'] != manifest['source_commit']:
        raise ValueError('Development main source differs')
    hashes = development.get('sha256')
    if not isinstance(hashes, dict) or not set(files).issubset(hashes):
        raise ValueError('Incomplete development payload inventory')
    extensions = {}
    for name in set(hashes) - set(files):
        path = PurePosixPath(name)
        if (name != path.as_posix() or len(path.parts) != 3 or path.parts[0] != 'plugins'
                or not re.fullmatch(r'[a-z0-9-]{1,64}', path.parts[1])
                or path.parts[2] in ('.', '..') or '\\' in name):
            raise ValueError('Invalid development extension path')
        extensions.setdefault(path.parts[1], set()).add(path.parts[2])
    if not extensions or len(extensions) > 128:
        raise ValueError('Invalid development extension inventory')
    for identifier, names in extensions.items():
        folder = package / 'plugins' / identifier
        plugin_path = folder / 'plugin.json'
        if any(path.is_symlink() for path in (package/'plugins', folder, plugin_path)) or not plugin_path.is_file() or plugin_path.stat().st_size > 65536:
            raise ValueError('Invalid development plugin manifest')
        plugin = json.loads(plugin_path.read_text())
        if (not isinstance(plugin, dict) or plugin.get('id') != identifier or plugin.get('api_version') != 1
                or plugin.get('sdk_id') != manifest['plugin_sdk_id']
                or not isinstance(plugin.get('library'), str)
                or not re.fullmatch(r'[A-Za-z0-9_-]+\.so', plugin['library'])):
            raise ValueError('Development plugin does not match the application SDK')
        extension_files = {'plugin.json', plugin['library'], 'README.md', 'LICENSE', 'LICENSE-MIT',
                           'LICENSE-APACHE', 'THIRD-PARTY-NOTICES.txt', 'ENGINE-THIRD-PARTY-NOTICES.txt'}
        if names != extension_files:
            raise ValueError('Incomplete development extension payload')
        if hashes['plugins/' + identifier + '/' + plugin['library']] != plugin.get('library_sha256'):
            raise ValueError('Development plugin library digest differs')
    payload = sorted(hashes)
    for name in payload:
        path = package / name
        expected = hashes[name]
        if (not isinstance(expected, str) or not re.fullmatch(r'[0-9a-f]{64}', expected)
                or path.is_symlink() or not path.is_file()
                or hashlib.sha256(path.read_bytes()).hexdigest() != expected):
            raise ValueError(f'Development integrity check failed: {name}')
    return manifest, development, [*payload, 'development.json']


def release_name(manifest, development):
    name = manifest['source_commit'][:12]
    return name + '-' + development['plugin_commit'][:12] if development else name


def desktop_value(value):
    return str(value).replace('\\', '\\\\').replace('\n', '\\n').replace('\r', '\\r').replace('\t', '\\t')


def desktop_entry(base, legacy=False, gui='eclipse'):
    executable = str(base / 'current' / gui)
    # Exec quoting is applied after the desktop string's own escape layer.
    # Percent is a field code even inside quotes; a literal percent is %%.
    def quoted_argument(value):
        quoted = ''.join('\\' + c if c in '\\"`$' else c for c in str(value)).replace('%', '%%')
        return '"' + quoted + '"'
    # GLib validates argv[0] before expanding %%; keep the program with a
    # possibly literal percent in argv[1]. env invokes it directly, no shell.
    launcher = shutil.which('env')
    if not launcher:
        raise RuntimeError('The standard env utility is required for the desktop launcher')
    command = desktop_value(quoted_argument(launcher) + ' ' + quoted_argument(executable))
    return ('[Desktop Entry]\nType=Application\nName=Eclipse\nName[zh_CN]=Eclipse\n'
            'Comment=Windows / Linux remote streaming client\n'
            f'Exec={command}\n'
            f'TryExec={desktop_value(executable)}\nIcon={desktop_value(base / "current" / "eclipse.png")}\n'
            'Terminal=false\nCategories=Network;RemoteAccess;\nStartupNotify=true\nStartupWMClass=Eclipse\n'
            + ('NoDisplay=true\n' if legacy else ''))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--prefix', type=Path, default=Path.home() / '.local')
    parser.add_argument('--package', type=Path, default=Path(__file__).resolve().parent)
    args = parser.parse_args()
    package = args.package.resolve()
    manifest, development, payload = verify_installation(package)
    revision = manifest['source_commit']
    gui,cli=executables(manifest)
    libraries = [gui,cli,*(['libpyrowave-shared.so.1'] if manifest.get('format_version',1)>=2 else []),*manifest.get('plugin_runtime',[])]
    libraries.extend(name for name in payload if name.startswith('plugins/') and name.endswith('.so'))
    environment = dict(os.environ, LD_LIBRARY_PATH=str(package) + (':' + os.environ['LD_LIBRARY_PATH'] if os.environ.get('LD_LIBRARY_PATH') else ''))
    for name in libraries:
        deps = subprocess.check_output(['ldd', str(package / name)], text=True, env=environment)
        if 'not found' in deps: raise RuntimeError(f'Missing shared libraries for {name}: {deps}')
    prefix = args.prefix.expanduser().resolve()
    if any(ord(c) < 32 for c in str(prefix)):
        raise ValueError('Installation prefix must not contain control characters')
    base = prefix / 'opt' / 'eclipse'
    desktop_contents = [(False, desktop_entry(base,gui=gui)), (True, desktop_entry(base, legacy=True,gui=gui))]
    releases = base / 'releases'
    uninstall_entry = prefix / 'bin/eclipse-uninstall'
    if manifest.get('uninstaller') and (uninstall_entry.exists() or uninstall_entry.is_symlink()):
        if not uninstall_entry.is_symlink() or not uninstall_entry.resolve().is_relative_to(base):
            raise ValueError('Uninstall command already belongs to another installation')
    releases.mkdir(parents=True, exist_ok=True)
    dest = releases / release_name(manifest, development)
    if dest.is_symlink():
        raise ValueError('Release directory must not be a symbolic link')
    if not dest.exists():
        staging = Path(tempfile.mkdtemp(prefix='.install-', dir=releases))
        try:
            for name in payload:
                (staging / name).parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(package / name, staging / name)
            for name in [gui,cli]: (staging / name).chmod(0o755)
            if manifest.get('uninstaller'): (staging / 'uninstall.py').chmod(0o755)
            staging.rename(dest)
        finally:
            if staging.exists(): shutil.rmtree(staging)
    else:
        # Reusing a release must not promote a damaged or misidentified install.
        # Build timestamps may differ for a legitimate rebuild of the same source.
        existing, existing_development, _ = verify_installation(dest)
        if existing['source_commit'] != revision or existing['sha256'] != manifest['sha256']:
            raise ValueError('Existing release differs; refusing to overwrite a running installation')
        if existing_development != development:
            raise ValueError('Existing development release differs; refusing to overwrite it')
    def link(target, path):
        path.parent.mkdir(parents=True, exist_ok=True)
        pending = path.with_name(path.name + f'.tmp-{os.getpid()}')
        try:
            pending.symlink_to(target)
            pending.replace(path)
        finally: pending.unlink(missing_ok=True)
    link(dest, base / 'current')
    for name in ['eclipse','uu-client','uu-remote-rust']:link(base/'current'/gui,prefix/'bin'/name)
    for name in ['eclipse-cli','uu-rust']:link(base/'current'/cli,prefix/'bin'/name)
    if manifest.get('uninstaller'): link(base/'current'/'uninstall.py',uninstall_entry)
    # The filename matches the window's Wayland app_id (brand::NAME).
    desktop = prefix / 'share' / 'applications' / 'Eclipse.desktop'
    desktop.parent.mkdir(parents=True, exist_ok=True)
    for legacy, contents in desktop_contents:
        path = desktop.with_name('uu-remote-rust.desktop') if legacy else desktop
        pending = path.with_name(path.name + f'.tmp-{os.getpid()}')
        try:
            pending.write_text(contents, encoding='utf-8')
            pending.replace(path)
        finally:
            pending.unlink(missing_ok=True)
    # Old pinned launchers remain usable without duplicate application entries.
    old_desktop=desktop.with_name('uu-client.desktop')
    if old_desktop.exists():
        pending=old_desktop.with_name(old_desktop.name+f'.tmp-{os.getpid()}')
        try:
            pending.write_text(desktop_entry(base,legacy=True,gui=gui),encoding='utf-8')
            pending.replace(old_desktop)
        finally:pending.unlink(missing_ok=True)
    subprocess.run(['update-desktop-database', str(desktop.parent)], check=False) if shutil.which('update-desktop-database') else None
    print(json.dumps({'source_commit': revision, 'executable': str(base / 'current' / gui), 'desktop': str(desktop)}, ensure_ascii=False))

if __name__ == '__main__':
    main()
