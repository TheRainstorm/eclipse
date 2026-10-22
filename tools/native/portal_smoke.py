#!/usr/bin/env python3
"""Exercise portal consent lifecycle on an owned D-Bus bus; never access a desktop."""
import argparse
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import sys
import tempfile
import time


def start_video(prefix, runtime):
    """Private daemon with a generated SPA video node and no device discovery."""
    binary = prefix / 'bin' if prefix else Path(shutil.which('pipewire') or '/missing').parent
    env = os.environ.copy()
    if prefix:
        env['PIPEWIRE_CONFIG_DIR'] = str(prefix / 'share/pipewire')
        modules = list((prefix / 'lib').glob('*/pipewire-0.3')) + list((prefix / 'lib').glob('pipewire-0.3'))
        if modules: env['PIPEWIRE_MODULE_DIR'] = str(modules[0])
    config = Path(runtime) / 'generated-video.conf'
    config.write_text('''
context.properties = { core.daemon = true core.name = pipewire-0 support.dbus = false }
context.spa-libs = { support.* = support/libspa-support videotestsrc = videotestsrc/libspa-videotestsrc }
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
 { factory = spa-node-factory args = { factory.name = videotestsrc node.name = EclipseFixture media.class = Video/Source } }
]
''')
    log = open(Path(runtime) / 'pipewire.log', 'w')
    process = subprocess.Popen([str(binary / 'pipewire'), '-c', str(config)], env=env, stdout=log, stderr=log)
    log.close()
    try:
        for _ in range(60):
            if process.poll() is not None: raise RuntimeError('Owned PipeWire daemon failed')
            if (Path(runtime) / 'pipewire-0').exists(): break
            time.sleep(.03)
        dump = subprocess.check_output([str(binary / 'pw-dump')], env=env, text=True, timeout=2)
        node = next(item['id'] for item in json.loads(dump)
                    if item.get('info', {}).get('props', {}).get('node.name') == 'EclipseFixture')
        return process, node, binary, env
    except BaseException:
        process.terminate(); process.wait(timeout=2)
        raise


