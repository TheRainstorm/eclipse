#!/usr/bin/env python3
"""Set a Linux host's pairing password locally or over SSH, without restarting it."""
import argparse
import fcntl
import getpass
import hashlib
import json
import os
from pathlib import Path
import secrets
import shlex
import subprocess
import sys
import tempfile


def process_data_dir(process):
    environment = dict(item.split('=', 1) for item in
                       (process / 'environ').read_text().split('\0') if '=' in item)
    if 'ECLIPSE_PROFILE_DIR' in environment:
        profile = Path(environment['ECLIPSE_PROFILE_DIR'])
        if not profile.is_absolute():
            raise ValueError('ECLIPSE_PROFILE_DIR must be an absolute path')
        return profile / 'data'
    executable = Path(os.readlink(process / 'exe').removesuffix(' (deleted)'))
    portable = executable.parent / 'eclipse-data'
    if portable.exists():
        return portable / 'data'
    home = Path(environment['HOME'])
    return Path(environment.get('XDG_DATA_HOME', str(home / '.local/share'))) / 'eclipse'


def select_data_dir(explicit):
    if explicit:
        return Path(explicit).expanduser().resolve(strict=True)
    profiles = set()
    for process in Path('/proc').iterdir():
        if not process.name.isdigit():
            continue
        try:
            if process.stat().st_uid != os.getuid():
                continue
            executable = Path(os.readlink(process / 'exe').removesuffix(' (deleted)'))
            if executable.name not in ('eclipse', 'uu-gui'):
                continue
            profiles.add(process_data_dir(process).resolve(strict=True))
        except (FileNotFoundError, ProcessLookupError, PermissionError):
            continue
    if len(profiles) != 1:
        raise ValueError('Cannot select one running Eclipse profile; specify --data-dir explicitly')
    return profiles.pop()


def validate_permissions(permissions):
    if (not isinstance(permissions, dict)
            or set(permissions) - {'view', 'control', 'details'}
            or permissions.get('view') is not True
            or type(permissions.get('control')) is not bool):
        raise ValueError('Invalid existing password permissions; original file preserved')
    details = permissions.get('details')
    if details is not None:
        names = {'keyboard', 'mouse', 'gamepad', 'clipboard', 'manage_server'}
        if (not isinstance(details, dict) or set(details) - names
                or any(type(value) is not bool for value in details.values())
                or permissions['control'] != any(details.get(key, False)
                                                 for key in ('keyboard', 'mouse', 'gamepad'))):
            raise ValueError('Invalid existing detailed permissions; original file preserved')


def set_password(data_dir, password):
    directory = data_dir / 'device-trust'
    if not (directory / 'identity.pk8').is_file():
        raise ValueError('No existing Eclipse identity in the selected data directory')
    # Version 1 matches uu-core/src/pairing_password.rs. Never store plaintext.
    salt = secrets.token_bytes(16)
    verifier = hashlib.pbkdf2_hmac('sha256', password, salt, 600_000, dklen=32)
    destination = directory / 'pairing-password.json'
    lock_fd = os.open(directory / 'store.lock', os.O_RDWR | os.O_CREAT, 0o600)
    with os.fdopen(lock_fd, 'rb+') as lock:
        # Same flock as TrustStore: preserve permissions from the latest record.
        fcntl.flock(lock, fcntl.LOCK_EX)
        permissions = {'view': True, 'control': True}
        if destination.is_symlink():
            raise ValueError('Password file must not be a symbolic link')
        if destination.exists():
            record = json.loads(destination.read_bytes())
            if record.get('version') != 1:
                raise ValueError('Unsupported password record version; original file preserved')
            permissions = record['permissions']
        validate_permissions(permissions)
        record = dict(version=1, salt=list(salt), verifier=list(verifier), permissions=permissions)
        fd, temporary = tempfile.mkstemp(prefix='.eclipse-password-', dir=directory)
        try:
            with os.fdopen(fd, 'wb') as output:
                output.write(json.dumps(record, separators=(',', ':')).encode())
                output.flush()
                os.fsync(output.fileno())
            os.replace(temporary, destination)
            if json.loads(destination.read_bytes()) != record:
                raise RuntimeError('Password readback did not match the update')
        finally:
            Path(temporary).unlink(missing_ok=True)
    return dict(updated=True, data_dir=str(data_dir), permissions=permissions,
                restart_required=False)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--ssh', metavar='HOST', help='SSH destination; uses your SSH config')
    parser.add_argument('--data-dir', help='Target data directory; otherwise detect the running GUI profile')
    parser.add_argument('--password-stdin', action='store_true', help='Read one UTF-8 line instead of prompting')
    args = parser.parse_args(argv)
    try:
        if args.password_stdin:
            password = sys.stdin.buffer.read(258)
            if password.endswith(b'\n'):
                password = password[:-1].removesuffix(b'\r')
            password.decode('utf-8')
        else:
            text = getpass.getpass('New pairing password: ')
            if text != getpass.getpass('Confirm password: '):
                raise ValueError('Passwords do not match')
            password = text.encode('utf-8')
        if not 8 <= len(password) <= 256 or b'\n' in password or b'\r' in password:
            raise ValueError('Password must be one UTF-8 line of 8–256 bytes')
        if args.ssh:
            command = ['python3', '-c', Path(__file__).read_text(), '--password-stdin']
            if args.data_dir:
                command.extend(['--data-dir', args.data_dir])
            return subprocess.run(['ssh', '-o', 'BatchMode=yes', '-o', 'ConnectTimeout=10',
                                   '--', args.ssh, shlex.join(command)], input=password).returncode
        print(json.dumps(set_password(select_data_dir(args.data_dir), password), ensure_ascii=False))
        return 0
    except (OSError, ValueError, KeyError, RuntimeError, EOFError) as error:
        print('Error: ' + str(error), file=sys.stderr)
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
