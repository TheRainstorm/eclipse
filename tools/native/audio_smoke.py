#!/usr/bin/env python3
"""Record a private null sink while the default input is a separate silent source."""
import argparse
import json
import math
import os
from pathlib import Path
import secrets
import select
import shutil
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
    runtime = Path(tempfile.mkdtemp(prefix='eclipse-owned-audio-'))
    processes = []
    logs = []

    def log(name):
        stream = (out / name).open('w')
        logs.append(stream)
        return stream

    try:
        cookie = runtime / 'cookie'
        cookie.write_bytes(secrets.token_bytes(256))
        cookie.chmod(0o600)
        server = runtime / 'native'
        env = os.environ.copy()
        for name in ('PULSE_SERVER', 'PULSE_COOKIE', 'DBUS_SESSION_BUS_ADDRESS', 'DISPLAY', 'WAYLAND_DISPLAY'):
            env.pop(name, None)
        env.update(XDG_RUNTIME_DIR=str(runtime), XDG_CONFIG_HOME=str(runtime / 'config'),
                   XDG_STATE_HOME=str(runtime / 'state'), XDG_DATA_HOME=str(runtime / 'data'),
                   PULSE_SERVER='unix:' + str(server), PULSE_COOKIE=str(cookie), RUST_LOG='error')
        config = runtime / 'test.pa'
        config.write_text(
            'load-module module-null-sink sink_name=eclipse_output rate=48000 channels=2\n'
            'load-module module-null-source source_name=eclipse_unrelated_input rate=48000 channels=2\n'
            'set-default-sink eclipse_output\nset-default-source eclipse_unrelated_input\n'
            f'load-module module-native-protocol-unix socket={server} auth-cookie={cookie}\n')
        pulse = subprocess.Popen(['pulseaudio', '--daemonize=no', '-n', '--file=' + str(config),
                                  '--use-pid-file=no', '--exit-idle-time=-1', '--disable-shm=yes',
                                  '--log-target=stderr'], env=env, stdout=log('pulse.log'), stderr=subprocess.STDOUT)
        processes.append(pulse)
        deadline = time.monotonic() + 8
        while not server.is_socket():
            if pulse.poll() is not None or time.monotonic() > deadline:
                raise RuntimeError('Owned audio server did not become ready')
            time.sleep(.05)
        # The socket was created in our private directory by this exact child.
        # All clients below receive only this server and its private cookie.
        tone = runtime / 'tone.raw'
        tone.write_bytes(b''.join(struct.pack('<hh', *([int(math.sin(i * math.tau * 440 / 48000) * 3200)] * 2))
                                  for i in range(48000 * 6)))
        player = subprocess.Popen(['paplay', '--device=eclipse_output', '--raw', '--rate=48000',
                                   '--channels=2', '--format=s16le', '--latency-msec=10', str(tone)],
                                  env=env, stdout=log('player.log'), stderr=subprocess.STDOUT)
        processes.append(player)
        # Let the private null sink leave its initial idle clock before
        # measuring capture. This affects only this owned synthetic server.
        time.sleep(2)
        probe = subprocess.Popen([str(args.probe.resolve()), '--system-output', '--packets', '250',
                                  '--expect-signal', '--drop-every', '20'], env=env,
                                 stdout=subprocess.PIPE, stderr=log('probe.err'), text=True)
        processes.append(probe)
        if not select.select([probe.stdout], [], [], 10)[0] or json.loads(probe.stdout.readline()) != {'ready': True}:
            raise RuntimeError('Owned monitor capture did not become ready')
        output = probe.communicate(timeout=15)[0]
        (out / 'probe.log').write_text(output)
        if probe.returncode:
            raise RuntimeError('Owned output-monitor audio probe failed')
        report = json.loads(output.strip())
        if (report.get('audio_capture') != 'passed' or report.get('rms', 0) <= .04
                or not 12 <= report.get('concealment_attempts', 0) <= 20
                or report.get('first_signal_packet', 999) > 12
                or report.get('signal_packets', 0) < report.get('decoded', 0) * .8
                or report.get('backend') != 'PulseAudio output monitor'):
            raise RuntimeError('Output-monitor signal/concealment checks failed')
        remaining = subprocess.check_output(['pactl', 'list', 'short', 'source-outputs'], env=env, text=True)
        if remaining.strip():
            raise RuntimeError('Owned audio capture stream remained open after probe exit')
        report.update(scope='owned null sink; unrelated default input', output_device_unchanged=True)
        (out / 'result.json').write_text(json.dumps(report, indent=2) + '\n')
        print(json.dumps(report))
    finally:
        for process in reversed(processes):
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=4)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=4)
        for stream in logs:
            stream.close()
        shutil.rmtree(runtime)


if __name__ == '__main__':
    main()