def fixture(args):
    import dbus
    import dbus.lowlevel
    import dbus.mainloop.glib
    from gi.repository import GLib
    dbus.mainloop.glib.DBusGMainLoop(set_as_default=True)
    bus = dbus.SessionBus(private=True)
    bus.request_name('org.freedesktop.portal.Desktop')
    events = []
    loop = GLib.MainLoop()
    probe = None
    native_host = None
    host_log = None
    video = None
    video_node = None
    if args.case.startswith('stream'):
        video, video_node, pw_binary, pw_env = start_video(args.pipewire_prefix, os.environ['XDG_RUNTIME_DIR'])
    root = '/org/freedesktop/portal/desktop'
    portal = 'org.freedesktop.portal.'

    def response(path, code, data):
        message = dbus.lowlevel.SignalMessage(path, portal + 'Request', 'Response')
        message.append(dbus.UInt32(code), dbus.Dictionary(data, signature='sv'), signature='ua{sv}')
        bus.send_message(message)
        return False

    def method(_connection, message):
        if message.get_type() != dbus.lowlevel.MESSAGE_TYPE_METHOD_CALL:
            return
        interface, name = message.get_interface(), message.get_member()
        if not (interface or '').startswith(portal) and interface != 'org.freedesktop.DBus.Properties':
            return
        values = message.get_args_list()
        answer = dbus.lowlevel.MethodReturnMessage(message)
        events.append(dict(method=name, interface=interface, timestamp=time.monotonic()))
        if interface == 'org.freedesktop.DBus.Properties':
            if name == 'Get':
                answer.append(dbus.UInt32(5, variant_level=1), signature='v')
            elif name == 'GetAll':
                answer.append(dbus.Dictionary({'version': dbus.UInt32(5)}, signature='sv'), signature='a{sv}')
        elif name == 'Close':
            events[-1]['session'] = '/session/' in message.get_path()
        elif name in ('CreateSession', 'SelectSources', 'SelectDevices', 'Start'):
            options = values[-1]
            sender = message.get_sender().lstrip(':').replace('.', '_')
            request = root + '/request/' + sender + '/' + str(options['handle_token'])
            answer.append(dbus.ObjectPath(request), signature='o')
            data, code = {}, 0
            if name == 'CreateSession':
                data['session_handle'] = dbus.ObjectPath(root + '/session/' + sender + '/' + str(options['session_handle_token']))
            if name == 'Start':
                if video_node is None:
                    code = 1
                else:
                    data['devices'] = dbus.UInt32(3)
                    data['streams'] = dbus.Array([(dbus.UInt32(video_node), dbus.Dictionary({
                        'size': dbus.Struct((160, 120), signature='ii'),
                        'position': dbus.Struct((0, 0), signature='ii'),
                        'source_type': dbus.UInt32(1),
                    }, signature='sv'))], signature='(ua{sv})')
            if name != 'Start' or args.case != 'cancel':
                GLib.timeout_add(10, response, request, code, data)
        elif name == 'OpenPipeWireRemote' and video_node is not None:
            connection = socket.socket(socket.AF_UNIX)
            try:
                connection.connect(str(Path(os.environ['XDG_RUNTIME_DIR']) / 'pipewire-0'))
                answer.append(dbus.types.UnixFd(connection.fileno()), signature='h')
            finally:
                connection.close()
        elif name.startswith('Notify') and video_node is not None:
            if name == 'NotifyKeyboardKeycode':
                events[-1].update(keycode=int(values[-2]), state=int(values[-1]))
            if name == 'NotifyPointerMotionAbsolute':
                events[-1].update(x=float(values[-2]), y=float(values[-1]))
        else:
            error = dbus.lowlevel.ErrorMessage(message, 'org.freedesktop.DBus.Error.UnknownMethod', name)
            bus.send_message(error)
            return
        bus.send_message(answer)
        return dbus.lowlevel.HANDLER_RESULT_HANDLED

    bus.add_message_filter(method)
    command = [str(args.probe)]
    if args.case != 'stream-view': command += ['--allow-input']
    if args.case.startswith('stream'): command += ['--frames', '60', '--sessions', '2']
    if args.case == 'stream-input': command += ['--exercise-fixture-input']
    if args.case == 'cancel': command += ['--cancel-after-ms', '400']
    if args.case == 'stream-native':
        if args.cli is None: raise ValueError('stream-native requires --cli')
        runtime = Path(os.environ['XDG_RUNTIME_DIR'])
        invitation = runtime / 'fixture.eclipse-invite.json'
        host_log = (runtime / 'native-host.log').open('w')
        native_host = subprocess.Popen([str(args.cli), 'native-host', '--allow-input',
                                        '--invitation-file', str(invitation), '--max-seconds', '10'],
                                       env=pw_env, stdout=host_log, stderr=host_log)
        for _ in range(100):
            if invitation.exists(): break
            if native_host.poll() is not None: raise RuntimeError('Owned native host ended before invitation')
            time.sleep(.03)
        command = [str(args.cli), 'native-probe', '--invitation-file', str(invitation),
                   '--frames', '60', '--timeout-seconds', '8', '--report', str(runtime / 'native-report.json')]
    probe = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                             env=pw_env if video_node is not None else None)
    deadline = time.monotonic() + 10

    def poll():
        if probe.poll() is not None or time.monotonic() > deadline:
            if (args.case == 'stream-native' and time.monotonic() <= deadline
                    and not any(e['method'] == 'Close' and e.get('session') for e in events)):
                return True
            loop.quit()
            return False
        if video_node is not None:
            output = subprocess.run([str(pw_binary / 'pw-link'), '-o'], env=pw_env, capture_output=True, text=True, timeout=1)
            inputs = subprocess.run([str(pw_binary / 'pw-link'), '-i'], env=pw_env, capture_output=True, text=True, timeout=1)
            if output.stdout.strip() and inputs.stdout.strip():
                link = subprocess.run([str(pw_binary / 'pw-link'), output.stdout.strip().splitlines()[0], inputs.stdout.strip().splitlines()[0]],
                                      env=pw_env, capture_output=True, text=True, timeout=1)
                if link.returncode == 0: events.append(dict(method='fixture-link'))
        return True

    GLib.timeout_add(20, poll)
    try:
        loop.run()
        stdout, stderr = probe.communicate(timeout=1)
        if args.case == 'stream-native':
            if probe.returncode:
                raise RuntimeError('Native portal stream failed: ' + stderr[-2000:] + ' / ' + stdout[-1000:])
            native = json.loads((Path(os.environ['XDG_RUNTIME_DIR']) / 'native-report.json').read_text())
            assert native['completed'] and native['final']['decoded'] >= 60 and native['final']['errors'] == 0, native
            assert (native['final']['width'], native['final']['height']) == (320, 240), native
            native_host.wait(timeout=3)
            assert native_host.returncode == 0, 'Native host shutdown failed'
            assert any(e['method'] == 'Close' and e.get('session') for e in events), events
            assert not any(e['method'].startswith('Notify') for e in events), events
            print(json.dumps(dict(case=args.case, generated_video=True, encrypted_native_media=True,
                                  decoded=native['final']['decoded'], errors=native['final']['errors'],
                                  no_user_desktop=True, session_closed=True)))
            return
        reports = [json.loads(line) for line in stdout.splitlines() if line.strip()]
        report = reports[-1]
        if video_node is not None:
            assert probe.returncode == 0 and report['ok'] and report['frames'] == 60, (stdout, stderr, events)
            assert len(reports) == 2 and all(r['ok'] for r in reports), reports
            assert all(r['distinct_frames'] >= 2 for r in reports), reports
            assert len([e for e in events if e['method'] == 'Close' and e.get('session')]) == 2, events
            assert (report['width'], report['height']) == (320, 240), report
            assert report['input_available'] == (args.case == 'stream-input'), report
            if args.case == 'stream-input':
                keys = [(e['keycode'], e['state']) for e in events if e['method'] == 'NotifyKeyboardKeycode']
                assert keys == [(30, 1), (30, 0), (30, 1), (30, 0)], keys
                assert any(e['method'] == 'NotifyPointerMotionAbsolute' and e['x'] == 80 and e['y'] == 60 for e in events), events
            else:
                assert not any(e['method'].startswith('Notify') for e in events), events
        else:
            assert probe.returncode != 0 and not report['ok'], (stdout, stderr)
        assert any(e['method'] == 'Start' for e in events), events
        assert any(e['method'] == 'Close' and e.get('session') for e in events), events
        assert report['elapsed_ms'] < 3000, report
        if video_node is None:
            assert not any(e['method'] == 'OpenPipeWireRemote' or e['method'].startswith('Notify') for e in events), events
        result = dict(case=args.case, consent_started=True, session_closed=True,
                      generated_video=video_node is not None, no_user_desktop=True,
                      input_checked=args.case == 'stream-input', elapsed_ms=report['elapsed_ms'])
        print(json.dumps(result))
    finally:
        if probe.poll() is None:
            probe.terminate()
            try: probe.wait(timeout=2)
            except subprocess.TimeoutExpired: probe.kill(); probe.wait()
        bus.close()
        if native_host is not None and native_host.poll() is None:
            native_host.terminate()
            try: native_host.wait(timeout=2)
            except subprocess.TimeoutExpired: native_host.kill(); native_host.wait()
        if host_log is not None: host_log.close()
        if video is not None:
            video.terminate(); video.wait(timeout=2)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--probe', type=Path, required=True)
    parser.add_argument('--case', choices=('deny', 'cancel', 'stream-view', 'stream-input', 'stream-native'), required=True)
    parser.add_argument('--cli', type=Path, help='Native CLI used for the encrypted-media scenario')
    parser.add_argument('--pipewire-prefix', type=Path, help='Optional private installation prefix for PipeWire fixture tools')
    parser.add_argument('--inside-owned-bus', action='store_true', help=argparse.SUPPRESS)
    args = parser.parse_args()
    args.probe = args.probe.resolve()
    if args.cli: args.cli = args.cli.resolve()
    if args.pipewire_prefix: args.pipewire_prefix = args.pipewire_prefix.resolve()
    if args.inside_owned_bus:
        if os.environ.get('ECLIPSE_PORTAL_FIXTURE') != '1':
            raise RuntimeError('The fixture must run on its own D-Bus bus')
        fixture(args)
        return
    with tempfile.TemporaryDirectory(prefix='eclipse-portal-fixture-') as temporary:
        env = os.environ.copy()
        for name in ('DBUS_SESSION_BUS_ADDRESS', 'DISPLAY', 'WAYLAND_DISPLAY'):
            env.pop(name, None)
        env.update(ECLIPSE_PORTAL_FIXTURE='1', WAYLAND_DISPLAY='owned-fixture',
                   XDG_RUNTIME_DIR=temporary, XDG_DATA_HOME=temporary + '/data', XDG_STATE_HOME=temporary + '/state')
        command = ['dbus-run-session', '--', sys.executable, str(Path(__file__).resolve()),
                   '--inside-owned-bus', '--probe', str(args.probe), '--case', args.case]
        if args.pipewire_prefix: command += ['--pipewire-prefix', str(args.pipewire_prefix)]
        if args.cli: command += ['--cli', str(args.cli)]
        subprocess.run(command, env=env, check=True, timeout=15)


if __name__ == '__main__':
    main()
