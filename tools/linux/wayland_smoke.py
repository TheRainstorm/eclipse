#!/usr/bin/env python3
"""Run an owned headless Wayland compositor; never inject into the user's desktop."""
import argparse
import json
import os
from pathlib import Path
import shutil
import select
import subprocess
import tempfile
import time


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--binary', type=Path, required=True)
    ap.add_argument('--state', type=Path, required=True, help='Existing private uu-remote/state.json; never printed')
    ap.add_argument('--device', required=True)
    ap.add_argument('--input-driver', type=Path, required=True)
    ap.add_argument('--renderer', choices=['gles2', 'pixman'], default='gles2')
    ap.add_argument('--clipboard-probe', type=Path)
    ap.add_argument('--keyboard-fixture', action='store_true', help='Type fixed text ONLY when a dedicated remote test text box is active')
    ap.add_argument('--output', type=Path, required=True)
    args = ap.parse_args()
    os.umask(0o077)
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    binary = out / 'uu-gui'
    shutil.copy2(args.binary, binary)
    runtime = Path(tempfile.mkdtemp(prefix='uu-wayland-', dir='/tmp'))
    data = out / 'data' / 'uu-remote'
    data.mkdir(parents=True)
    shutil.copyfile(args.state, data / 'state.json')
    (data / 'gui-settings.json').write_text(json.dumps({'display_mode': 'preserve', 'fps': 60, 'muted': True, 'clipboard': False, 'hud': True}))
    config = out / 'sway.conf'
    marker = out / 'compositor-shortcut'
    config.write_text(f'bindsym Mod4+F23 exec touch {marker}\n' + 'output HEADLESS-1 mode 1600x1000@60Hz\nxwayland disable\nseat seat0 fallback true\ndefault_border none\nfocus_follows_mouse no\nfor_window [app_id=".*"] floating enable\n')
    env = os.environ.copy()
    for key in ['DISPLAY', 'WAYLAND_DISPLAY', 'SWAYSOCK', 'UU_AUTOCONNECT', 'UU_FORCE_JOIN']:
        env.pop(key, None)
    env.update(XDG_RUNTIME_DIR=str(runtime), WLR_BACKENDS='headless', WLR_HEADLESS_OUTPUTS='1', WLR_LIBINPUT_NO_DEVICES='1', WLR_RENDERER=args.renderer, WLR_RENDER_DRM_DEVICE='/dev/dri/renderD128', PIPEWIRE_RUNTIME_DIR=f'/run/user/{os.getuid()}', XDG_DATA_HOME=str(out / 'data'), UU_DECODER='hw', UU_UI_DEBUG='1')
    result = {'phases': []}
    sway = gui = keyboard = None
    files = []
    def log(name):
        f = (out / name).open('w'); files.append(f); return f
    def ipc(command):
        response = subprocess.check_output(['swaymsg', '-r', command], env=env, text=True)
        values = json.loads(response)
        if isinstance(values, list) and any(not v.get('success', False) for v in values):
            raise RuntimeError(f'Sway command failed: {command}: {response}')
        return values
    def windows(node):
        yield node
        for child in node.get('nodes', []) + node.get('floating_nodes', []):
            yield from windows(child)
    def window():
        tree = json.loads(subprocess.check_output(['swaymsg', '-r', '-t', 'get_tree'], env=env, text=True))
        return next((n for n in windows(tree) if n.get('pid') == gui.pid), None)
    def wait_for(check, seconds=15):
        until = time.monotonic() + seconds
        while time.monotonic() < until:
            if gui and gui.poll() is not None:
                raise RuntimeError(f'GUI exited unexpectedly: {gui.returncode}')
            value = check()
            if value: return value
            time.sleep(.1)
        raise TimeoutError('Test condition did not complete')
    def driver(request):
        keyboard.stdin.write(json.dumps(request) + '\n'); keyboard.stdin.flush()
        if not select.select([keyboard.stdout], [], [], 8)[0]: raise TimeoutError('Virtual input driver timed out')
        if json.loads(keyboard.stdout.readline()) != {'ok': True}: raise RuntimeError('Virtual input failed')
    def keys(key):
        driver({'mods': ['ctrl', 'alt'], 'key': key})
    def screenshot(name):
        subprocess.run(['grim', '-o', 'HEADLESS-1', str(out / f'{name}.png')], env=env, check=True)
    try:
        sway = subprocess.Popen(['sway', '-c', str(config), '--unsupported-gpu'], env=env, stdout=log('sway.log'), stderr=subprocess.STDOUT)
        until = time.monotonic() + 10
        while time.monotonic() < until:
            sockets = list(runtime.glob('sway-ipc.*.sock'))
            displays = [p for p in runtime.glob('wayland-*') if not p.name.endswith('.lock')]
            if sockets and displays:
                env.update(SWAYSOCK=str(sockets[0]), WAYLAND_DISPLAY=displays[0].name)
                break
            if sway.poll() is not None: raise RuntimeError('Private Sway failed to start')
            time.sleep(.1)
        else: raise TimeoutError('Private compositor socket missing')
        # Persistent devices preserve focus and a standard physical US keymap.
        keyboard = subprocess.Popen([str(args.input_driver.resolve())], env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=log('keyboard.log'), text=True)
        if not select.select([keyboard.stdout], [], [], 10)[0] or json.loads(keyboard.stdout.readline()) != {'ready': True}:
            raise RuntimeError('Virtual input driver did not start')
        if args.clipboard_probe:
            probe = subprocess.run([str(args.clipboard_probe.resolve())], env=env, capture_output=True, text=True, timeout=20)
            (out / 'clipboard.log').write_text(probe.stdout + probe.stderr)
            if probe.returncode: raise RuntimeError('Isolated clipboard probe failed')
            result['phases'].append('wayland-clipboard')
        gui = subprocess.Popen([str(binary), '--stream-device', args.device, '--stream-alias', 'Linux 自动测试'], cwd=out, env=env, stdout=log('stdout.log'), stderr=log('stderr.log'))
        node = wait_for(window)
        ipc(f'[pid={gui.pid}] focus')
        def events():
            paths = list(out.glob('uu-session-*.jsonl'))
            result = []
            for path in paths:
                for line in path.read_text().splitlines():
                    try: result.append(json.loads(line))
                    except json.JSONDecodeError: pass
            return result
        wait_for(lambda: any(e.get('event') == 'first_frame' for e in events()), 35)
        wait_for(lambda: any(e.get('event') == 'transport' and e.get('control') == 'open' for e in events()), 20)
        time.sleep(1)
        screenshot('stream-window')
        result['phases'].append('native-wayland-stream')
        keys('s'); time.sleep(.3); screenshot('hud-toggle')
        wait_for(lambda: not json.loads((data / 'gui-settings.json').read_text()).get('hud', True))
        keys('s')
        wait_for(lambda: json.loads((data / 'gui-settings.json').read_text()).get('hud', False))
        keys('x'); wait_for(lambda: window().get('fullscreen_mode') == 1)
        screenshot('fullscreen')
        time.sleep(.5)
        keys('z')
        def capture_active():
            values = [e for e in events() if e.get('event') == 'input_focus']
            return values[-1].get('active') if values else None
        wait_for(lambda: capture_active() is not None)
        if not capture_active(): keys('z')
        wait_for(capture_active)
        time.sleep(.3)
        if args.keyboard_fixture:
            driver({'key': 'F22'})
            time.sleep(.5)
            driver({'text': 'aA;:,.?/[]{}+-_'})
            for key in ['c', 'v', 'x']:
                driver({'mods': ['ctrl'], 'key': key})
            result['phases'].append('remote-keyboard-fixture-sent')
        driver({'mods': ['logo'], 'key': 'F23'})
        time.sleep(.5)
        if marker.exists(): raise RuntimeError('Fullscreen system shortcut escaped to compositor')
        keys('f'); wait_for(lambda: window().get('fullscreen_mode') == 0)
        time.sleep(.5)
        driver({'mods': ['logo'], 'key': 'F23'})
        wait_for(marker.exists)
        result['phases'].append('fullscreen-shortcuts-and-release')
        node = window(); rect = node['rect']
        # Current stream title bar: first and second real display tabs.
        for x in [200, 340, 200, 340]:
            driver({'x': rect['x'] + x, 'y': rect['y'] + 18})
            time.sleep(.8)
        screenshot('display-switch')
        result['display_ready'] = [e.get('elapsed_ms') for e in events() if e.get('event') == 'display_ready']
        result['video'] = [e for e in events() if e.get('event') == 'first_frame'][-1:]
        result['timeouts'] = sum(e.get('event') == 'switch_timeout' for e in events())
        if len(result['display_ready']) < 4: raise RuntimeError('Display tabs did not complete four switches')
        keys('q')
        # Exit ends the stream; its dedicated process then closes itself.
        gui.wait(timeout=12)
        result.update(exit_code=gui.returncode, graceful_close=gui.returncode == 0)
        if gui.returncode != 0 or result['timeouts']: raise RuntimeError('GUI failed smoke validation')
    except Exception as error:
        result['error'] = str(error)
    finally:
        if gui and gui.poll() is None:
            try: ipc(f'[pid={gui.pid}] kill'); gui.wait(timeout=8)
            except Exception: gui.terminate(); gui.wait(timeout=8)
        if keyboard and keyboard.poll() is None:
            keyboard.terminate(); keyboard.wait(timeout=5)
        if sway and sway.poll() is None:
            sway.terminate()
            try: sway.wait(timeout=5)
            except subprocess.TimeoutExpired: sway.kill(); sway.wait()
        for f in files: f.close()
        # Credentials only live for the duration of this owned session.
        (data / 'state.json').unlink(missing_ok=True)
        shutil.rmtree(runtime)
        (out / 'result.json').write_text(json.dumps(result, ensure_ascii=False, indent=2))
        print(json.dumps(result, ensure_ascii=False))
    return 1 if 'error' in result else 0

if __name__ == '__main__':
    raise SystemExit(main())
