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
- Windows: coalesce application redraw requests into one posted notification per
  window, including hidden coordinators. Drawing runs through `RedrawRequested`
  instead of a repaint producer or low-priority `WM_PAINT`; OS exposure and
  resize paints still use their native path. Reentrant redraws are buffered and
  deduplicated, buffered work wakes the loop without changing its idle policy,
  and each buffer drain yields after a redraw. Hardware input is dispatched
  before posted frame messages so animation cannot indefinitely precede it.

Windows API contracts:
[SetForegroundWindow](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setforegroundwindow),
[AllowSetForegroundWindow](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-allowsetforegroundwindow).
[PostMessage](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-postmessagew),
[PeekMessage](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-peekmessagew).
