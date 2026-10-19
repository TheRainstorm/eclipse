# 原生被控端采集进度

原生局域网模式正在分阶段实现；桌面采集后端不等于已经交付网络被控端。兼容客户端不依赖这些采集接口。

## X11

`uu-capture` 已支持已授权 X11 会话的显示器与窗口采集：RandR 活跃显示源与主屏、显式显示源选择、真实 BGRA 像素、行对齐及颜色掩码处理。当前使用 GetImage，不宣称共享内存或零拷贝。

在真实 X11 会话里可运行：

```bash
cargo run -p uu-capture --example capture_probe -- --list
cargo run -p uu-capture --example capture_probe -- --output /tmp/new-capture.ppm
```

诊断必须显式提供一个不存在的输出文件；不会默认保存画面，不覆盖已有文件。Unix 输出权限为600。`--monitor`按枚举名称选显示器，`--window`指定本机窗口ID，`--frames`最多300帧。

Wayland 下不把 Xwayland 根窗口误当真实桌面，也不调整桌面访问权限；后续通过 portal 请求屏幕共享授权。`--x11-display` 仅用于明确选择自己管理的 X11 会话，例如隔离测试服务器。

## Windows

桌面采集后端尚未交付；现有 Windows 硬件解码不等于已经支持原生被控端采集。完成真实采集、颜色与资源清理验证后再启用相应能力。
