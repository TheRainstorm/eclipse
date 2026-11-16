#!/usr/bin/env python3
"""Generate repeatable mouse motion for a foreground Windows test window.

Only absolute mouse moves are injected. The two-second triangular path stays
between 25% and 75% of the client width, at 55% of its height. This is a workload
generator, not an input or display latency measurement. No coordinates, window
titles or key contents are written to the report.
"""
import argparse
import ctypes
import json
import math
import os
from pathlib import Path
import statistics
import sys
import time


def interval_summary(times):
    values = sorted((b - a) * 1000 for a, b in zip(times, times[1:]))
    if not values:
        return {"count": 0}
    return {"count": len(values), "mean_ms": statistics.fmean(values),
            **{name: values[math.ceil(len(values) * q) - 1]
               for name, q in [("p50_ms", .5), ("p95_ms", .95), ("p99_ms", .99)]},
            "max_ms": values[-1]}


def run_probe(api, seconds, hz, clock=time.perf_counter, sleep=time.sleep):
    """API/clock injection keeps cancellation and pacing testable without input."""
    start = clock()
    deadline = start + seconds
    tick, skipped = 0, 0
    times, last = [], None
    restored = False
    reason, error = "completed", None
    try:
        initial = api.cursor()
        while clock() < deadline:
            due = start + tick / hz
            if clock() < due:
                # Check focus and user takeover even at a low requested rate.
                reason = api.stop_reason()
                if reason:
                    break
                remaining = due - clock()
                if remaining > 0:
                    sleep(min(remaining, .01))
                continue
            reason = api.stop_reason()
            if reason:
                break
            x, y, width, height = api.client_rect()
            reason = api.stop_reason()
            if reason:
                break
            now = clock()
            if now >= deadline:
                reason = "completed"
                break
            phase = 1 - abs(((now - start) % 2) - 1)
            point = (round(x + (width - 1) * (.25 + .5 * phase)),
                     round(y + (height - 1) * .55))
            if not api.target_at(point):
                reason = "target_obscured_or_constrained"
                break
            api.move(point)
            last = point
            times.append(clock())
            # Advance past missed opportunities; never replay an input backlog.
            following = max(tick + 1, math.floor((clock() - start) * hz) + 1)
            skipped += following - tick - 1
            tick = following
        if reason is None:
            reason = "completed"
        if reason == "completed" and last is not None:
            reason = api.stop_reason() or "completed"
            # SendInput acceptance does not prove immediate cursor position.
            # A mismatch prevents restoration; it never proves user takeover.
            if reason == "completed" and api.cursor() == last:
                api.move(initial)
                restored = True
    except KeyboardInterrupt:
        reason = "interrupted"
    except (OSError, RuntimeError) as exc:
        reason, error = "api_error", str(exc)
    elapsed = clock() - start
    return {"workload_only": True, "requested_seconds": seconds, "requested_hz": hz,
            "elapsed_seconds": elapsed, "moves": len(times), "skipped_ticks": skipped,
            "actual_hz": (len(times) - 1) / (times[-1] - times[0])
            if len(times) > 1 and times[-1] > times[0] else None,
            "intervals": interval_summary(times), "stop_reason": reason,
            "cursor_restored": restored, "error": error}


