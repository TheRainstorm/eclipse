#!/usr/bin/env python3
"""Verify native host input on an owned headless Sway with two scaled outputs."""
import argparse
import json
import os
from pathlib import Path
import select
import shutil
import subprocess
import tempfile
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--probe', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--portal-probe', type=Path, help='Also verify ScreenCast fallback and explicit input denial')
    args = parser.parse_args()
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    os.umask(0o077)
    runtime = Path(tempfile.mkdtemp(prefix='uu-wayland-input-'))
    env = os.environ.copy()
    for key in ('DISPLAY', 'WAYLAND_DISPLAY', 'SWAYSOCK', 'DBUS_SESSION_BUS_ADDRESS'):
        env.pop(key, None)
    env.update(XDG_RUNTIME_DIR=str(runtime), WLR_BACKENDS='headless', WLR_HEADLESS_OUTPUTS='2',
               WLR_LIBINPUT_NO_DEVICES='1', WLR_RENDERER='pixman', UU_OWNED_WAYLAND_TEST='1')
    config = out / 'sway.conf'
    config.write_text('output HEADLESS-1 mode 1200x800 position 0 0 scale 1\n'
                      'output HEADLESS-2 mode 1280x800 position 1200 0 scale 2\n'
                      'xwayland disable\nseat seat0 fallback true\nfocus_follows_mouse no\n'
                      'for_window [app_id="uu-wayland-input-probe"] fullscreen enable\n')
    # Foot provides a real Wayland surface and a PTY. Mouse reporting and raw
    # bytes prove OS delivery without accepting input from the user's session.
    observer = out / 'observer.py'
    observer.write_text('''import os,select,sys,termios,time,tty
from pathlib import Path
fd=0
old=termios.tcgetattr(fd)
tty.setraw(fd)
os.write(1,b"\\x1b[?1003h\\x1b[?1006h")
Path(sys.argv[1]+'.ready').touch()
data=bytearray()
try:
 deadline=time.monotonic()+12
 while time.monotonic()<deadline:
  if select.select([fd],[],[],.1)[0]: data.extend(os.read(fd,4096))
finally:
 Path(sys.argv[1]).write_bytes(data)
 termios.tcsetattr(fd,termios.TCSANOW,old)
''')
    sway = foot = probe = None
    logs = []

    def log(name):
        handle = (out / name).open('w')
        logs.append(handle)
        return handle

    def wait(check, seconds=5):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            value = check()
            if value:
                return value
            if sway.poll() is not None:
                raise RuntimeError('Owned Sway ended')
            time.sleep(.05)
        raise TimeoutError('Owned compositor condition timed out')

    def ipc(command):
        return json.loads(subprocess.check_output(['swaymsg', '-r', command], env=env, text=True))

    def command(line):
        probe.stdin.write(line+'\n')
        probe.stdin.flush()
        if not select.select([probe.stdout], [], [], 3)[0] or json.loads(probe.stdout.readline()) != {'ok': True}:
            raise RuntimeError('Owned native input failed: '+line)
        time.sleep(.035)

    try:
        sway = subprocess.Popen(['sway', '-c', str(config), '--unsupported-gpu'], env=env,
                                stdout=log('sway.log'), stderr=subprocess.STDOUT)
        def ready():
            sockets = list(runtime.glob('sway-ipc.*.sock'))
            displays = [p for p in runtime.glob('wayland-*') if not p.name.endswith('.lock')]
            if sockets and displays:
                env.update(SWAYSOCK=str(sockets[0]), WAYLAND_DISPLAY=displays[0].name)
                return True
        wait(ready, 10)
        ipc('focus output HEADLESS-2')
        captured = out / 'delivered.bin'
        foot = subprocess.Popen(['foot', '--app-id=uu-wayland-input-probe', '--', 'python3', str(observer), str(captured)],
                                env=env, stdout=log('foot.log'), stderr=subprocess.STDOUT)
        wait(lambda: Path(str(captured)+'.ready').exists())
        probe = subprocess.Popen([str(args.probe.resolve()), 'HEADLESS-2', '1280', '800'], env=env,
                                 stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=log('probe.log'), text=True)
        if not select.select([probe.stdout], [], [], 4)[0] or json.loads(probe.stdout.readline()) != {'ready': True}:
            raise RuntimeError('Native host input did not initialize')
        command('move 640 400')
        command('button left 1'); command('button left 0')
        command('key 225 1'); command('key 229 1')  # Both Shift keys
        command('key 225 0')
        command('key 4 1'); command('key 4 0')      # A, right Shift still held
        command('release')
        command('key 4 1'); command('key 4 0')      # a, all owned modifiers released
        command('key 224 1')                       # Ctrl+a, release on explicit reset
        command('key 4 1'); command('key 4 0'); command('release')
        command('key 4 1'); command('key 4 0')
        command('relative 30 20')
        command('scroll 120 -120')
        command('key 224 1')                       # Teardown owns this held Ctrl
        probe.stdin.write('quit\n'); probe.stdin.flush()
        probe.wait(timeout=3)
        assert probe.returncode == 0
        probe = subprocess.Popen([str(args.probe.resolve()), 'HEADLESS-2', '1280', '800'], env=env,
                                 stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=log('reconnect.log'), text=True)
        if not select.select([probe.stdout], [], [], 4)[0] or json.loads(probe.stdout.readline()) != {'ready': True}:
            raise RuntimeError('Native host input did not reconnect')
        command('key 4 1'); command('key 4 0')      # a, previous Ctrl was released
        probe.stdin.write('quit\n'); probe.stdin.flush()
        probe.wait(timeout=3)
        assert probe.returncode == 0
        foot.wait(timeout=15)
        data = captured.read_bytes()
        assert b'Aa\x01a' in data, repr(data)
        assert b'\x1b[<0;' in data and b'M' in data and b'm' in data, repr(data)
        # Events must reach the scaled second output. A whole-layout absolute
        # pointer maps x=640/1280 into output 1 and cannot produce these reports.
        assert b'\x1b[<35;' in data, repr(data)
        assert b'\x1b[<65;' in data and b'\x1b[<67;' in data, repr(data)
        import re
        assert re.sub(rb'\x1b\[<\d+;\d+;\d+[Mm]', b'', data) == b'Aa\x01aa', repr(data)
        result = dict(ok=True, real_wayland_delivery=True, outputs=2, selected_output_scale=2,
                      keyboard=True, simultaneous_shifts=True, reset_releases=True, reconnect_releases=True,
                      pointer_absolute=True, pointer_relative=True, buttons=True, two_axis_scroll=True,
                      no_user_desktop=True)
        if args.portal_probe:
            for case in ('stream-no-input', 'stream-input-denied'):
                completed = subprocess.run(['python3', str(Path(__file__).with_name('portal_smoke.py')),
                    '--probe', str(args.portal_probe.resolve()), '--case', case,
                    '--owned-compositor-output', 'HEADLESS-2'], env=env, capture_output=True, text=True, timeout=20)
                (out / (case + '.log')).write_text(completed.stdout+completed.stderr)
                assert completed.returncode == 0, completed.stdout+completed.stderr
            result.update(portal_fallback=True, portal_denial_respected=True)
        (out / 'result.json').write_text(json.dumps(result, indent=2)+'\n')
        print(json.dumps(result))
    finally:
        for process in (probe, foot, sway):
            if process and process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    process.kill(); process.wait(timeout=3)
        for handle in logs:
            handle.close()
        shutil.rmtree(runtime, ignore_errors=True)


if __name__ == '__main__':
    main()
