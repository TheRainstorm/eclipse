#!/usr/bin/env python3
"""Native viewer checks in an owned headless Sway; never inject into the user's desktop."""
import argparse
import json
import os
from pathlib import Path
import select
import shutil
import subprocess
import sys
import tempfile
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--gui', type=Path, required=True)
    parser.add_argument('--cli', type=Path, required=True)
    parser.add_argument('--input-driver', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--renderer', choices=('gles2', 'pixman'), default='gles2')
    parser.add_argument('--decoder', choices=('sw', 'auto', 'hw'), default='auto')
    parser.add_argument('--multi-dpi', action='store_true')
    parser.add_argument('--expect-hardware', action='store_true')
    args = parser.parse_args()
    os.umask(0o077)
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    runtime = Path(tempfile.mkdtemp(prefix='uu-native-wayland-'))
    env = os.environ.copy()
    for name in ('DISPLAY', 'WAYLAND_DISPLAY', 'SWAYSOCK', 'DBUS_SESSION_BUS_ADDRESS', 'UU_AUTOCONNECT', 'UU_FORCE_JOIN'):
        env.pop(name, None)
    env.update(XDG_RUNTIME_DIR=str(runtime), XDG_DATA_HOME=str(out / 'data'),
               XDG_STATE_HOME=str(out / 'state'), UU_SIGNING_KEY_FILE=str(out / 'absent-signing-key'),
               WLR_BACKENDS='headless', WLR_HEADLESS_OUTPUTS='2' if args.multi_dpi else '1',
               WLR_LIBINPUT_NO_DEVICES='1', WLR_RENDERER=args.renderer,
               WLR_RENDER_DRM_DEVICE='/dev/dri/renderD128',
               UU_DECODER='hw' if args.expect_hardware else args.decoder, RUST_LOG='error')
    data = out / 'data' / 'uu-remote'
    data.mkdir(parents=True)
    (data / 'gui-settings.json').write_text(json.dumps(dict(hud=True, clipboard=False, muted=True)))
    marker = out / 'compositor-shortcut'
    config = out / 'sway.conf'
    config.write_text(f'bindsym Mod4+c exec touch {marker}\n'
                      'output HEADLESS-1 mode 1600x1000@60Hz position 0 0 scale 1\n'
                      'output HEADLESS-2 mode 1280x800@60Hz position 1600 0 scale 1.5\n'
                      'xwayland disable\nseat seat0 fallback true\ndefault_border none\n'
                      'focus_follows_mouse no\nfor_window [app_id=".*"] floating enable\n')
    logs = []
    sway = host = gui = keyboard = None
    window_id = None

    def log(name):
        file = (out / name).open('w')
        logs.append(file)
        return file

    def wait(check, seconds=15):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            if gui and gui.poll() is not None:
                raise RuntimeError('Native GUI ended unexpectedly')
            value = check()
            if value:
                return value
            time.sleep(.1)
        raise TimeoutError('Owned native Wayland condition timed out')

    def ipc(command, kind=None):
        command = ['swaymsg', '-r'] + (['-t', kind] if kind else [command])
        value = json.loads(subprocess.check_output(command, env=env, text=True))
        if isinstance(value, list) and any(not item.get('success', True) for item in value):
            raise RuntimeError('Owned compositor command failed')
        return value

    def nodes(node):
        yield node
        for child in node.get('nodes', []) + node.get('floating_nodes', []):
            yield from nodes(child)

    def window():
        nonlocal window_id
        owned = [node for node in nodes(ipc('', 'get_tree')) if node.get('pid') == gui.pid]
        if window_id is None:
            # A stream workspace also owns a hidden coordinating root. Pin the
            # mapped stream surface, then retain its compositor identity across
            # fullscreen and output changes, even while visibility transitions.
            mapped = [node for node in owned if node.get('visible')]
            chosen = next((node for node in mapped if node.get('focused')), None)
            if chosen is None and len(mapped) == 1:
                chosen = mapped[0]
            if chosen is not None:
                window_id = chosen['id']
        return next((node for node in owned if node['id'] == window_id), None)

    def window_command(command):
        ipc(f'[con_id="{window_id}"] {command}')

    def keys(key, mods=('ctrl', 'alt')):
        keyboard.stdin.write(json.dumps(dict(mods=list(mods), key=key)) + '\n')
        keyboard.stdin.flush()
        if not select.select([keyboard.stdout], [], [], 8)[0] or json.loads(keyboard.stdout.readline()) != {'ok': True}:
            raise RuntimeError('Owned virtual keyboard failed')

    def events():
        result = []
        for path in (out / 'state' / 'uu-remote' / 'diagnostics').glob('uu-session-*.jsonl'):
            for line in path.read_text().splitlines():
                try:
                    result.append(json.loads(line))
                except ValueError:
                    pass
        return result

    def screenshot(name, output='HEADLESS-1'):
        subprocess.run(['grim', '-o', output, str(out / (name + '.png'))], env=env, check=True)

    try:
        sway = subprocess.Popen(['sway', '-c', str(config), '--unsupported-gpu'], env=env, stdout=log('sway.log'), stderr=subprocess.STDOUT)
        def compositor():
            sockets = list(runtime.glob('sway-ipc.*.sock'))
            displays = [path for path in runtime.glob('wayland-*') if not path.name.endswith('.lock')]
            if sockets and displays:
                env.update(SWAYSOCK=str(sockets[0]), WAYLAND_DISPLAY=displays[0].name)
                return True
            if sway.poll() is not None:
                raise RuntimeError('Owned compositor failed to start')
        wait(compositor, 10)
        ipc('focus output HEADLESS-1')
        keyboard = subprocess.Popen([str(args.input_driver.resolve())], env=env, stdin=subprocess.PIPE,
                                    stdout=subprocess.PIPE, stderr=log('keyboard.log'), text=True)
        if not select.select([keyboard.stdout], [], [], 10)[0] or json.loads(keyboard.stdout.readline()) != {'ready': True}:
            raise RuntimeError('Owned virtual input driver did not become ready')
        invitation = out / 'session.eclipse-invite.json'
        host = subprocess.Popen([str(args.cli.resolve()), 'native-host', '--synthetic', '--invitation-file', str(invitation), '--max-seconds', '90'], env=env, stdout=log('host.log'), stderr=subprocess.STDOUT)
        wait(invitation.exists)
        gui = subprocess.Popen([str(args.gui.resolve()), '--native-invitation', str(invitation)], env=env, stdout=log('gui.log'), stderr=subprocess.STDOUT)
        wait(window)
        wait(lambda: any(event.get('event') == 'first_frame' for event in events()))
        wait(lambda: window().get('focused'))
        screenshot('window')
        keys('s'); keys('s')
        if not json.loads((data / 'gui-settings.json').read_text()).get('muted'):
            raise RuntimeError('A native viewing shortcut overwrote the saved audio preference')
        keys('x')
        wait(lambda: window().get('fullscreen_mode') == 1)
        screenshot('fullscreen')
        keys('c', ('logo',))
        wait(marker.exists)
        keys('x')
        wait(lambda: window().get('fullscreen_mode') == 0)
        result = dict(native_viewer=True, fullscreen=True, local_system_shortcut_in_readonly=True)
        if args.multi_dpi:
            window_command('move container to output HEADLESS-2')
            outputs = ipc('', 'get_outputs')
            area = next(output['rect'] for output in outputs if output['name'] == 'HEADLESS-2')
            (out / 'dpi-output.json').write_text(json.dumps(outputs, indent=2))
            try:
                wait(lambda: window()['rect']['width'] <= area['width'] * .9
                     and window()['rect']['height'] <= area['height'] * .9)
            except TimeoutError:
                (out / 'dpi-failed-window.json').write_text(json.dumps(window(), indent=2))
                screenshot('dpi-failed', 'HEADLESS-2')
                raise
            result['cross_dpi_before_compositor_center'] = window()['rect']
            # Wayland clients cannot set global positions. The owned compositor
            # chooses placement, independently of the application's size policy.
            window_command('move position center')
            result['cross_dpi_rect'] = window()['rect']
            screenshot('cross-dpi', 'HEADLESS-2')
            # Moving a container to another output does not focus that output.
            # Keep the final shortcut inside this test's own focused window.
            window_command('focus')
            wait(lambda: window().get('focused'))
            keys('x')
            wait(lambda: window().get('fullscreen_mode') == 1)
            keys('x')
            wait(lambda: window().get('fullscreen_mode') == 0)
            window_command('move container to output HEADLESS-1')
            wait(lambda: window()['rect']['width'] > area['width'] * .9
                 and window()['rect']['width'] <= 1600 * .9
                 and window()['rect']['height'] <= 1000 * .9)
            result['cross_dpi_return_rect'] = window()['rect']
            window_command('move position center, focus')
            wait(lambda: window().get('focused'))
        sample = wait(lambda: next((event for event in reversed(events()) if event.get('event') == 'video_stats' and event.get('decoded', 0) >= 120), None))
        if sample['errors'] or sample['input_sent']:
            raise RuntimeError('Native readonly viewing had decode errors or forwarded input')
        if args.expect_hardware and not sample['backend'].lower().startswith('vaapi ('):
            raise RuntimeError('Native viewer did not remain on VAAPI')
        result['stats'] = sample
        keys('q')
        gui.wait(timeout=6)
        host.wait(timeout=6)
        if gui.returncode or host.returncode:
            raise RuntimeError('Native viewing did not close cleanly')
        result['clean_exit'] = True
        (out / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
        print(json.dumps(result))
    except Exception:
        # Retain the failing compositor's diagnostics in CI without emitting
        # invitation contents, account state, or the host's authenticated data.
        if sway and sway.poll() is None and env.get('SWAYSOCK'):
            try:
                tree = ipc('', 'get_tree')
                owned = [node for node in nodes(tree) if gui and node.get('pid') == gui.pid]
                print('Owned Wayland windows: ' + json.dumps(owned), file=sys.stderr)
                (out / 'failed-windows.json').write_text(json.dumps(owned, indent=2))
            except (OSError, ValueError, subprocess.SubprocessError):
                pass
        for name in ('sway.log', 'keyboard.log', 'gui.log'):
            path = out / name
            if path.exists():
                print(f'Owned Wayland test {name}:\n{path.read_text(errors="replace")[-6000:]}', file=sys.stderr)
        raise
    finally:
        for process in (gui, host, keyboard, sway):
            if process and process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill(); process.wait(timeout=5)
        (out / 'session.eclipse-invite.json').unlink(missing_ok=True)
        shutil.rmtree(runtime)
        for file in logs:
            file.close()


if __name__ == '__main__':
    main()
