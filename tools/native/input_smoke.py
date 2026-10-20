#!/usr/bin/env python3
"""Run an X11 check on a fresh, authenticated Xvfb; never use the user's DISPLAY."""
import argparse
import json
import os
from pathlib import Path
import secrets
import shutil
import socket
import struct
import subprocess
import tempfile
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--probe', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--probe-arg', action='append', default=[])
    parser.add_argument('--report-field', choices=('input', 'window', 'video'), default='input')
    args = parser.parse_args()
    os.umask(0o077)
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    runtime = Path(tempfile.mkdtemp(prefix='uu-input-x11-'))
    server = None
    try:
        number = next((n for n in range(90, 190)
                       if not Path(f'/tmp/.X11-unix/X{n}').exists()
                       and not Path(f'/tmp/.X{n}-lock').exists()), None)
        if number is None:
            raise RuntimeError('No free owned X11 test display')
        display = f':{number}'
        authority = runtime / 'Xauthority'
        def field(value):
            return struct.pack('>H', len(value)) + value
        # FamilyLocal record for this server, with a private random cookie.
        authority.write_bytes(struct.pack('>H', 256) + field(socket.gethostname().encode())
                              + field(str(number).encode()) + field(b'MIT-MAGIC-COOKIE-1')
                              + field(secrets.token_bytes(16)))
        authority.chmod(0o600)
        env = os.environ.copy()
        for key in ('DISPLAY', 'WAYLAND_DISPLAY', 'SWAYSOCK', 'DBUS_SESSION_BUS_ADDRESS',
                    'UU_MEDIA_DEBUG', 'UU_DUMP_H264', 'UU_INPUT_AUDIT', 'UU_AUTOCONNECT', 'UU_FORCE_JOIN'):
            env.pop(key, None)
        for key in list(env):
            if key.startswith('UU_TEST_'):
                env.pop(key)
        env.update(DISPLAY=display, XAUTHORITY=str(authority),
                   XDG_DATA_HOME=str(runtime / 'data'), XDG_STATE_HOME=str(runtime / 'state'),
                   UU_SIGNING_KEY_FILE=str(runtime / 'absent-signing-key'), UU_DECODER='sw', RUST_LOG='error')
        with (out / 'xvfb.log').open('w') as log:
            server = subprocess.Popen(['Xvfb', display, '-screen', '0', '1280x720x24',
                                       '-nolisten', 'tcp', '-auth', str(authority)],
                                      env=env, stdout=log, stderr=subprocess.STDOUT)
            deadline = time.monotonic() + 10
            while True:
                if server.poll() is not None:
                    raise RuntimeError('Owned Xvfb failed to start')
                lock = Path(f'/tmp/.X{number}-lock')
                if lock.exists() and lock.read_text().strip() == str(server.pid) and Path(f'/tmp/.X11-unix/X{number}').exists():
                    break
                if time.monotonic() > deadline:
                    raise TimeoutError('Owned Xvfb did not become ready')
                time.sleep(.05)
            env['UU_INPUT_TEST_SERVER_PID'] = str(server.pid)
            result = subprocess.run([str(args.probe.resolve()), '--x11-display', display, *args.probe_arg],
                                    env=env, capture_output=True, text=True, timeout=20)
            (out / 'probe.log').write_text(result.stdout + result.stderr)
            if result.returncode:
                raise RuntimeError('Owned input fixture failed: ' + result.stderr[-2000:])
            report = json.loads(result.stdout)
            if report.get(args.report_field) != 'passed':
                raise RuntimeError('Owned X11 fixture did not report success')
            (out / 'result.json').write_text(json.dumps(report, indent=2) + '\n')
            print(json.dumps(report))
    finally:
        if server and server.poll() is None:
            server.terminate()
            try:
                server.wait(timeout=5)
            except subprocess.TimeoutExpired:
                server.kill(); server.wait(timeout=5)
        shutil.rmtree(runtime)


if __name__ == '__main__':
    main()
