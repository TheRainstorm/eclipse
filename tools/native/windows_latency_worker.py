#!/usr/bin/env python3
"""Foreground, bounded Windows half of an owned native latency experiment.

Launch this as the interactive console owner, with a NEW isolated working
folder. A machine-specific adapter stages verified artifacts and collects the
reports. This worker never installs a service or opens an existing profile.
"""
import argparse
import bisect
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


class StimulusReport:
    """One owner: sample while running, seal on close, persist once in finally.

    Closing stops samples immediately. A failed write leaves a sealed report
    available for retry and raises to the caller; it never advertises success.
    """
    def __init__(self, path, metadata, writer=atomic, capacity=900_000):
        self.path = Path(path)
        self.metadata = metadata
        self.writer = writer
        self.capacity = capacity
        self.completions = []
        self.closed = False
        self.error = None
        self.final = None

    def record(self, at_us):
        if self.closed:
            raise RuntimeError('stimulus already closed')
        if len(self.completions) >= self.capacity:
            raise RuntimeError('stimulus samples overflow')
        if self.completions and at_us <= self.completions[-1]:
            raise RuntimeError('stimulus clock did not advance')
        self.completions.append(at_us)

    def close(self, error=None):
        self.closed = True
        if error is not None and self.error is None:
            self.error = str(error)

    def report(self, final=False):
        first = bisect.bisect_left(self.completions, 1_000_000)
        count = len(self.completions)-first
        gaps = sorted((b-a)/1000 for a,b in zip(self.completions[first:], self.completions[first+1:])) if final else []
        def rank(p,d=100):
            return gaps[max(0,(len(gaps)*p+d-1)//d-1)] if gaps else None
        return {**self.metadata(), 'version':3, 'updates':len(self.completions),
                'completion_offsets_retained':len(self.completions),
                'completion_offsets_us':list(self.completions) if final else [],
                'final_raw_timestamps':final, 'completed':final and self.error is None,
                'state':'completed' if final and self.error is None else 'failed' if final else 'running',
                'steady_completed_hz':(count-1)*1e6/(self.completions[-1]-self.completions[first]) if count>1 else None,
                'paint_completion_gap_ms':{'samples':len(gaps),'p50':rank(50),'p95':rank(95),
                    'p99':rank(99),'p999':rank(999,1000),'max':gaps[-1] if gaps else None},
                'scope':'GDI EndPaint API completion, not DWM composition or scanout', 'error':self.error}

    def progress(self):
        if self.closed:
            return self.final
        value = self.report()
        self.writer(self.path, value)
        return value

    def finalize(self, error=None):
        if self.final is not None:
            return self.final
        self.close(error)
        value = self.report(final=True)
        self.writer(self.path, value)
        self.final = value
        return value


def process_identity(product_process, stimulus_process, monitored_process):
    """Stimulus-clock has no product; its monitored PID is the stimulus PID."""
    return {'product_pid':product_process.pid if product_process is not None else None,
            'stimulus_pid':stimulus_process.pid if stimulus_process is not None else None,
            'monitored_pid':monitored_process.pid if monitored_process is not None else None}


def stimulus_fps(config):
    value = config.get('stimulus_fps', config['fps'])
    if isinstance(value, bool) or not isinstance(value, int) or not 30 <= value <= 240:
        raise ValueError('stimulus_fps must be an integer in 30..240')
    return value


def query_timeout_ms(config):
    value = config.get('amf_query_timeout_ms', 1)
    if isinstance(value, bool) or not isinstance(value, int) or value not in (0, 1):
        raise ValueError('amf_query_timeout_ms must be 0 or 1')
    return value


def av1_input_queue_size(config):
    value = config.get('amf_av1_input_queue_size')
    if value is not None and (isinstance(value, bool) or not isinstance(value, int) or value not in (1, 16)):
        raise ValueError('amf_av1_input_queue_size must be 1 or 16 when specified')
    return value



def binary_switch(config, name):
    value = config.get(name, 0)
    if isinstance(value, bool) or not isinstance(value, int) or value not in (0, 1):
        raise ValueError(name + ' must be integer 0 or 1')
    return value


def owner_pacing_switch(config, overlap, separate):
    paced=binary_switch(config,'capture_owner_pacing')
    if paced and not (overlap and separate):
        raise ValueError('capture owner pacing requires overlap and separate device')
    return paced


def capture_device_switches(config):
    overlap=binary_switch(config,'capture_overlap')
    separate=binary_switch(config,'capture_separate_device')
    if separate and not overlap:
        raise ValueError('separate device requires capture overlap')
    return overlap,separate


class WlanObserver:
    """Read-only lab observer: never resolves/calls WlanSetInterface.

    One separate client handle; enumerate once, query at most once per second.
    Runtime GUIDs are written only to private experiment evidence.
    """
    def __init__(self):
        self.handle = W.HANDLE(); self.api = None; self.interfaces = []; self.error = None
        try:
            class Guid(C.Structure):
                _fields_ = [('bytes', C.c_ubyte * 16)]
            class Info(C.Structure):
                _fields_ = [('guid', Guid), ('description', W.WCHAR * 256), ('state', W.DWORD)]
            self.Guid = Guid
            self.api = C.WinDLL('wlanapi.dll', winmode=0x800)
            self.api.WlanOpenHandle.argtypes = [W.DWORD, C.c_void_p, C.POINTER(W.DWORD), C.POINTER(W.HANDLE)]
            self.api.WlanOpenHandle.restype = W.DWORD
            self.api.WlanCloseHandle.argtypes = [W.HANDLE, C.c_void_p]; self.api.WlanCloseHandle.restype = W.DWORD
            self.api.WlanEnumInterfaces.argtypes = [W.HANDLE, C.c_void_p, C.POINTER(C.c_void_p)]
            self.api.WlanEnumInterfaces.restype = W.DWORD
            self.api.WlanQueryInterface.argtypes = [W.HANDLE, C.POINTER(Guid), C.c_int, C.c_void_p,
                    C.POINTER(W.DWORD), C.POINTER(C.c_void_p), C.POINTER(C.c_int)]
            self.api.WlanQueryInterface.restype = W.DWORD
            self.api.WlanFreeMemory.argtypes = [C.c_void_p]
            version = W.DWORD(); status = self.api.WlanOpenHandle(2, None, C.byref(version), C.byref(self.handle))
            if status: raise RuntimeError('WlanOpenHandle=' + str(status))
            data = C.c_void_p()
            try:
                status = self.api.WlanEnumInterfaces(self.handle, None, C.byref(data))
                if status or not data.value: raise RuntimeError('WlanEnumInterfaces=' + str(status))
                count = C.cast(data, C.POINTER(W.DWORD))[0]
                if count > 1024: raise RuntimeError('invalid WLAN interface count')
                array = C.cast(data.value + 8, C.POINTER(Info))
                self.interfaces = [Info.from_buffer_copy(array[i]) for i in range(count)]
            finally:
                if data.value: self.api.WlanFreeMemory(data)
        except Exception as error:
            self.error = type(error).__name__ + ': ' + str(error)
    def snapshot(self):
        rows = []
        if self.error is None:
            for index, info in enumerate(self.interfaces):
                size = W.DWORD(); data = C.c_void_p(); kind = C.c_int()
                status = self.api.WlanQueryInterface(self.handle, C.byref(info.guid), 3, None,
                        C.byref(size), C.byref(data), C.byref(kind))
                try:
                    rows.append({'index':index, 'guid_bytes':list(info.guid.bytes), 'initial_state':info.state,
                        'result':status, 'bytes':size.value,
                        'media':bool(C.cast(data, C.POINTER(W.BOOL))[0]) if not status and data.value and size.value == C.sizeof(W.BOOL) else None})
                finally:
                    if data.value: self.api.WlanFreeMemory(data)
                state_size = W.DWORD(); state_data = C.c_void_p(); state_kind = C.c_int()
                state_result = self.api.WlanQueryInterface(self.handle, C.byref(info.guid), 6, None,
                        C.byref(state_size), C.byref(state_data), C.byref(state_kind))
                try:
                    rows[-1].update(state_result=state_result, state_bytes=state_size.value,
                        state=C.cast(state_data, C.POINTER(C.c_int))[0] if not state_result and state_data.value and state_size.value == C.sizeof(C.c_int) else None)
                finally:
                    if state_data.value: self.api.WlanFreeMemory(state_data)
        return {'at':time.monotonic(), 'read_only':True, 'error':self.error, 'interfaces':rows}
    def close(self):
        if self.handle.value:
            handle = self.handle; self.handle = W.HANDLE()
            return self.api.WlanCloseHandle(handle, None)
        return None


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


def thread_count(pid):
    """Read-only Toolhelp snapshot at most once per second, one target PID.

    https://learn.microsoft.com/windows/win32/api/tlhelp32/ns-tlhelp32-threadentry32
    """
    now=time.monotonic();cache=getattr(thread_count,'cached',None)
    if cache and cache['pid']==pid and now-cache['at']<1:return cache
    class Entry(C.Structure):
        _fields_=[('size',W.DWORD),('usage',W.DWORD),('thread',W.DWORD),('owner',W.DWORD),('priority',W.LONG),('delta',W.LONG),('flags',W.DWORD)]
    api=C.WinDLL('kernel32',use_last_error=True)
    api.CreateToolhelp32Snapshot.argtypes=[W.DWORD,W.DWORD];api.CreateToolhelp32Snapshot.restype=W.HANDLE
    api.Thread32First.argtypes=[W.HANDLE,C.POINTER(Entry)];api.Thread32First.restype=W.BOOL
    api.Thread32Next.argtypes=[W.HANDLE,C.POINTER(Entry)];api.Thread32Next.restype=W.BOOL
    api.CloseHandle.argtypes=[W.HANDLE];api.CloseHandle.restype=W.BOOL
    result={'pid':pid,'at':now,'count':None,'error':None};handle=api.CreateToolhelp32Snapshot(4,0)
    if handle==C.c_void_p(-1).value:result['error']=C.get_last_error()
    else:
        try:
            row=Entry();row.size=C.sizeof(row);count=0
            if not api.Thread32First(handle,C.byref(row)):result['error']=C.get_last_error()
            else:
                while True:
                    count+=int(row.owner==pid);row.size=C.sizeof(row)
                    if not api.Thread32Next(handle,C.byref(row)):break
                error=C.get_last_error()
                if error!=18:result['error']=error
                else:result['count']=count
        finally:
            if not api.CloseHandle(handle):result['error']=C.get_last_error();result['count']=None
    thread_count.cached=result
    return result


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
    ticks, frequency = C.c_int64(), C.c_int64()
    qpc_ok = kernel.QueryPerformanceCounter(C.byref(ticks)) and kernel.QueryPerformanceFrequency(C.byref(frequency))
    return {'at': time.monotonic(), 'qpc':{'ticks':ticks.value,'frequency':frequency.value} if qpc_ok else None, 'working_set_mb': counters.working_set / 1048576,
            'private_mb': counters.private / 1048576, 'handles': handles.value,
            'cpu_seconds': (kernel_time.value + user_time.value) / 1e7, 'thread_count':thread_count(process.pid)}


def stimulus(config):
    """One nonactivating GDI surface; count completed EndPaint calls, not ticks.

    Waitable timer API: https://learn.microsoft.com/windows/win32/api/synchapi/nf-synchapi-createwaitabletimerexw
    The legacy WM_TIMER mode is a bounded diagnostic control, never the default.
    """
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
    user.DestroyWindow.argtypes = [W.HWND]; user.UpdateWindow.argtypes = [W.HWND]
    user.ShowWindow.argtypes = [W.HWND, C.c_int]
    user.SetWindowPos.argtypes = [W.HWND, W.HWND, C.c_int, C.c_int, C.c_int, C.c_int, W.UINT]
    gdi.CreateSolidBrush.argtypes = [W.DWORD]; gdi.CreateSolidBrush.restype = W.HBRUSH
    gdi.DeleteObject.argtypes = [W.HANDLE]
    user.GetMessageW.argtypes = [C.POINTER(W.MSG), W.HWND, W.UINT, W.UINT]
    user.PeekMessageW.argtypes = [C.POINTER(W.MSG), W.HWND, W.UINT, W.UINT, W.UINT]
    user.DispatchMessageW.argtypes = [C.POINTER(W.MSG)]; user.DispatchMessageW.restype = C.c_ssize_t
    user.MsgWaitForMultipleObjectsEx.argtypes = [W.DWORD, C.POINTER(W.HANDLE), W.DWORD, W.DWORD, W.DWORD]
    user.MsgWaitForMultipleObjectsEx.restype = W.DWORD
    kernel.CreateWaitableTimerExW.argtypes = [C.c_void_p, W.LPCWSTR, W.DWORD, W.DWORD]
    kernel.CreateWaitableTimerExW.restype = W.HANDLE
    kernel.SetWaitableTimer.argtypes = [W.HANDLE, C.POINTER(C.c_int64), W.LONG, C.c_void_p, C.c_void_p, W.BOOL]
    kernel.CancelWaitableTimer.argtypes = [W.HANDLE]; kernel.CloseHandle.argtypes = [W.HANDLE]
    clock = config.get('stimulus_clock', 'high_res_waitable')
    if clock not in ('high_res_waitable', 'legacy_wm_timer'):
        raise ValueError('unknown bounded stimulus clock')
    count = 0; started = time.perf_counter(); last_write = started
    errors = []; hwnd = None
    duration = config.get('stimulus_seconds', config['deadline_seconds'])
    recorder = StimulusReport(config['stimulus_report'], lambda: {
        'pid':os.getpid(),'hwnd':int(hwnd or 0),'clock':clock,
        'requested_fps':stimulus_fps(config),'elapsed_seconds':time.perf_counter()-started})
    def procedure(hwnd, message, wp, lp):
        nonlocal count, last_write
        if message == 0x113:
            if time.perf_counter()-started >= duration:user.DestroyWindow(hwnd)
            else:user.InvalidateRect(hwnd,None,False)
            return 0
        if message == 0xF:
            if recorder.closed:return user.DefWindowProcW(hwnd,message,wp,lp)
            paint=Paint();dc=user.BeginPaint(hwnd,C.byref(paint))
            for index in range(6):
                color=((count*3+index*37)%220+20)|(((index*41+count)%220+20)<<8)|(((count*2+index*31)%220+20)<<16)
                brush=gdi.CreateSolidBrush(color);rectangle=W.RECT(index*160,0,(index+1)*160,540)
                user.FillRect(dc,C.byref(rectangle),brush);gdi.DeleteObject(brush)
            user.EndPaint(hwnd,C.byref(paint))
            recorder.record(round((time.perf_counter()-started)*1e6));count+=1
            if time.perf_counter()-last_write>=1:
                recorder.progress();last_write=time.perf_counter()
            return 0
        if message == 2:
            recorder.close();user.PostQuitMessage(0);return 0
        return user.DefWindowProcW(hwnd,message,wp,lp)
    def checked(hwnd,message,wp,lp):
        try:return procedure(hwnd,message,wp,lp)
        except BaseException as error:
            errors.append(str(error));recorder.close(error);user.PostQuitMessage(1);return 0
    timer=None
    try:
        callback=callback_type(checked);instance=kernel.GetModuleHandleW(None);name='EclipseLatencyStimulus-'+str(os.getpid())
        cls=Class(0,callback,0,0,instance,None,None,None,None,name)
        if not user.RegisterClassW(C.byref(cls)):raise C.WinError(C.get_last_error())
        hwnd=user.CreateWindowExW(0x08000000|0x80,name,'Eclipse owned desktop latency stimulus',0x80000000,
                                  32,32,960,540,None,None,instance,None)
        if not hwnd:raise C.WinError(C.get_last_error())
        user.SetWindowPos(hwnd,W.HWND(-1),32,32,960,540,0x10);user.ShowWindow(hwnd,4)
        recorder.progress()
        if clock=='legacy_wm_timer':
            if not user.SetTimer(hwnd,1,max(1,round(1000/stimulus_fps(config))),None):raise C.WinError(C.get_last_error())
            message=W.MSG()
            while user.GetMessageW(C.byref(message),None,0,0)>0:
                user.TranslateMessage(C.byref(message));user.DispatchMessageW(C.byref(message))
        else:
            # Per-process high-resolution wait, no timeBeginPeriod/global state.
            timer=kernel.CreateWaitableTimerExW(None,None,2,0x1F0003)
            if not timer:raise C.WinError(C.get_last_error())
            handles=(W.HANDLE*1)(timer);period=1/stimulus_fps(config);deadline=started+period;running=True
            while running:
                due=C.c_int64(-max(1,round((deadline-time.perf_counter())*10_000_000)))
                if not kernel.SetWaitableTimer(timer,C.byref(due),0,None,None,False):raise C.WinError(C.get_last_error())
                result=user.MsgWaitForMultipleObjectsEx(1,handles,1000,0x4FF,4)
                if result==0:
                    now=time.perf_counter()
                    if now-started>=duration:user.DestroyWindow(hwnd)
                    else:user.InvalidateRect(hwnd,None,False);user.UpdateWindow(hwnd)
                    deadline+=period
                    if deadline<=now:deadline=now+period  # Never burst stale catch-up frames.
                elif result==0xFFFFFFFF:raise C.WinError(C.get_last_error())
                message=W.MSG()
                while user.PeekMessageW(C.byref(message),None,0,0,1):
                    if message.message==0x12:running=False;break
                    user.TranslateMessage(C.byref(message));user.DispatchMessageW(C.byref(message))
        if errors:raise RuntimeError(errors[0])
    except BaseException as error:
        recorder.close(error)
        raise
    finally:
        recorder.close()
        if timer:kernel.CancelWaitableTimer(timer);kernel.CloseHandle(timer)
        recorder.finalize()


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
    actual_environment['display'] = {'coordinate_units':'logical_pixels','outputs':actual_environment['display']}
    if config['role'] != 'stimulus-clock':
        inventory_command=[str(root/'bin/uu.exe'),'native-sources','--json']
        raw=subprocess.run(inventory_command,capture_output=True,text=True,encoding='utf8',timeout=15,creationflags=subprocess.CREATE_NO_WINDOW)
        if raw.returncode:raise RuntimeError('physical output inventory failed: '+raw.stderr[-1000:])
        actual_environment['physical_capture_inventory']=json.loads(raw.stdout)
        if config['role']=='host':
            primary=[m for m in actual_environment['physical_capture_inventory']['monitors'] if m['primary']]
            if len(primary)!=1:raise RuntimeError('no unique primary output for the owned fixture')
            actual_environment['selected_capture_source']=primary[0]
            # Pin the exact physical output examined above, not an implicit primary
            # which could change between inventory and stream preparation.
            config['command'] += ['--monitor',primary[0]['name']]
            atomic(root/'selected-source.json',primary[0])
            atomic(root/'actual-host-command.json',config['command'])
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
    overlap,separate=capture_device_switches(config)
    env['UU_CAPTURE_ENCODE_OVERLAP'] = str(overlap)
    env['UU_CAPTURE_ENCODE_SEPARATE_DEVICE'] = str(separate)
    owner_pacing=owner_pacing_switch(config,overlap,separate)
    env['UU_CAPTURE_OWNER_PACING'] = str(owner_pacing)
    env['UU_AMF_INPUT_POOL'] = str(binary_switch(config, 'amf_input_pool'))
    env['UU_NATIVE_QOS'] = str(binary_switch(config, 'native_qos'))
    env['UU_NATIVE_DEDICATED_IO'] = str(binary_switch(config, 'native_dedicated_io'))
    if binary_switch(config, 'gpu_encode_profile'):
        env['UU_GPU_ENCODE_PROFILE'] = '1'
    wlan_mode = config.get('wlan_streaming_mode', 0)
    if isinstance(wlan_mode, bool) or not isinstance(wlan_mode, int) or wlan_mode not in (0, 1):
        raise ValueError('wlan_streaming_mode must be 0 or 1')
    env['UU_WLAN_STREAMING_MODE'] = str(wlan_mode)
    query_timeout = query_timeout_ms(config)
    env['UU_AMF_QUERY_TIMEOUT_MS'] = str(query_timeout)
    av1_queue = av1_input_queue_size(config)
    if av1_queue is not None:
        env['UU_AMF_AV1_INPUT_QUEUE_SIZE'] = str(av1_queue)
    if binary_switch(config, 'socket_trace'):
        env['ECLIPSE_SOCKET_TRACE'] = '1'
        env['ECLIPSE_SOCKET_TRACE_FILE'] = str(root/'socket-trace.json')
    if config.get('local_frame_trace') is True:
        env['ECLIPSE_LOCAL_FRAME_TRACE'] = str(root/'host-local-trace.json')
    if config.get('continuous_trace') is True:
        env['ECLIPSE_CONTINUOUS_TRACE'] = '1'
    if config.get('encode_profile') is True:
        env['UU_ENCODE_PROFILE'] = '1'
    atomic(root/'diagnostic-switches.json',{'capture_owner_pacing':owner_pacing,'capture_separate_device':int(env['UU_CAPTURE_ENCODE_SEPARATE_DEVICE']),'capture_overlap':int(env['UU_CAPTURE_ENCODE_OVERLAP']),'native_dedicated_io':int(env['UU_NATIVE_DEDICATED_IO']),'native_qos':int(env['UU_NATIVE_QOS']),'socket_trace':env.get('ECLIPSE_SOCKET_TRACE')=='1','continuous_trace':env.get('ECLIPSE_CONTINUOUS_TRACE')=='1','amf_input_pool':env['UU_AMF_INPUT_POOL'],
                                         'amf_query_timeout_ms':query_timeout,'encode_profile':env.get('UU_ENCODE_PROFILE')=='1','stimulus_fps':stimulus_fps(config),'wlan_streaming_mode':wlan_mode,
                                         'amf_av1_input_queue_size':av1_queue,'gpu_encode_profile':env.get('UU_GPU_ENCODE_PROFILE')=='1'})
    processes = []; logs = []; samples = []; completed = False; error = None; moving = None
    product_process = None; monitored_process = None
    observer = WlanObserver(); wlan_samples = [observer.snapshot()]; next_wlan = time.monotonic() + 1
    try:
        def launch(argv, logfile):
            log = (root / logfile).open('wb'); logs.append(log)
            process = subprocess.Popen(argv, cwd=root, env=env, stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT,
                                       creationflags=subprocess.CREATE_NO_WINDOW)
            processes.append(process); return process
        moving = None
        if config['role'] == 'stimulus-clock':
            moving = launch([sys.executable,str(Path(__file__).resolve()),'--config',str(config_path),'--stimulus'],'stimulus.log')
            monitored_process = moving
        elif config['role'] == 'host':
            moving = launch([sys.executable, str(Path(__file__).resolve()), '--config', str(config_path), '--stimulus'], 'stimulus.log')
            deadline = time.monotonic()+10
            while not Path(config['stimulus_report']).exists():
                if moving.poll() is not None or time.monotonic() > deadline:
                    raise RuntimeError('owned stimulus did not start')
                time.sleep(.1)
            product_process = launch(config['command'], 'host.log')
        elif config['role'] == 'viewer':
            product_process = launch(config['command'], 'viewer.log')
        else:
            raise ValueError('unknown worker role')
        monitored_process = product_process if product_process is not None else monitored_process
        started = time.monotonic(); next_progress = started
        while monitored_process.poll() is None:
            if product_process is not None and moving is not None and moving.poll() is not None:
                raise RuntimeError('owned desktop stimulus stopped before the host')
            sample = resources(monitored_process)
            if sample: samples.append(sample)
            if time.monotonic() >= next_progress:
                atomic(root/'progress.json',{'worker_pid':os.getpid(),**process_identity(product_process,moving,monitored_process),'elapsed_seconds':time.monotonic()-started,'last_resource':sample,'resource_samples':len(samples),'wlan_samples':len(wlan_samples)})
                next_progress=time.monotonic()+10
            if time.monotonic() >= next_wlan:
                wlan_samples.append(observer.snapshot()); next_wlan = time.monotonic() + 1
            if time.monotonic()-started > config['deadline_seconds']:
                raise TimeoutError('owned native process exceeded its deadline')
            time.sleep(.25)
        if monitored_process.returncode:
            raise RuntimeError('native process exit ' + str(monitored_process.returncode))
        if config['role'] != 'stimulus-clock':
            end=subprocess.run(inventory_command,capture_output=True,text=True,encoding='utf8',timeout=15,creationflags=subprocess.CREATE_NO_WINDOW)
            if end.returncode or json.loads(end.stdout)!=actual_environment['physical_capture_inventory']:
                raise RuntimeError('physical output/color inventory changed during trial')
            actual_environment['end_inventory_matched']=True
            atomic(root/'environment.json',actual_environment)
        completed = True
    except BaseException as failure:
        error = f'{type(failure).__name__}: {failure}'
    finally:
        cleanup_details = {'stimulus_wm_close_attempted':False,'stimulus_wm_close_posted':False,'stimulus_wm_close_timed_out':False,'stimulus_wm_close_error':None,'children':[]}
        if moving is not None and moving.poll() is None:
            try:
                data=json.loads(Path(config['stimulus_report']).read_text())
                user=C.WinDLL('user32');user.GetWindowThreadProcessId.argtypes=[W.HWND,C.POINTER(W.DWORD)]
                user.PostMessageW.argtypes=[W.HWND,W.UINT,W.WPARAM,W.LPARAM]
                pid=W.DWORD();user.GetWindowThreadProcessId(W.HWND(data['hwnd']),C.byref(pid))
                if pid.value != moving.pid:raise RuntimeError('stimulus window PID changed')
                cleanup_details['stimulus_wm_close_attempted'] = True
                cleanup_details['stimulus_wm_close_posted'] = bool(user.PostMessageW(W.HWND(data['hwnd']),0x10,0,0))
                if not cleanup_details['stimulus_wm_close_posted']:raise RuntimeError('stimulus WM_CLOSE failed')
                moving.wait(timeout=3)
            except subprocess.TimeoutExpired:
                cleanup_details['stimulus_wm_close_timed_out'] = True
            except Exception as failure:
                cleanup_details['stimulus_wm_close_error'] = f'{type(failure).__name__}: {failure}'
        for process in reversed(processes):
            detail = {'pid':process.pid,'role':'stimulus' if process is moving or config['role']=='stimulus-clock' else 'native',
                      'exit_before_fallback':process.poll(),'terminate_used':False,'kill_used':False}
            if process.poll() is None:
                detail['terminate_used'] = True
                process.terminate()
                try: process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    detail['kill_used'] = True
                    process.kill(); process.wait(timeout=5)
            detail['exit_code'] = process.poll()
            cleanup_details['children'].append(detail)
        cleanup_details['forced_cleanup'] = any(p['terminate_used'] or p['kill_used'] for p in cleanup_details['children'])
        cleanup_details['product_forced_cleanup'] = any(p['role']=='native' and (p['terminate_used'] or p['kill_used']) for p in cleanup_details['children'])
        for log in logs: log.close()
        wlan_samples.append(observer.snapshot()); wlan_close = observer.close()
        atomic(root/'wlan-observations.json', {'samples':wlan_samples,'observer_close_result':wlan_close,'requester':'native process, not this read-only observer'})
        atomic(root/'worker-result.json', {'completed': completed, 'cleanup_ok': all(p.poll() is not None for p in processes),
               'cleanup_details':cleanup_details,'error': error, 'session': session.value, 'pid': os.getpid(), 'child_pids': [p.pid for p in processes], **process_identity(product_process,moving,monitored_process), 'fixture_retention':{'resource_samples':len(samples),'wlan_samples':len(wlan_samples),'bounded_deadline_seconds':config['deadline_seconds']}, 'resources': samples})
    return 0 if completed else 1


if __name__ == '__main__':
    raise SystemExit(main())
