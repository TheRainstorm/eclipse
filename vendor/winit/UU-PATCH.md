# Local winit patches

This vendored winit retains its upstream license. Local platform changes:

- X11: tolerate a window being destroyed before its geometry is queried.
- Wayland: retain the intended size when moving between displays with different scales.
- Windows: window activation calls `SetForegroundWindow` without synthesizing
  an Alt press/release. The upstream activation helper also runs on explicit
  focus requests; injecting Alt changes held modifiers and reaches streaming
  keyboard hooks. The GUI launcher delegates foreground permission to newly
  launched stream processes with `AllowSetForegroundWindow` and records native
  activation results. Windows can still refuse activation; no input is injected
  to bypass that decision.

Windows API contracts:
[SetForegroundWindow](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setforegroundwindow),
[AllowSetForegroundWindow](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-allowsetforegroundwindow).
