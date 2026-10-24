#!/usr/bin/env python3
"""Measure a real wlr portal on an owned headless Sway, never a user desktop."""
import argparse
import json
import os
import re
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
    processes, logs, process_names = [], [], []

    def start(name, command):
        log = (args.output / (name + '.log')).open('w')
        logs.append(log)
        process = subprocess.Popen(command, env=env, stdout=log, stderr=subprocess.STDOUT)
        processes.append(process)
        process_names.append(name)
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
                          f'output HEADLESS-2 mode 1280x800 position 320 0 scale 2 transform {args.output_transform}\n'
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
            'import sys,time,os,select,tty,json\nfrom pathlib import Path\n'
            'tty.setraw(0);os.write(1,b"\\x1b[?1003h\\x1b[?1006h");i=0\nwhile True:\n'
            ' if i==0 or not os.environ.get("ECLIPSE_STATIC_DESKTOP"):\n'
            '  if os.environ.get("ECLIPSE_NATIVE_QUADRANTS"):\n'
            '   size=os.get_terminal_size(0);sys.stdout.write("\\x1b[?7l")\n'
            '   for row in range(size.lines):\n'
            '    colors=("255;0;0","0;255;0") if row<size.lines//2 else ("0;0;255","255;255;255")\n'
            '    sys.stdout.write(f"\\x1b[{row+1};1H\\x1b[48;2;{colors[0]}m"+" "*(size.columns//2)+f"\\x1b[48;2;{colors[1]}m"+" "*(size.columns-size.columns//2))\n'
            '  else:sys.stdout.write("\\x1b[48;5;"+str(24+i%2)+"m\\x1b[2J\\x1b[Hgenerated capture fixture")\n'
            '  sys.stdout.flush()\n'
            ' i+=1\n'
            ' if select.select([0],[],[],.1)[0]:\n'
            '  data=os.read(0,4096)\n'
            '  with open(sys.argv[1],"ab") as output:output.write(data)\n'
            '  size=os.get_terminal_size(0);Path(sys.argv[1]+".json").write_text(json.dumps(dict(columns=size.columns,lines=size.lines)))\n',
            str(args.output/'generated-input.bin')])
        if not args.native_capture:
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
            # This updates only our private D-Bus activation environment. Backends
            # activated by the portal must see the same owned compositor/daemon.
            subprocess.run(['dbus-update-activation-environment', 'WAYLAND_DISPLAY',
                'XDG_CURRENT_DESKTOP', 'XDG_RUNTIME_DIR', 'XDG_CONFIG_HOME',
                'XDG_DATA_HOME', 'XDG_STATE_HOME', 'XDG_CACHE_HOME', 'PIPEWIRE_CONFIG_DIR'],
                env=env, capture_output=True, check=True, timeout=3)
            wlr = start('wlr', [str(args.backend), '--loglevel=DEBUG'])
            wait(lambda: owns('org.freedesktop.impl.portal.desktop.wlr'))
            start('portal', [str(args.portal)])
            wait(lambda: owns('org.freedesktop.portal.Desktop'))
        expected_size='800x1280' if args.output_transform in ('90','270','flipped-90','flipped-270') else '1280x800'
        env.update(ECLIPSE_PORTAL_FIXTURE='1', ECLIPSE_STARTUP_EXPECTED_SIZE=expected_size,
                   ECLIPSE_STARTUP_FRAMES=str(args.frames_per_session),
                   ECLIPSE_STARTUP_ROUNDS=str(args.sessions),
                   ECLIPSE_STARTUP_EXERCISE_QUALITY='1' if args.exercise_quality else '0',
                   ECLIPSE_STARTUP_EXERCISE_INPUT='1' if args.exercise_input else '0')
        if args.native_capture:
            env.update(ECLIPSE_STARTUP_MONITOR='HEADLESS-2',
                       ECLIPSE_STARTUP_VERIFY_ORIENTATION='1',
                       ECLIPSE_STARTUP_EXERCISE_DISPLAYS='1' if args.exercise_displays else '0')
        probe = subprocess.Popen([str(args.probe)], env=env, stdout=subprocess.PIPE,
                                 stderr=subprocess.PIPE, text=True)
        processes.append(probe)
        process_names.append('probe')
        deadline = time.monotonic() + args.frames_per_session*args.sessions/24 + 85
        links = 0
        graphs = set()
        source_started = time.monotonic() if args.native_capture else None
        terminated_at = None
        while probe.poll() is None:
            if time.monotonic() > deadline:
                raise TimeoutError('Real owned portal stream timed out')
            if (args.terminate_backend_after and source_started and terminated_at is None
                    and time.monotonic()>=source_started+args.terminate_backend_after):
                if args.native_capture:
                    subprocess.run(['swaymsg','output','HEADLESS-2','disable'],env=env,check=True,capture_output=True,timeout=2)
                else:
                    wlr.terminate()
                terminated_at=time.monotonic()
            if terminated_at and time.monotonic()-terminated_at>8:
                raise TimeoutError('Owned backend loss left the viewer running on cached frames')
            if args.native_capture:
                time.sleep(.04)
                continue
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
                if result.returncode == 0 and source_started is None:
                    source_started=time.monotonic()
                if result.returncode:
                    (args.output / 'link-error.log').write_text(result.stderr)
                elif links == 1:
                    (args.output / 'linked-graph.json').write_text(subprocess.run(
                        ['pw-dump'], env=env, capture_output=True, text=True, timeout=3).stdout)
            time.sleep(.04)
        stdout, stderr = probe.communicate(timeout=2)
        (args.output / 'probe.out').write_text(stdout)
        (args.output / 'probe.err').write_text(stderr)
        if args.terminate_backend_after:
            assert terminated_at is not None and probe.returncode!=0, 'Backend loss was not reported'
            result=dict(ok=True,backend_loss_detected=True,
                detection_ms=round((time.monotonic()-terminated_at)*1000),
                user_desktop_accessed=False)
            (args.output / 'result.json').write_text(json.dumps(result,indent=2)+'\n')
            print(json.dumps(result),flush=True)
            return
        if probe.returncode:
            raise RuntimeError('Real portal startup probe failed: ' + stderr[-3000:])
        reports = [json.loads(line) for line in stdout.splitlines() if line.startswith('{')]
        assert len(reports) == args.sessions and all(
            r['ok'] and r['frames'] >= args.frames_per_session for r in reports), reports
        negotiated=[]
        if args.native_capture:
            assert all(r['first_frame_ms']<3000 for r in reports), reports
            if args.exercise_displays:
                assert all(len(r['display_switches'])==2 for r in reports),reports
            assert 'portal' not in process_names and 'pipewire' not in process_names, process_names
        else:
            backend_log = (args.output / 'wlr.log').read_text()
            negotiated = [int(value) for value in re.findall(r'max_framerate: \((\d+) / 1\)', backend_log)]
            assert len(negotiated)>=args.sessions and all(0<fps<=240 for fps in negotiated)
        if args.exercise_quality:
            assert all([value['settings']['fps'] for value in report['applied_quality']]==[60,24]
                       for report in reports), 'Native output frame rates were not applied'
        observed_input = None
        if args.exercise_input:
            data=(args.output/'generated-input.bin').read_bytes()
            size=json.loads((args.output/'generated-input.bin.json').read_text())
            clicks=[(int(x),int(y)) for x,y in re.findall(rb'\x1b\[<0;(\d+);(\d+)M',data)]
            centered=[(x,y) for x,y in clicks if abs(x-(size['columns']+1)/2)<=3
                       and abs(y-(size['lines']+1)/2)<=3]
            assert b'a' in data and len(centered)>=args.sessions, \
                'Native input did not reach the selected scaled terminal surface'
            observed_input=dict(keyboard=True,centered_clicks=len(centered),sessions=args.sessions)
        result = dict(ok=True, real_wlr_portal=not args.native_capture,native_capture=args.native_capture,
                      static_desktop=args.static_desktop,isolated_compositor=True,
                      user_desktop_accessed=False, explicit_owned_links=links,
                      capture_max_fps=negotiated, observed_input=observed_input,reports=reports)
        (args.output / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
        print(json.dumps(result), flush=True)
    finally:
        (args.output / 'process-status.json').write_text(json.dumps([
            dict(name=name,pid=process.pid,exit_code=process.poll())
            for name,process in zip(process_names,processes)],indent=2))
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
    parser.add_argument('--frames-per-session', type=int, default=60)
    parser.add_argument('--sessions', type=int, default=2)
    parser.add_argument('--exercise-quality', action='store_true')
    parser.add_argument('--exercise-input', action='store_true')
    parser.add_argument('--native-capture',action='store_true',help='Exercise compositor capture with no portal or PipeWire process')
    parser.add_argument('--static-desktop',action='store_true',help='Do not animate the generated surface')
    parser.add_argument('--exercise-displays',action='store_true',help='Switch between actual owned outputs and back')
    parser.add_argument('--output-transform',choices=('normal','90','180','270','flipped','flipped-90','flipped-180','flipped-270'),default='normal')
    parser.add_argument('--terminate-backend-after',type=float,default=0)
    parser.add_argument('--inside-owned-bus', action='store_true', help=argparse.SUPPRESS)
    args = parser.parse_args()
    if not (60<=args.frames_per_session<=18000 and 1<=args.sessions<=8
            and args.frames_per_session*args.sessions<=108000
            and 0<=args.terminate_backend_after<=60):
        parser.error('Requires 60..18000 frames, 1..8 sessions, at most 108000 total frames')
    args.probe, args.output = args.probe.resolve(), args.output.resolve()
    if args.inside_owned_bus:
        inside(args)
        return
    if args.exercise_displays and not args.native_capture:
        parser.error('Display switching requires --native-capture')
    required=['sway','swaymsg','foot','dbus-run-session']
    if not args.native_capture:
        required+=['pipewire','pw-link','pw-dump','gdbus','dbus-update-activation-environment']
    for name in required:
        if shutil.which(name) is None:
            raise RuntimeError('Missing fixture dependency: ' + name)
    for path in [args.probe]+([] if args.native_capture else [args.portal,args.backend]):
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
        if args.native_capture:
            env['ECLIPSE_NATIVE_QUADRANTS']='1'
            if args.static_desktop:env['ECLIPSE_STATIC_DESKTOP']='1'
        else:
            env['UU_WAYLAND_CAPTURE']='portal'
        command = ['dbus-run-session', '--', sys.executable, str(Path(__file__).resolve()),
            '--inside-owned-bus', '--probe', str(args.probe), '--output', str(args.output),
            '--portal', str(args.portal), '--backend', str(args.backend),
            '--frames-per-session', str(args.frames_per_session), '--sessions', str(args.sessions),
            '--output-transform',args.output_transform,
            '--terminate-backend-after',str(args.terminate_backend_after)]
        if args.exercise_quality:
            command.append('--exercise-quality')
        if args.exercise_input:
            command.append('--exercise-input')
        if args.native_capture:command.append('--native-capture')
        if args.static_desktop:command.append('--static-desktop')
        if args.exercise_displays:command.append('--exercise-displays')
        process = subprocess.Popen(command, env=env, start_new_session=True)
        try:
            code = process.wait(timeout=args.frames_per_session*args.sessions/24+115)
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
