# Local change to winit 0.30.13

Upstream: https://github.com/rust-windowing/winit/tree/v0.30.13
The original Apache-2.0 license and copyright notices remain in this directory.

Only the Wayland `Window::current_monitor` selection changes. During a move, a surface may remain entered on its old and new outputs. Upstream picks the first output even after the surface's preferred scale changes. This combines the old output mode with the new DPI in egui's monitor-size calculation, so an 80% window can exceed its destination's intended size.

Prefer an entered output whose integer `wl_output` scale agrees with the ceiling of the surface's preferred scale; ties prefer the later entered output. This stays within the surface's entered outputs and does not guess from every output on the desktop. Fractional surface and integer output scales are intentionally distinct; see [upstream discussion](https://github.com/rust-windowing/winit/issues/3183).

Window positioning remains the Wayland compositor's responsibility. The fix requests no additional permissions, does not connect to a compositor IPC service, and does not change Windows or X11 backends. Validate using the owned two-output, mixed-DPI native viewer smoke test before updating this dependency.
