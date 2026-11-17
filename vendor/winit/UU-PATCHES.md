# Local change to winit 0.30.13

Upstream: https://github.com/rust-windowing/winit/tree/v0.30.13
The original Apache-2.0 license and copyright notices remain in this directory.

## Wayland monitor selection

During a move, a surface may remain entered on its old and new outputs. Upstream picks the first output even after the surface's preferred scale changes. This combines the old output mode with the new DPI in egui's monitor-size calculation, so an 80% window can exceed its destination's intended size.

Prefer an entered output whose integer `wl_output` scale agrees with the ceiling of the surface's preferred scale; ties prefer the later entered output. This stays within the surface's entered outputs and does not guess from every output on the desktop. Fractional surface and integer output scales are intentionally distinct; see [upstream discussion](https://github.com/rust-windowing/winit/issues/3183).

Window positioning remains the Wayland compositor's responsibility. This monitor-selection fix requests no additional permissions, does not connect to a compositor IPC service, and does not change Windows or X11 monitor selection. Validate using the owned two-output, mixed-DPI native viewer smoke test before updating this dependency.

## X11 position queries during destruction

The X server can destroy a window while input/redraw events for it remain queued in the application. The upstream `inner_position_physical` unwraps `TranslateCoordinates`, so a late geometry query can panic on `BadWindow`. This was reproduced by destroying only an owned fixture window inside an authenticated Xvfb.

Successful queries are unchanged. Failed queries use the cached inner position if immediately available, or `(0, 0)` during teardown; `try_lock` avoids blocking an internal caller that already holds the shared-state lock. This fallback does not claim the window is still alive or move it.

Regression: build `uu-gui` example `x11_destroyed_window_probe`, then run `tools/native/input_smoke.py --probe ... --report-field window --output ...`. The probe requires the runner's owned Xvfb, creates its own hidden window, destroys that exact window, and repeats public position queries. It never sends input, captures the desktop, or accesses a clipboard.

## Windows 11 mixed-DPI window placement

Backport the Windows placement policy from upstream [winit #4341](https://github.com/rust-windowing/winit/pull/4341), particularly commit [`9d791c97239d07cfeda5d978126634759bf2639f`](https://github.com/rust-windowing/winit/commit/9d791c97239d07cfeda5d978126634759bf2639f). The original upstream code and this adaptation remain under the vendored Apache-2.0 license; copyright and license notices are retained.

On Windows 11 and newer, `WM_DPICHANGED` uses the operating system's suggested outer rectangle directly. The older conservative-size/monitor-nudging workaround can send a window back across a monitor boundary, producing repeated DPI changes and incorrect growth. Only a positively identified Windows 10 build below 22000 retains that workaround, matching the upstream policy. The existing `RtlGetVersion` build cache is shared with dark-mode detection, with its structure size initialized according to the API contract. No keyboard-combination or device-specific handling is added.

The `ScaleFactorChanged` event and its `InnerSizeWriter` are still delivered. As in upstream #4341, the modern Windows placement branch uses the suggested rectangle instead of a callback-overridden size; see the upstream [InnerSizeWriter regression #4721](https://github.com/rust-windowing/winit/issues/4721). The current egui integration does not override that writer. A future consumer requiring a custom DPI-change size needs a separately verified adaptation rather than silently changing this backport's placement policy.

Validate with owned native windows moving in both directions between monitors with different DPI, including a window's suggested rectangle, final monitor/size, and DPI-event sequence. Model or synthetic-message tests alone do not verify Windows Shell migration behavior. Keep normal, maximized, and borderless-fullscreen restoration checks separate.
