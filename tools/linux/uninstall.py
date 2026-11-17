#!/usr/bin/env python3
"""Uninstall Eclipse for the current Linux user; --purge also removes profiles."""
import argparse
import json
import os
from pathlib import Path
import shutil

COMMANDS = ('eclipse', 'eclipse-cli', 'eclipse-uninstall', 'uu-client', 'uu-remote-rust', 'uu-rust')
DESKTOPS = ('Eclipse.desktop', 'eclipse.desktop', 'uu-remote-rust.desktop', 'uu-client.desktop')


def default_prefix():
    script = Path(__file__).resolve()
    if (script.parent.parent.name == 'releases' and script.parents[2].name == 'eclipse'
            and script.parents[3].name == 'opt'):
        return script.parents[4]
    return Path.home() / '.local'


def autostart_exec(executable):
    escaped = str(executable).replace('\\', '\\\\').replace('"', '\\"').replace('`', '\\`').replace('$', '\\$').replace('%', '%%')
    return 'Exec=' + ('"/usr/bin/env" "' + escaped + '"').replace('\\', '\\\\')


def installed_processes(base):
    running = []
    for process in Path('/proc').iterdir():
        if not process.name.isdigit():
            continue
        try:
            executable = Path(os.readlink(process / 'exe').removesuffix(' (deleted)'))
            if process.stat().st_uid == os.getuid() and executable.is_relative_to(base):
                running.append(int(process.name))
        except (OSError, ValueError):
            continue
    return running


def profile_paths(home, environment):
    paths = []
    for variable, fallback in (('XDG_DATA_HOME', '.local/share'),
                               ('XDG_STATE_HOME', '.local/state'),
                               ('XDG_CACHE_HOME', '.cache')):
        root = Path(environment.get(variable) or home / fallback).expanduser()
        if not root.is_absolute():
            raise ValueError(variable + ' must be an absolute path')
        paths.extend(root / name for name in ('eclipse', 'uu-remote'))
    return list(dict.fromkeys(paths))


def uninstall(prefix, purge=False, dry_run=False):
    prefix = Path(prefix).expanduser().resolve()
    base = prefix / 'opt/eclipse'
    if base.is_symlink():
        raise ValueError('Installation directory must not be a symbolic link: ' + str(base))
    if base.exists() and not (base / 'releases').is_dir():
        raise ValueError('Not an Eclipse versioned installation: ' + str(base))
    running = installed_processes(base)
    if running:
        raise RuntimeError('Exit Eclipse and its streaming windows first; running PIDs: ' + ', '.join(map(str, running)))
    if not purge and base.exists() and any(path.is_symlink() or (path.is_dir() and any(path.iterdir()))
            for path in (base / 'releases').glob('*/eclipse-data')):
        raise RuntimeError('Back up portable eclipse-data before uninstalling, or use --purge to delete it explicitly')
    paths = []
    for name in COMMANDS:
        path = prefix / 'bin' / name
        if path.is_symlink() and path.resolve().is_relative_to(base):
            paths.append(path)
    config = Path(os.environ.get('XDG_CONFIG_HOME') or Path.home() / '.config').expanduser()
    if not config.is_absolute():
        raise ValueError('XDG_CONFIG_HOME must be an absolute path')
    for folder in (prefix / 'share/applications', config / 'autostart'):
        for name in DESKTOPS:
            path = folder / name
            if path.is_file() and not path.is_symlink():
                lines = path.read_text().splitlines()
                targets = [base / 'current' / gui for gui in ('eclipse', 'uu-gui')]
                targets.extend((base / 'releases').glob('*/eclipse'))
                if any('TryExec=' + str(target) in lines or autostart_exec(target) in lines
                       or 'Exec=' + str(target) in lines or 'Exec="' + str(target) + '"' in lines for target in targets):
                    paths.append(path)
    if base.exists():
        paths.append(base)
    if purge:
        paths.extend(path for path in profile_paths(Path.home(), os.environ)
                     if path.exists() or path.is_symlink())
    paths = list(dict.fromkeys(paths))
    report = dict(dry_run=dry_run, purge=purge, paths=[str(path) for path in paths])
    if not dry_run:
        for path in paths:
            if path.is_symlink() or not path.is_dir():
                path.unlink(missing_ok=True)
            else:
                shutil.rmtree(path)
    return report


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--prefix', type=Path, default=default_prefix(), help='Installation prefix (default: this installation, or ~/.local from an extracted package)')
    parser.add_argument('--purge', action='store_true', help='Also delete current and legacy settings, paired devices, history and caches')
    parser.add_argument('--dry-run', action='store_true', help='List paths without removing them')
    args = parser.parse_args(argv)
    try:
        report = uninstall(args.prefix, purge=args.purge, dry_run=args.dry_run)
    except (OSError, ValueError, RuntimeError) as error:
        parser.exit(1, str(error) + '\n')
    print(json.dumps(report, ensure_ascii=False, indent=2))
    return 0


if __name__ == '__main__':
    main()
