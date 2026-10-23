#!/usr/bin/env python3
"""Measure a real wlr portal on an owned headless Sway, never a user desktop."""
import argparse
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import time


def inside(args):
    runtime = Path(os.environ['XDG_RUNTIME_DIR'])
    if (not runtime.name.startswith('uu-wayland-input-') or
            os.environ.get('UU_OWNED_WAYLAND_TEST') != '1'):
        raise RuntimeError('Requires the owned compositor fixture')
    env = os.environ.copy()
    processes, logs = [], []

    def start(name, command):
        log = (args.output / (name + '.log')).open('w')
        logs.append(log)
        process = subprocess.Popen(command, env=env, stdout=log, stderr=subprocess.STDOUT)
        processes.append(process)
        return process

    def wait(check, seconds=8):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            result = check()
            if result:
                return result
            if any(p.poll() is not None for p in processes):
                raise RuntimeError('Owned portal fixture process ended; inspect its logs')
            time.sleep(.04)
        raise TimeoutError('Owned portal fixture did not become ready')

    def owns(name):
        r = subprocess.run(['gdbus', 'call', '--session', '--dest', 'org.freedesktop.DBus',
            '--object-path', '/org/freedesktop/DBus', '--method',
            'org.freedesktop.DBus.NameHasOwner', name], env=env, capture_output=True,
            text=True, timeout=2)
        return r.returncode == 0 and 'true' in r.stdout

    try:
        config = runtime / 'sway.conf'
        config.write_text('output HEADLESS-1 mode 320x240 position 0 0 scale 1\n'
                          'output HEADLESS-2 mode 1280x800 position 320 0 scale 2\n'
                          'for_window [app_id="uu-portal-generated"] fullscreen enable\n'
                          'xwayland disable\nseat seat0 fallback true\n')
        start('sway', ['sway', '--unsupported-gpu', '-c', str(config)])

        def compositor():
            sockets = list(runtime.glob('sway-ipc.*.sock'))
            displays = [p for p in runtime.glob('wayland-*') if not p.name.endswith('.lock')]
            if sockets and displays:
                env.update(SWAYSOCK=str(sockets[0]), WAYLAND_DISPLAY=displays[0].name)
                return True
        wait(compositor)
        (args.output / 'outputs.json').write_text(subprocess.check_output(
            ['swaymsg', '-r', '-t', 'get_outputs'], env=env, text=True))
        subprocess.run(['swaymsg', 'focus output HEADLESS-2'], env=env,
                       capture_output=True, check=True, timeout=2)
        # Keep one owned surface alive. Replacing swaybg changes the pixman
        # renderer's capture constraints and tests backend reconfiguration
        # rather than steady screen sharing.
        start('generated-surface', ['foot', '--app-id=uu-portal-generated', '--',
            sys.executable, '-u', '-c',
            'import sys,time\nfor i in range(1000):\n'
            ' sys.stdout.write("\\x1b[48;5;"+str(24+i%2)+"m\\x1b[2J\\x1b[Hgenerated portal fixture")\n'
            ' sys.stdout.flush();time.sleep(.1)'])
        (runtime / 'client.conf').write_text('''
context.spa-libs = { support.* = support/libspa-support }
context.modules = [
 { name = libpipewire-module-protocol-native }
 { name = libpipewire-module-client-node }
 { name = libpipewire-module-client-device }
 { name = libpipewire-module-adapter }
 { name = libpipewire-module-metadata }
]
''')
        (runtime / 'pipewire.conf').write_text('''
context.properties = { core.daemon = true core.name = pipewire-0 support.dbus = false }
context.spa-libs = { support.* = support/libspa-support }
context.modules = [
 { name = libpipewire-module-protocol-native }
 { name = libpipewire-module-metadata }
 { name = libpipewire-module-spa-node-factory }
 { name = libpipewire-module-client-node }
 { name = libpipewire-module-link-factory }
 { name = libpipewire-module-access args = { access.force = unrestricted } }
]
context.objects = [
 { factory = spa-node-factory args = { factory.name = support.node.driver node.name = Dummy-Driver node.group = pipewire.dummy priority.driver = 20000 } }
]
''')
        env['PIPEWIRE_CONFIG_DIR'] = str(runtime)
        start('pipewire', ['pipewire', '-c', str(runtime / 'pipewire.conf')])
        wait(lambda: (runtime / 'pipewire-0').exists())
        backend_config = Path(env['XDG_CONFIG_HOME']) / 'xdg-desktop-portal-wlr'
        backend_config.mkdir(parents=True)
        (backend_config / 'config').write_text('[screencast]\noutput_name=HEADLESS-2\n'
            'chooser_type=none\nmax_fps=60\nforce_mod_linear=true\n')
        portal_config = Path(env['XDG_CONFIG_HOME']) / 'xdg-desktop-portal'
        portal_config.mkdir()
        (portal_config / 'portals.conf').write_text('[preferred]\ndefault=wlr\n'
            'org.freedesktop.impl.portal.ScreenCast=wlr\n'
            'org.freedesktop.impl.portal.Screenshot=wlr\n')
        start('wlr', [str(args.backend), '--loglevel=TRACE'])
        wait(lambda: owns('org.freedesktop.impl.portal.desktop.wlr'))
        start('portal', [str(args.portal)])
        wait(lambda: owns('org.freedesktop.portal.Desktop'))
        env.update(ECLIPSE_PORTAL_FIXTURE='1', ECLIPSE_STARTUP_EXPECTED_SIZE='1280x800')
        probe = subprocess.Popen([str(args.probe)], env=env, stdout=subprocess.PIPE,
                                 stderr=subprocess.PIPE, text=True)
        processes.append(probe)
        deadline = time.monotonic() + 85
        links = 0
        graphs = set()
        while probe.poll() is None:
            if time.monotonic() > deadline:
                raise TimeoutError('Real owned portal stream timed out')
            # This private PipeWire intentionally has no session manager or
            # hardware discovery. Link only its finite, owned video ports.
            outputs = subprocess.run(['pw-link', '-o'], env=env, capture_output=True,
                                     text=True, timeout=2).stdout.strip().splitlines()
            inputs = subprocess.run(['pw-link', '-i'], env=env, capture_output=True,
                                    text=True, timeout=2).stdout.strip().splitlines()
            graph = (tuple(outputs), tuple(inputs))
            if graph not in graphs:
                graphs.add(graph)
                (args.output / f'graph-{len(graphs)}.json').write_text(
                    subprocess.run(['pw-dump'], env=env, capture_output=True,
                                   text=True, timeout=3).stdout)
                (args.output / f'ports-{len(graphs)}.json').write_text(
                    json.dumps(dict(outputs=outputs, inputs=inputs), indent=2))
            if outputs and inputs:
                result = subprocess.run(['pw-link', outputs[-1], inputs[-1]], env=env,
                    capture_output=True, text=True, timeout=2)
                links += result.returncode == 0
                if result.returncode:
                    (args.output / 'link-error.log').write_text(result.stderr)
                elif links == 1:
                    (args.output / 'linked-graph.json').write_text(subprocess.run(
                        ['pw-dump'], env=env, capture_output=True, text=True, timeout=3).stdout)
            time.sleep(.04)
        stdout, stderr = probe.communicate(timeout=2)
        (args.output / 'probe.out').write_text(stdout)
        (args.output / 'probe.err').write_text(stderr)
        if probe.returncode:
            raise RuntimeError('Real portal startup probe failed: ' + stderr[-3000:])
        reports = [json.loads(line) for line in stdout.splitlines() if line.startswith('{')]
        assert len(reports) == 2 and all(r['ok'] and r['frames'] >= 60 for r in reports), reports
        backend_log = (args.output / 'wlr.log').read_text()
        assert backend_log.count('max_framerate: (30 / 1)') >= 2, \
            'The real variable-rate source must honor the requested 30 FPS ceiling'
        result = dict(ok=True, real_wlr_portal=True, isolated_compositor=True,
                      user_desktop_accessed=False, explicit_owned_links=links,
                      capture_max_fps=30, reports=reports)
        (args.output / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
        print(json.dumps(result), flush=True)
    finally:
        for process in reversed(processes):
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=2)
        for log in logs:
            log.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--probe', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--portal', type=Path, default=Path('/usr/lib/xdg-desktop-portal'))
    parser.add_argument('--backend', type=Path, default=Path('/usr/lib/xdg-desktop-portal-wlr'))
    parser.add_argument('--inside-owned-bus', action='store_true', help=argparse.SUPPRESS)
    args = parser.parse_args()
    args.probe, args.output = args.probe.resolve(), args.output.resolve()
    if args.inside_owned_bus:
        inside(args)
        return
    for name in ['sway', 'swaymsg', 'foot', 'pipewire', 'pw-link', 'pw-dump', 'gdbus', 'dbus-run-session']:
        if shutil.which(name) is None:
            raise RuntimeError('Missing fixture dependency: ' + name)
    for path in [args.probe, args.portal, args.backend]:
        if not path.is_file():
            raise RuntimeError('Missing fixture binary: ' + str(path))
    args.output.mkdir(parents=True, exist_ok=False)
    with tempfile.TemporaryDirectory(prefix='uu-wayland-input-') as temporary:
        env = os.environ.copy()
        for name in list(env):
            if name in ['DISPLAY', 'WAYLAND_DISPLAY', 'SWAYSOCK', 'DBUS_SESSION_BUS_ADDRESS'] or name.startswith(('PIPEWIRE_', 'UU_', 'XDG_')):
                env.pop(name, None)
        env.update(XDG_RUNTIME_DIR=temporary, XDG_CONFIG_HOME=temporary+'/config',
            XDG_DATA_HOME=temporary+'/data', XDG_STATE_HOME=temporary+'/state',
            XDG_CACHE_HOME=temporary+'/cache', XDG_CURRENT_DESKTOP='sway',
            WLR_BACKENDS='headless', WLR_HEADLESS_OUTPUTS='2', WLR_RENDERER='pixman',
            WLR_LIBINPUT_NO_DEVICES='1', UU_OWNED_WAYLAND_TEST='1')
        command = ['dbus-run-session', '--', sys.executable, str(Path(__file__).resolve()),
            '--inside-owned-bus', '--probe', str(args.probe), '--output', str(args.output),
            '--portal', str(args.portal), '--backend', str(args.backend)]
        process = subprocess.Popen(command, env=env, start_new_session=True)
        try:
            code = process.wait(timeout=115)
            if code:
                raise subprocess.CalledProcessError(code, command)
        finally:
            try:
                os.killpg(process.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            if process.poll() is None:
                process.wait(timeout=5)


if __name__ == '__main__':
    main()
