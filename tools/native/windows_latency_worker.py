#!/usr/bin/env python3
"""Foreground, bounded Windows half of an owned native latency experiment.

Launch this as the interactive console owner, with a NEW isolated working
folder. A machine-specific adapter stages verified artifacts and collects the
reports. This worker never installs a service or opens an existing profile.
"""
import argparse
import ctypes as C
from ctypes import wintypes as W
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import time


def atomic(path, value):
    temp = path.with_suffix(path.suffix + '.partial')
    temp.write_text(json.dumps(value, indent=2) + '\n', encoding='utf8')
    temp.replace(path)


def sha(path):
    with Path(path).open('rb') as source:
        return hashlib.file_digest(source, 'sha256').hexdigest()


def owned_job():
    """Kernel-owned descendant cleanup even if the scheduler kills the worker."""
    kernel = C.WinDLL('kernel32', use_last_error=True)
    kernel.CreateJobObjectW.argtypes = [C.c_void_p, W.LPCWSTR]
    kernel.CreateJobObjectW.restype = W.HANDLE
    kernel.GetCurrentProcess.restype = W.HANDLE
    kernel.SetInformationJobObject.argtypes = [W.HANDLE, C.c_int, C.c_void_p, W.DWORD]
    kernel.AssignProcessToJobObject.argtypes = [W.HANDLE, W.HANDLE]
    class Basic(C.Structure):
        _fields_ = [('process_time', C.c_int64), ('job_time', C.c_int64), ('flags', W.DWORD),
                    ('min_ws', C.c_size_t), ('max_ws', C.c_size_t), ('active', W.DWORD),
                    ('affinity', C.c_size_t), ('priority', W.DWORD), ('scheduling', W.DWORD)]
    class IO(C.Structure):
        _fields_ = [(name, C.c_uint64) for name in ('reads', 'writes', 'other', 'read_bytes', 'write_bytes', 'other_bytes')]
    class Extended(C.Structure):
        _fields_ = [('basic', Basic), ('io', IO), ('process_memory', C.c_size_t),
                    ('job_memory', C.c_size_t), ('peak_process', C.c_size_t), ('peak_job', C.c_size_t)]
    job = kernel.CreateJobObjectW(None, None)
    limits = Extended(); limits.basic.flags = 0x2000  # KILL_ON_JOB_CLOSE
    if not job or not kernel.SetInformationJobObject(job, 9, C.byref(limits), C.sizeof(limits)):
        raise C.WinError(C.get_last_error())
    if not kernel.AssignProcessToJobObject(job, kernel.GetCurrentProcess()):
        raise C.WinError(C.get_last_error())
    # Keep the only handle alive until process exit (closing it kills this worker too).
    return job


def resources(process):
    class Counters(C.Structure):
        _fields_ = [('cb', W.DWORD), ('faults', W.DWORD)] + [(name, C.c_size_t) for name in
                    ('peak_ws', 'working_set', 'peak_paged', 'paged', 'peak_nonpaged', 'nonpaged', 'pagefile', 'peak_pagefile', 'private')]
    psapi = C.WinDLL('psapi', use_last_error=True); kernel = C.WinDLL('kernel32', use_last_error=True)
    psapi.GetProcessMemoryInfo.argtypes = [W.HANDLE, C.c_void_p, W.DWORD]
    kernel.GetProcessHandleCount.argtypes = [W.HANDLE, C.POINTER(W.DWORD)]
    kernel.GetProcessTimes.argtypes = [W.HANDLE, C.c_void_p, C.c_void_p, C.c_void_p, C.c_void_p]
    counters = Counters(); counters.cb = C.sizeof(counters); handles = W.DWORD()
    creation, exit_time, kernel_time, user_time = [C.c_uint64() for _ in range(4)]
    handle = W.HANDLE(int(process._handle))
    if not psapi.GetProcessMemoryInfo(handle, C.byref(counters), C.sizeof(counters)):
        return None
    kernel.GetProcessHandleCount(handle, C.byref(handles))
    kernel.GetProcessTimes(handle, C.byref(creation), C.byref(exit_time), C.byref(kernel_time), C.byref(user_time))
    return {'at': time.monotonic(), 'working_set_mb': counters.working_set / 1048576,
            'private_mb': counters.private / 1048576, 'handles': handles.value,
            'cpu_seconds': (kernel_time.value + user_time.value) / 1e7}


