#!/usr/bin/env python3
"""Exercise two real CLI processes over authenticated, encrypted localhost video."""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import time


def run(executable, frames, decoder, audio=False, encoder='software'):
    with tempfile.TemporaryDirectory(prefix='uu-native-smoke-') as temporary:
        temporary = Path(temporary)
        invitation = temporary / 'session.eclipse-invite.json'
        environment = os.environ.copy()
        environment.update(XDG_DATA_HOME=str(temporary / 'data'),
                           XDG_STATE_HOME=str(temporary / 'state'),
                           UU_SIGNING_KEY_FILE=str(temporary / 'missing-signing-key'),
                           RUST_LOG='error')
        if decoder == 'sw':
            environment['UU_DECODER'] = 'sw'
        else:
            environment.pop('UU_DECODER', None)
        with (temporary / 'host.log').open('w+', encoding='utf-8') as log:
            host = subprocess.Popen([str(executable), 'native-host', '--synthetic', *(['--synthetic-audio'] if audio else []),
                                     *(['--encoder', encoder] if encoder != 'software' else []),
                                     '--invitation-file', str(invitation), '--max-seconds', '30'],
                                    env=environment, stdout=log, stderr=subprocess.STDOUT)
            try:
                deadline = time.monotonic() + 15
                while not invitation.exists():
                    if host.poll() is not None:
                        raise RuntimeError('Native host ended before creating its invitation')
                    if time.monotonic() > deadline:
                        raise TimeoutError('Native invitation was not ready')
                    time.sleep(.02)
                probe = subprocess.run([str(executable), 'native-probe', '--invitation-file',
                                        str(invitation), '--frames', str(frames), '--timeout-seconds', '25',
                                        '--report', str(temporary / 'probe.json'), *(['--audio-loss-every', '5'] if audio else [])],
                                       env=environment, capture_output=True, text=True,
                                       encoding='utf-8', errors='replace', timeout=40)
                if probe.returncode:
                    metrics = '\n'.join(line for line in probe.stdout.splitlines() if line.startswith('native probe:'))
                    raise RuntimeError('Native viewing process failed: ' + metrics + '\n' + probe.stderr[-4000:])
                lines = [line for line in probe.stdout.splitlines() if line.startswith('native probe:')]
                if len(lines) != 1:
                    raise RuntimeError('Native probe did not report one completed video session')
                report_path = temporary / 'probe.json'
                report = json.loads(report_path.read_text())
                if (not report.get('completed') or report['final']['decoded'] < frames
                        or report['final']['errors'] or not report['samples']
                        or report['first_frame_ms'] is None or report['observed'] <= 0):
                    raise RuntimeError('Native probe report did not match a completed, error-free video session')
                if audio and (report['final']['audio_samples'] < frames * 300 or report['final']['audio_injected_loss'] < 2 or report['final']['audio_recovered'] < report['final']['audio_injected_loss'] - 2):
                    raise RuntimeError('Native audio redundancy did not recover the injected packet loss')
                if os.name == 'posix' and report_path.stat().st_mode & 0o077:
                    raise RuntimeError('Native probe report was not private')
                metric = re.search(r'decoded=(\d+).*resolution=(\d+)x(\d+).*errors=(\d+)', lines[0])
                if not metric or int(metric[1]) < frames or (metric[2], metric[3], metric[4]) != ('1280', '720', '0'):
                    raise RuntimeError('Native video failed resolution, frame-count or decode-error checks')
                host.wait(timeout=8)
                if host.returncode:
                    raise RuntimeError('Native host failed to shut down after its viewer disconnected')
                log.seek(0)
                actual_encoder = re.search(r'encoder="([^"\r\n]+)"', log.read())
                actual_encoder = actual_encoder[1] if actual_encoder else None
                if encoder == 'vaapi' and (not actual_encoder or not actual_encoder.startswith('vaapi (')):
                    raise RuntimeError('Explicit VA-API host did not report hardware encoding')
                if list((temporary / 'data').rglob('state.json')):
                    raise RuntimeError('Native smoke test unexpectedly created an interoperability account')
                print(json.dumps(dict(native_video='passed', service_credentials_supplied=False, account='absent',
                                      clean_shutdown=True, audio_redundancy=audio, encoder_backend=actual_encoder,
                                      decoder_backend=report['final']['backend'], metrics=lines[0])))
            finally:
                if host.poll() is None:
                    host.terminate()
                    try:
                        host.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        host.kill()
                        host.wait(timeout=5)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--exe', type=Path, required=True)
    parser.add_argument('--frames', type=int, default=120)
    parser.add_argument('--decoder', choices=('sw', 'auto'), default='sw',
                        help='Use deterministic software decoding in CI; auto exercises the actual machine decoder')
    parser.add_argument('--audio', action='store_true', help='Check generated encrypted audio and receive-only packet loss; never plays sound')
    parser.add_argument('--encoder', choices=('software', 'auto', 'vaapi'), default='software',
                        help='Native host preference; explicit VA-API requires a reported hardware encoder')
    args = parser.parse_args()
    if not 1 <= args.frames <= 600:
        parser.error('--frames must be 1..=600')
    run(args.exe.resolve(strict=True), args.frames, args.decoder, args.audio, args.encoder)


if __name__ == '__main__':
    main()
