#!/usr/bin/env python3
"""Test LAN discovery, confirmed pairing and generated video on owned loopback sockets."""
import argparse
import json
import os
from pathlib import Path
import queue
import re
import shutil
import socket
import subprocess
import tempfile
import threading
import time


class Process:
    def __init__(self, command, env):
        self.lines = queue.Queue()
        self.process = subprocess.Popen(command, env=env, stdin=subprocess.PIPE,
                                        stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                        text=True, bufsize=1)
        def read():
            for line in self.process.stdout:
                self.lines.put(line.strip())
        self.reader = threading.Thread(target=read, daemon=True)
        self.reader.start()

    def line(self, pattern, timeout=10):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            try:
                value = self.lines.get(timeout=.1)
            except queue.Empty:
                if self.process.poll() is not None:
                    raise RuntimeError('Owned CLI process ended before its expected event')
                continue
            matched = re.search(pattern, value)
            if matched:
                return matched
        raise TimeoutError('Owned CLI process did not report its expected event')

    def send(self, line):
        self.process.stdin.write(line + '\n')
        self.process.stdin.flush()

    def finish(self):
        code = self.process.wait(timeout=8)
        if code:
            raise RuntimeError('Owned CLI process returned a failure')

    def cleanup(self):
        if self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.process.kill(); self.process.wait(timeout=5)
        self.reader.join(timeout=1)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--exe', required=True, type=Path)
    parser.add_argument('--output', required=True, type=Path)
    args = parser.parse_args()
    os.umask(0o077)
    out = args.output.resolve(); out.mkdir(parents=True, exist_ok=False)
    root = Path(tempfile.mkdtemp(prefix='eclipse-lan-smoke-'))
    processes = []
    try:
        binary = str(args.exe.resolve())
        def environment(role):
            env = os.environ.copy()
            for name in list(env):
                if name.startswith(('UU_', 'WAYLAND', 'SWAY')) or name == 'DISPLAY':
                    env.pop(name, None)
            env.update(XDG_DATA_HOME=str(root/role/'data'),
                       XDG_STATE_HOME=str(root/role/'state'),
                       UU_DECODER='sw', UU_SIGNING_KEY_FILE=str(root/'absent-key'),
                       RUST_LOG='error')
            return env
        host_env, client_env = environment('host'), environment('client')
        def run(*arguments, success=True):
            result = subprocess.run([binary, 'lan', *arguments], env=client_env,
                                    stdin=subprocess.DEVNULL, capture_output=True,
                                    text=True, timeout=20)
            if (result.returncode == 0) != success:
                raise RuntimeError('LAN diagnostic command had an unexpected exit status')
            return result
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
            sock.bind(('127.0.0.1', 0)); discovery_port = sock.getsockname()[1]
        def start(address):
            host = Process([binary, 'lan', 'host', '--synthetic', '--synthetic-width', '64',
                            '--synthetic-height', '64', '--fps', '30', '--bitrate-kbps', '300',
                            '--bind', address, '--discovery-bind', f'127.0.0.1:{discovery_port}'], host_env)
            processes.append(host)
            endpoint = host.line(r'LAN server listening on (127\.0\.0\.1:\d+)').group(1)
            return host, endpoint
        host, endpoint = start('127.0.0.1:0')
        metadata = json.loads(run('inspect', endpoint).stdout)
        assert metadata['pairing_open']
        discovered = [json.loads(line) for line in run('discover', '--target', f'127.0.0.1:{discovery_port}').stdout.splitlines()]
        assert len(discovered) == 1 and discovered[0]['id'] == metadata['id']
        # New hosts accept requests by default, but never auto-approve them.
        # The diagnostic CLI can explicitly close admission independently of
        # remembered authorizations; verify that transition too.
        host.send('close-pairing')
        deadline = time.monotonic() + 3
        while json.loads(run('inspect', endpoint).stdout)['pairing_open']:
            if time.monotonic() >= deadline:
                raise TimeoutError('Owned host did not close new pairing admission')
            time.sleep(.02)
        run('pair', endpoint, success=False)
        host.send('pair'); host.line('New pairing requests enabled')
        client = Process([binary, 'lan', 'pair', endpoint], client_env); processes.append(client)
        # Both ends are controlled fixtures. A production host never auto-approves.
        code = client.line(r'Enter this code on the host to approve pairing: (\d{4} \d{4})').group(1)
        host.line('Pairing requested by')
        host.send('code ' + ('11111111' if code.replace(' ', '') == '00000000' else '00000000'))
        host.line('Pairing code does not match')
        assert not (root/'client/data/uu-remote/native-lan/paired-devices.json').exists()
        host.send('code ' + code)
        paired_id = client.line(r'^paired ([a-f0-9-]{36})$').group(1); client.finish()
        del code
        saved = [json.loads(line) for line in run('devices').stdout.splitlines()]
        assert len(saved) == 1 and saved[0]['id'] == paired_id
        assert 'token' not in saved[0] and 'certificate' not in saved[0]
        report = out/'first-probe.json'
        run('probe', paired_id, '--frames', '30', '--report', str(report))
        first = json.loads(report.read_text())
        assert first['completed'] and first['final']['decoded'] >= 30
        host.send('stop'); host.finish()
        host, restarted = start(endpoint)
        assert restarted == endpoint and json.loads(run('inspect', endpoint).stdout)['pairing_open']
        report = out/'resumed-probe.json'
        run('probe', paired_id, '--frames', '30', '--report', str(report))
        assert json.loads(report.read_text())['completed']
        book = json.loads((root/'client/data/uu-remote/native-lan/paired-devices.json').read_text())
        host.send('revoke ' + book['client_id']); del book
        host.line('Device authorization revoked')
        run('probe', paired_id, '--frames', '1', success=False)
        host.send('stop'); host.finish()
        with socket.socket() as sock:
            sock.settimeout(.5)
            assert sock.connect_ex(('127.0.0.1', int(endpoint.rsplit(':', 1)[1]))) != 0
        report = {'lan': 'passed', 'discovery': True, 'host_confirmation': True,
                  'wrong_code_rejected': True, 'default_pairing_admission': True,
                  'explicit_pairing_close': True, 'generated_video': True,
                  'saved_pairing_after_restart': True, 'revocation': True, 'clean_shutdown': True}
        (out/'result.json').write_text(json.dumps(report, indent=2)+'\n')
        print(json.dumps(report))
    finally:
        for process in reversed(processes): process.cleanup()
        shutil.rmtree(root)


if __name__ == '__main__':
    main()