class WindowsApi:
    def __init__(self):
        from ctypes import wintypes as w

        class MouseInput(ctypes.Structure):
            _fields_ = [("dx", w.LONG), ("dy", w.LONG), ("mouseData", w.DWORD),
                        ("dwFlags", w.DWORD), ("time", w.DWORD), ("dwExtraInfo", ctypes.c_size_t)]

        class Payload(ctypes.Union):
            _fields_ = [("mouse", MouseInput)]  # Largest INPUT union member.

        class Input(ctypes.Structure):
            _fields_ = [("type", w.DWORD), ("payload", Payload)]

        self.w, self.input_type, self.mouse_type, self.payload_type = w, Input, MouseInput, Payload
        self.user = ctypes.WinDLL("user32", use_last_error=True)
        signatures = {
            "SetThreadDpiAwarenessContext": ([ctypes.c_void_p], ctypes.c_void_p),
            "GetForegroundWindow": ([], w.HWND),
            "GetAncestor": ([w.HWND, w.UINT], w.HWND),
            "GetWindowThreadProcessId": ([w.HWND, ctypes.POINTER(w.DWORD)], w.DWORD),
            "IsWindow": ([w.HWND], w.BOOL), "IsWindowVisible": ([w.HWND], w.BOOL),
            "IsIconic": ([w.HWND], w.BOOL), "GetAsyncKeyState": ([ctypes.c_int], ctypes.c_short),
            "GetCursorPos": ([ctypes.POINTER(w.POINT)], w.BOOL),
            "WindowFromPoint": ([w.POINT], w.HWND),
            "GetClipCursor": ([ctypes.POINTER(w.RECT)], w.BOOL),
            "GetClientRect": ([w.HWND, ctypes.POINTER(w.RECT)], w.BOOL),
            "ClientToScreen": ([w.HWND, ctypes.POINTER(w.POINT)], w.BOOL),
            "GetSystemMetrics": ([ctypes.c_int], ctypes.c_int),
            "SendInput": ([w.UINT, ctypes.POINTER(Input), ctypes.c_int], w.UINT),
        }
        for name, (arguments, result) in signatures.items():
            function = getattr(self.user, name)
            function.argtypes, function.restype = arguments, result
        # Per-monitor-v2 awareness makes all queried geometry physical pixels.
        self.old_dpi = self.user.SetThreadDpiAwarenessContext(ctypes.c_void_p(-4))
        if not self.old_dpi:
            raise ctypes.WinError(ctypes.get_last_error())

    def bind(self, pid, hwnd):
        self.hwnd = hwnd or self.user.GetForegroundWindow()
        self.pid = self.window_pid()
        if not self.pid or (pid is not None and self.pid != pid):
            raise RuntimeError("The requested process is not the foreground window")
        self.root = self.user.GetAncestor(self.hwnd, 2)  # GA_ROOT; child HWNDs are allowed.

    def close(self):
        self.user.SetThreadDpiAwarenessContext(self.old_dpi)

    def window_pid(self):
        pid = self.w.DWORD()
        self.user.GetWindowThreadProcessId(self.hwnd, ctypes.byref(pid))
        return pid.value

    def stop_reason(self):
        if (not self.user.IsWindow(self.hwnd) or not self.user.IsWindowVisible(self.hwnd)
                or self.user.IsIconic(self.root) or self.window_pid() != self.pid):
            return "target_lost"
        if self.user.GetForegroundWindow() != self.root:
            return "focus_lost"
        # Held mouse buttons, Shift/Ctrl/Alt and both Windows keys.
        if any(self.user.GetAsyncKeyState(key) & 0x8000
               for key in (1, 2, 4, 5, 6, 0x10, 0x11, 0x12, 0x5B, 0x5C)):
            return "controls_held"
        return None

    def cursor(self):
        point = self.w.POINT()
        if not self.user.GetCursorPos(ctypes.byref(point)):
            raise ctypes.WinError(ctypes.get_last_error())
        return point.x, point.y

    def client_rect(self):
        rect, origin = self.w.RECT(), self.w.POINT()
        if (not self.user.GetClientRect(self.hwnd, ctypes.byref(rect))
                or not self.user.ClientToScreen(self.hwnd, ctypes.byref(origin))):
            raise ctypes.WinError(ctypes.get_last_error())
        if rect.right < 4 or rect.bottom < 4:
            raise RuntimeError("Target client area is unavailable")
        return origin.x, origin.y, rect.right, rect.bottom

    def target_at(self, point):
        # Foreground ownership alone does not exclude another topmost window.
        hit = self.user.WindowFromPoint(self.w.POINT(*point))
        if not hit or self.user.GetAncestor(hit, 2) != self.root:
            return False
        clip = self.w.RECT()
        if not self.user.GetClipCursor(ctypes.byref(clip)):
            raise ctypes.WinError(ctypes.get_last_error())
        return clip.left <= point[0] < clip.right and clip.top <= point[1] < clip.bottom

    def move(self, point):
        left, top, width, height = (self.user.GetSystemMetrics(i) for i in (76, 77, 78, 79))
        x, y = point
        if width < 1 or height < 1 or not (left <= x < left + width and top <= y < top + height):
            raise RuntimeError("Requested motion lies outside the virtual desktop")
        # Aim at pixel centers in the normalized 0..65535 virtual desktop.
        dx = min(65535, ((x - left) * 65536 + 32768) // width)
        dy = min(65535, ((y - top) * 65536 + 32768) // height)
        mouse = self.mouse_type(dx, dy, 0, 0x0001 | 0x8000 | 0x4000, 0, 0)
        event = self.input_type(0, self.payload_type(mouse=mouse))
        if self.user.SendInput(1, ctypes.byref(event), ctypes.sizeof(event)) != 1:
            raise RuntimeError("SendInput did not accept the mouse move")


def bounded_number(low, high):
    def parse(value):
        number = float(value)
        if not math.isfinite(number) or not low <= number <= high:
            raise argparse.ArgumentTypeError(f"must be finite and between {low} and {high}")
        return number
    return parse


def target_number(value):
    number = int(value, 0)
    if not 1 <= number <= 0xFFFFFFFFFFFFFFFF:
        raise argparse.ArgumentTypeError("must be a positive decimal or hexadecimal identifier")
    return number


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    target = parser.add_mutually_exclusive_group(required=True)
    target.add_argument("--pid", type=target_number, help="bind the foreground window of this process after the countdown")
    target.add_argument("--hwnd", type=target_number, help="explicit target HWND (decimal or 0x hexadecimal)")
    parser.add_argument("--seconds", type=bounded_number(.1, 300), default=25, help="motion duration, 0.1..300 seconds (default: 25)")
    parser.add_argument("--hz", type=bounded_number(1, 1000), default=120, help="requested motion rate, 1..1000 Hz (default: 120)")
    parser.add_argument("--start-delay", type=bounded_number(0, 30), default=3, help="seconds to focus the target before starting, 0..30 (default: 3)")
    parser.add_argument("--output", type=Path, help="optional JSON timing report; contains no coordinates or key contents")
    args = parser.parse_args(argv)
    if args.pid is not None and args.pid > 0xFFFFFFFF:
        parser.error("--pid exceeds the Windows process identifier range")
    if os.name != "nt":
        parser.error("Mouse injection is supported only on Windows; --help works on other systems")
    api = None
    try:
        api = WindowsApi()
        print(f"Starting in {args.start_delay:g} seconds. Focus the target and release all buttons/modifiers. Press a mouse button/modifier or switch focus to stop.", file=sys.stderr)
        time.sleep(args.start_delay)
        api.bind(args.pid, args.hwnd)
        report = run_probe(api, args.seconds, args.hz)
    except (OSError, RuntimeError, KeyboardInterrupt) as exc:
        report = {"workload_only": True, "moves": 0, "cursor_restored": False,
                  "stop_reason": "interrupted" if isinstance(exc, KeyboardInterrupt) else "api_error", "error": str(exc)}
    finally:
        if api is not None:
            api.close()
    encoded = json.dumps(report, indent=2, ensure_ascii=False) + "\n"
    print(encoded, end="")
    if args.output:
        try:
            args.output.write_text(encoded, encoding="utf-8")
        except OSError as exc:
            print(f"Cannot save report: {exc}", file=sys.stderr)
            return 2
    return 0 if report["stop_reason"] == "completed" else 1


if __name__ == "__main__":
    raise SystemExit(main())
