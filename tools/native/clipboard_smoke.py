#!/usr/bin/env python3
"""Exercise native text consent on two fresh, authenticated Xvfb servers."""
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
    args = parser.parse_args()
    os.umask(0o077)
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    runtime = Path(tempfile.mkdtemp(prefix='uu-native-clipboard-'))
    servers, probes, logs = [], [], []
    try:
        environments = []
        for role in ('host', 'viewer'):
            number = next((n for n in range(90, 190)
                           if not Path(f'/tmp/.X11-unix/X{n}').exists()
                           and not Path(f'/tmp/.X{n}-lock').exists()), None)
            if number is None:
                raise RuntimeError('No free owned X11 test display')
            display = f':{number}'
            authority = runtime / f'{role}.Xauthority'
            def field(value):
                return struct.pack('>H', len(value)) + value
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
                       XDG_DATA_HOME=str(runtime / role / 'data'), XDG_STATE_HOME=str(runtime / role / 'state'),
                       UU_SIGNING_KEY_FILE=str(runtime / 'absent-key'), UU_DECODER='sw', RUST_LOG='error')
            log = (out / f'{role}-xvfb.log').open('w')
            logs.append(log)
            server = subprocess.Popen(['Xvfb', display, '-screen', '0', '640x360x24',
                                       '-nolisten', 'tcp', '-auth', str(authority)],
                                      env=env, stdout=log, stderr=subprocess.STDOUT)
            servers.append(server)
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
            env['UU_CLIPBOARD_TEST_SERVER_PID'] = str(server.pid)
            environments.append(env)
        phases = runtime / 'phases'
        phases.mkdir(mode=0o700)
        invitation = runtime / 'session.eclipse-invite.json'
        for role, env in zip(('host', 'viewer'), environments):
            probe = subprocess.Popen([str(args.probe.resolve()), role, str(invitation), str(phases)],
                                     env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            probes.append(probe)
        reports = []
        # Both roles advance concurrently; wait for the controller first.
        for role, probe in reversed(list(zip(('host', 'viewer'), probes))):
            stdout, stderr = probe.communicate(timeout=40)
            (out / f'{role}.log').write_text(stdout + stderr)
            if probe.returncode:
                raise RuntimeError(f'Owned {role} clipboard fixture failed: ' + stderr[-2000:])
            report = json.loads(stdout)
            if report.get('native_clipboard') is not True or report.get('clean_stop') is not True:
                raise RuntimeError('Native clipboard fixture did not report completion')
            reports.append(report)
        if invitation.exists():
            raise RuntimeError('Native clipboard host left its invitation behind')
        result = dict(native_clipboard='passed', isolated_desktops=2, system_clipboard='owned Xvfb only', reports=reports)
        (out / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
        print(json.dumps(result))
    finally:
        for process in list(reversed(probes)) + list(reversed(servers)):
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)
        for log in logs:
            log.close()
        shutil.rmtree(runtime)


if __name__ == '__main__':
    main()
