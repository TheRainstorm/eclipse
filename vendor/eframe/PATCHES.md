# Local changes

Based on eframe 0.29.1 from crates.io, under its original MIT OR Apache-2.0 license.

`request_vsync` / `vsync_result` allow changing the swap interval of the current
native Glow viewport. Requests are applied through glutin with the owning surface
current, before the next swap, for both deferred and immediate viewports.
Failures are returned to the application. Surface recreation reapplies the last
request. The default NativeOptions setting is otherwise unchanged.
