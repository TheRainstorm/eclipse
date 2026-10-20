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
