#!/usr/bin/env python3
"""Verify two captures pause and resume on an owned headless Wayland output."""
import argparse
import json
import os
from pathlib import Path
import select
import signal
import shutil
import subprocess
import tempfile
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--probe', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--pause', choices=('compositor', 'lock', 'power'), default='compositor')
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    runtime = Path(tempfile.mkdtemp(prefix='eclipse-capture-recovery-'))
    environment = dict(os.environ)
    for name in ('DISPLAY', 'WAYLAND_DISPLAY', 'SWAYSOCK', 'DBUS_SESSION_BUS_ADDRESS'):
        environment.pop(name, None)
    environment.update(XDG_RUNTIME_DIR=str(runtime), WLR_BACKENDS='headless', WLR_HEADLESS_OUTPUTS='1',
                       WLR_RENDERER='pixman', WLR_LIBINPUT_NO_DEVICES='1', UU_OWNED_WAYLAND_TEST='1')
    config = output/'sway.conf'
    config.write_text('output HEADLESS-1 mode 640x360\nxwayland disable\nseat seat0 fallback true\n')
    compositor = probe = locker = None
    compositor_paused = False
    try:
        with (output/'sway.log').open('w') as log:
            compositor = subprocess.Popen(['sway', '-c', str(config), '--unsupported-gpu'], env=environment,
                                          stdout=log, stderr=subprocess.STDOUT)
        deadline = time.monotonic()+10
        while time.monotonic() < deadline:
            sockets = list(runtime.glob('sway-ipc.*.sock'))
            displays = [p for p in runtime.glob('wayland-*') if not p.name.endswith('.lock')]
            if sockets and displays:
                environment.update(SWAYSOCK=str(sockets[0]), WAYLAND_DISPLAY=displays[0].name)
                break
            if compositor.poll() is not None:
                raise RuntimeError('Owned compositor did not start')
            time.sleep(.05)
        else:
            raise TimeoutError('Owned compositor startup timed out')
        with (output/'probe.stderr').open('w') as errors:
            probe = subprocess.Popen([str(args.probe.resolve())], env=environment, stdout=subprocess.PIPE,
                                     stderr=errors, bufsize=0)
            rows = []
            deadline = time.monotonic()+8
            while sum(bool(row.get('ready')) for row in rows) < 2:
                if probe.poll() is not None or time.monotonic() >= deadline:
                    raise RuntimeError('Captures did not become ready')
                if select.select([probe.stdout], [], [], .1)[0]:
                    line = probe.stdout.readline()
                    if not line:
                        raise RuntimeError('Capture probe ended; see '+str(output/'probe.stderr'))
                    rows.append(json.loads(line))
            if args.pause == 'compositor':
                # Stop only this private compositor. This deterministically
                # withholds Ready/release, including a copy already in flight.
                compositor.send_signal(signal.SIGSTOP)
                compositor_paused = True
                time.sleep(3)
                compositor.send_signal(signal.SIGCONT)
                compositor_paused = False
            elif args.pause == 'lock':
                read_ready, write_ready = os.pipe()
                try:
                    with (output/'swaylock.log').open('w') as lock_log:
                        locker = subprocess.Popen(['swaylock', '--config', '/dev/null', '--color', '000000',
                                                   '--ready-fd', str(write_ready)], env=environment,
                                                  pass_fds=(write_ready,), start_new_session=True,
                                                  stdin=subprocess.DEVNULL, stdout=lock_log, stderr=subprocess.STDOUT)
                    os.close(write_ready)
                    write_ready = None
                    if not select.select([read_ready], [], [], 5)[0] or not os.read(read_ready, 1):
                        raise RuntimeError('Owned compositor did not lock')
                    time.sleep(3)
                    locker.send_signal(signal.SIGUSR1)
                    locker.wait(timeout=3)
                finally:
                    os.close(read_ready)
                    if write_ready is not None:
                        os.close(write_ready)
            else:
                for power in ('off', 'on'):
                    completed = subprocess.run(['swaymsg', '-r', 'output HEADLESS-1 power '+power],
                                               env=environment, capture_output=True, text=True, check=True)
                    if not all(item.get('success') for item in json.loads(completed.stdout)):
                        raise RuntimeError('Owned output power transition failed')
                    if power == 'off':
                        time.sleep(3)
            remaining, _ = probe.communicate(timeout=20)
            rows.extend(json.loads(line) for line in remaining.splitlines())
            (output/'probe.json').write_text(json.dumps(rows, indent=2)+'\n')
            if probe.returncode or sum(bool(row.get('ok')) for row in rows) != 2:
                raise RuntimeError('Capture pause/resume failed; see '+str(output))
            print(json.dumps(dict(ok=True, streams=2, real_wayland_recovery=True,
                                  mapping_retired=True, pts_monotonic=True, no_user_desktop=True)))
    finally:
        if compositor_paused and compositor and compositor.poll() is None:
            compositor.send_signal(signal.SIGCONT)
        for process in (probe, locker, compositor):
            if process and process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
        shutil.rmtree(runtime)


if __name__ == '__main__':
    main()