def stimulus(config):
    """Own nonactivating GDI window: DWM -> real desktop duplication, not synthetic capture."""
    user = C.WinDLL('user32', use_last_error=True); gdi = C.WinDLL('gdi32', use_last_error=True)
    kernel = C.WinDLL('kernel32', use_last_error=True)
    callback_type = C.WINFUNCTYPE(C.c_ssize_t, W.HWND, W.UINT, W.WPARAM, W.LPARAM)
    class Class(C.Structure):
        _fields_ = [('style', W.UINT), ('proc', callback_type), ('cls_extra', C.c_int), ('wnd_extra', C.c_int),
                    ('instance', W.HINSTANCE), ('icon', W.HICON), ('cursor', W.HANDLE), ('brush', W.HBRUSH),
                    ('menu', W.LPCWSTR), ('name', W.LPCWSTR)]
    class Paint(C.Structure):
        _fields_ = [('dc', W.HDC), ('erase', W.BOOL), ('rect', W.RECT), ('restore', W.BOOL),
                    ('incremental', W.BOOL), ('reserved', C.c_byte * 32)]
    kernel.GetModuleHandleW.argtypes = [W.LPCWSTR]; kernel.GetModuleHandleW.restype = W.HMODULE
    user.DefWindowProcW.argtypes = [W.HWND, W.UINT, W.WPARAM, W.LPARAM]; user.DefWindowProcW.restype = C.c_ssize_t
    user.CreateWindowExW.argtypes = [W.DWORD, W.LPCWSTR, W.LPCWSTR, W.DWORD, C.c_int, C.c_int, C.c_int, C.c_int,
                                   W.HWND, W.HMENU, W.HINSTANCE, C.c_void_p]; user.CreateWindowExW.restype = W.HWND
    user.BeginPaint.argtypes = [W.HWND, C.POINTER(Paint)]; user.BeginPaint.restype = W.HDC
    user.EndPaint.argtypes = [W.HWND, C.POINTER(Paint)]
    user.FillRect.argtypes = [W.HDC, C.POINTER(W.RECT), W.HBRUSH]
    user.InvalidateRect.argtypes = [W.HWND, C.c_void_p, W.BOOL]
    user.SetTimer.argtypes = [W.HWND, C.c_size_t, W.UINT, C.c_void_p]; user.SetTimer.restype = C.c_size_t
    user.DestroyWindow.argtypes = [W.HWND]
    user.ShowWindow.argtypes = [W.HWND, C.c_int]
    user.SetWindowPos.argtypes = [W.HWND, W.HWND, C.c_int, C.c_int, C.c_int, C.c_int, W.UINT]
    gdi.CreateSolidBrush.argtypes = [W.DWORD]; gdi.CreateSolidBrush.restype = W.HBRUSH
    gdi.DeleteObject.argtypes = [W.HANDLE]
    user.GetMessageW.argtypes = [C.POINTER(W.MSG), W.HWND, W.UINT, W.UINT]
    user.DispatchMessageW.argtypes = [C.POINTER(W.MSG)]; user.DispatchMessageW.restype = C.c_ssize_t
    count = 0; started = time.monotonic(); last_write = started
    def procedure(hwnd, message, wp, lp):
        nonlocal count, last_write
        if message == 0x113:  # WM_TIMER, one long-lived surface, no topology changes
            if time.monotonic() - started > config['deadline_seconds']:
                user.DestroyWindow(hwnd)
            else:
                user.InvalidateRect(hwnd, None, False)
            return 0
        if message == 0xF:
            paint = Paint(); dc = user.BeginPaint(hwnd, C.byref(paint)); count += 1
            for index in range(6):
                color = ((count * 3 + index * 37) % 220 + 20) | (((index*41+count) % 220+20) << 8) | (((count*2+index*31) % 220+20) << 16)
                brush = gdi.CreateSolidBrush(color)
                rectangle = W.RECT(index * 160, 0, (index+1)*160, 540)
                user.FillRect(dc, C.byref(rectangle), brush); gdi.DeleteObject(brush)
            user.EndPaint(hwnd, C.byref(paint))
            if time.monotonic() - last_write >= 1:
                atomic(Path(config['stimulus_report']), {'updates': count, 'elapsed_seconds': time.monotonic()-started})
                last_write = time.monotonic()
            return 0
        if message == 2:
            atomic(Path(config['stimulus_report']), {'updates': count, 'elapsed_seconds': time.monotonic()-started})
            user.PostQuitMessage(0); return 0
        return user.DefWindowProcW(hwnd, message, wp, lp)
    callback = callback_type(procedure)
    instance = kernel.GetModuleHandleW(None); name = 'EclipseLatencyStimulus-' + str(os.getpid())
    cls = Class(0, callback, 0, 0, instance, None, None, None, None, name)
    if not user.RegisterClassW(C.byref(cls)):
        raise C.WinError(C.get_last_error())
    hwnd = user.CreateWindowExW(0x08000000 | 0x00000080, name, 'Eclipse owned desktop latency stimulus',
                               0x80000000, 32, 32, 960, 540, None, None, instance, None)
    if not hwnd:
        raise C.WinError(C.get_last_error())
    user.SetWindowPos(hwnd, W.HWND(-1), 32, 32, 960, 540, 0x10)  # TOPMOST, NOACTIVATE
    user.ShowWindow(hwnd, 4)  # SHOWNOACTIVATE
    user.SetTimer(hwnd, 1, max(1, round(1000 / config['fps'])), None)
    atomic(Path(config['stimulus_report']), {'updates': 0, 'elapsed_seconds': 0})
    message = W.MSG()
    while user.GetMessageW(C.byref(message), None, 0, 0) > 0:
        user.TranslateMessage(C.byref(message)); user.DispatchMessageW(C.byref(message))


