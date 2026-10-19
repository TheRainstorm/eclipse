# egui-winit 0.29.1 局部补丁

来源：https://github.com/emilk/egui/tree/0.29.1/crates/egui-winit
上游许可证：MIT OR Apache-2.0（见 Cargo.toml / README）。

Linux 串流需要完整键盘按下、抬起事件。新增可选 preserve-key-events 特性，保留剪切、复制、粘贴 GUI 事件的同时发送 Key 事件；默认行为不变。剪贴板快捷键匹配排除 Alt，避免 Ctrl+Alt+X 被剪切拦截。由 Linux GUI 目标依赖启用；Windows 原生键盘路径保留。
