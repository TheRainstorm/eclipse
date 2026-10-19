#!/usr/bin/env python3
"""Prove Opus playback reaches an owned PipeWire null sink, without recording user audio."""
import argparse
import json
import math
import os
from pathlib import Path
import struct
import subprocess
import time
import wave


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--probe', type=Path, required=True)
    p.add_argument('--output', type=Path, required=True)
    a = p.parse_args()
    os.umask(0o077)
    a.output.mkdir(parents=True, exist_ok=False)
    name = f'uu_rust_test_{os.getpid()}'
    env = os.environ.copy()
    env.setdefault('XDG_RUNTIME_DIR', f'/run/user/{os.getuid()}')
    env.setdefault('DBUS_SESSION_BUS_ADDRESS', f'unix:path=/run/user/{os.getuid()}/bus')
    module = subprocess.check_output(['pactl', 'load-module', 'module-null-sink', f'sink_name={name}', 'rate=48000', 'channels=2'], env=env, text=True).strip()
    recorder = None
    result = {}
    try:
        config = a.output / 'alsa.conf'
        config.write_text(f'</usr/share/alsa/alsa.conf>\npcm.!default {{\n type pipewire\n server "pipewire-0"\n playback_node "{name}"\n capture_node "-1"\n}}\n')
        env['ALSA_CONFIG_PATH'] = str(config.resolve())
        record = a.output / 'monitor.wav'
        with (a.output / 'record.log').open('w') as log:
            recorder = subprocess.Popen(['ffmpeg', '-nostdin', '-y', '-f', 'pulse', '-i', f'{name}.monitor', '-t', '5', '-ar', '48000', '-ac', '2', '-c:a', 'pcm_s16le', str(record)], env=env, stdout=log, stderr=subprocess.STDOUT)
            time.sleep(.6)
            probe = subprocess.run([str(a.probe.resolve())], env=env, capture_output=True, text=True, timeout=15)
            (a.output / 'probe.log').write_text(probe.stdout + probe.stderr)
            recorder.wait(timeout=10)
        with wave.open(str(record)) as wav:
            assert wav.getsampwidth() == 2
            values = struct.unpack('<' + 'h' * (wav.getnframes() * wav.getnchannels()), wav.readframes(wav.getnframes()))
        samples = values[::2]
        rms = math.sqrt(sum(v * v for v in samples) / max(1, len(samples))) / 32768
        tone = abs(sum(complex(math.cos(i * math.tau * 440 / 48000), -math.sin(i * math.tau * 440 / 48000)) * v / 32768 for i, v in enumerate(samples))) / max(1, len(samples))
        result.update(probe_exit=probe.returncode, rms=round(rms, 5), tone_440hz=round(tone, 5), frames=len(samples))
        if probe.returncode or rms < .02 or tone < .01:
            raise RuntimeError('No valid Opus test tone reached the private playback device')
    except Exception as error:
        result['error'] = str(error)
    finally:
        if recorder and recorder.poll() is None:
            recorder.terminate(); recorder.wait(timeout=5)
        subprocess.run(['pactl', 'unload-module', module], env=env, check=True)
        (a.output / 'result.json').write_text(json.dumps(result, indent=2))
        print(json.dumps(result))
    return 1 if 'error' in result else 0

if __name__ == '__main__':
    raise SystemExit(main())