def main():
    parser = argparse.ArgumentParser(description=__doc__); parser.add_argument('--config', required=True, type=Path)
    parser.add_argument('--stimulus', action='store_true'); args = parser.parse_args()
    if sys.platform != 'win32':
        parser.error('Windows interactive console required')
    config_path = args.config.resolve(); config = json.loads(config_path.read_text(encoding='utf8'))
    if args.stimulus:
        stimulus(config); return 0
    session = W.DWORD(); kernel = C.WinDLL('kernel32', use_last_error=True)
    if not kernel.ProcessIdToSessionId(os.getpid(), C.byref(session)) or not session.value:
        raise RuntimeError('refusing desktop benchmark in session zero')
    if not 10 <= config['deadline_seconds'] <= 3750:
        raise ValueError('bounded deadline required')
    root = Path(config['root']).resolve(); root.mkdir(exist_ok=True)
    marker = root / 'worker-started.json'
    with marker.open('x', encoding='utf8') as file:
        json.dump({'pid': os.getpid(), 'session': session.value}, file)
    job_handle = owned_job()
    # Query the interactive worker's display context. SSH/session-zero screen
    # enumeration can report a fictitious WinDisc 1024x768 display.
    inventory = subprocess.run(['powershell.exe', '-NoProfile', '-NonInteractive', '-Command',
        "[Console]::OutputEncoding=New-Object System.Text.UTF8Encoding($false);Add-Type -AssemblyName System.Windows.Forms;$s=@([System.Windows.Forms.Screen]::AllScreens|Sort-Object DeviceName|ForEach-Object{@{name=$_.DeviceName;bounds=@($_.Bounds.X,$_.Bounds.Y,$_.Bounds.Width,$_.Bounds.Height);primary=$_.Primary}});$g=@(Get-CimInstance Win32_VideoController|Sort-Object Name|Select-Object Name,DriverVersion,CurrentHorizontalResolution,CurrentVerticalResolution,CurrentRefreshRate);@{display=$s;gpu=$g}|ConvertTo-Json -Depth 8 -Compress"],
        capture_output=True, text=True, encoding='utf8', timeout=15, creationflags=subprocess.CREATE_NO_WINDOW)
    if inventory.returncode:
        raise RuntimeError('interactive display inventory failed')
    actual_environment = json.loads(inventory.stdout)
    user = C.WinDLL('user32', use_last_error=True)
    kernel.GetCurrentThreadId.restype = W.DWORD
    user.GetProcessWindowStation.restype = W.HANDLE
    user.GetThreadDesktop.argtypes = [W.DWORD]; user.GetThreadDesktop.restype = W.HANDLE
    user.GetUserObjectInformationW.argtypes = [W.HANDLE, C.c_int, C.c_void_p, W.DWORD, C.POINTER(W.DWORD)]
    for label, handle in [('window_station', user.GetProcessWindowStation()), ('desktop', user.GetThreadDesktop(kernel.GetCurrentThreadId()))]:
        name = C.create_unicode_buffer(256); needed = W.DWORD()
        if not user.GetUserObjectInformationW(handle, 2, name, C.sizeof(name), C.byref(needed)):
            raise C.WinError(C.get_last_error())
        actual_environment[label] = name.value
    actual_environment['python_version'] = sys.version
    actual_environment['python_sha256'] = sha(sys.executable)
    atomic(root/'environment.json', actual_environment)
    for name, expected in config['sha256'].items():
        if sha(name) != expected:
            raise ValueError('staged executable mismatch')
    env = os.environ.copy()
    for name in list(env):
        if name.startswith(('UU_', 'XDG_', 'ECLIPSE_')):
            env.pop(name)
    env.update(APPDATA=str(root/'profile'), LOCALAPPDATA=str(root/'local'), XDG_DATA_HOME=str(root/'data'),
               XDG_STATE_HOME=str(root/'state'), UU_SIGNING_KEY_FILE=str(root/'absent-key'), RUST_LOG='error', ECLIPSE_FRAME_TRACE='1')
    processes = []; logs = []; samples = []; completed = False; error = None
    try:
        def launch(argv, logfile):
            log = (root / logfile).open('wb'); logs.append(log)
            process = subprocess.Popen(argv, cwd=root, env=env, stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT,
                                       creationflags=subprocess.CREATE_NO_WINDOW)
            processes.append(process); return process
        if config['role'] == 'host':
            moving = launch([sys.executable, str(Path(__file__).resolve()), '--config', str(config_path), '--stimulus'], 'stimulus.log')
            deadline = time.monotonic()+10
            while not Path(config['stimulus_report']).exists():
                if moving.poll() is not None or time.monotonic() > deadline:
                    raise RuntimeError('owned stimulus did not start')
                time.sleep(.1)
            process = launch(config['command'], 'host.log')
        elif config['role'] == 'viewer':
            process = launch(config['command'], 'viewer.log')
        else:
            raise ValueError('unknown worker role')
        started = time.monotonic()
        while process.poll() is None:
            sample = resources(process)
            if sample: samples.append(sample)
            if time.monotonic()-started > config['deadline_seconds']:
                raise TimeoutError('owned native process exceeded its deadline')
            time.sleep(.25)
        if process.returncode:
            raise RuntimeError('native process exit ' + str(process.returncode))
        completed = True
    except BaseException as failure:
        error = f'{type(failure).__name__}: {failure}'
    finally:
        for process in reversed(processes):
            if process.poll() is None:
                process.terminate()
                try: process.wait(timeout=5)
                except subprocess.TimeoutExpired: process.kill(); process.wait(timeout=5)
        for log in logs: log.close()
        atomic(root/'worker-result.json', {'completed': completed, 'cleanup_ok': all(p.poll() is not None for p in processes),
               'error': error, 'session': session.value, 'pid': os.getpid(), 'child_pids': [p.pid for p in processes], 'resources': samples})
    return 0 if completed else 1


if __name__ == '__main__':
    raise SystemExit(main())
