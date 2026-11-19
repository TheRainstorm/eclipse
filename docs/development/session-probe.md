# 后台连接与切屏探测

`session_probe` 复用 GUI 的 `controller::spawn_with_stats`，在开发机后台核对真实被控端，不需要启动 Windows 窗口：

```sh
cargo run -p uu-core --example session_probe -- <设备ID>
cargo run -p uu-core --example session_probe -- <设备ID> <屏幕源ID> <屏幕源ID>
```

使用当前配置目录的既有登录。默认保持被控端显示布局，静音、不启用剪贴板、不发送键鼠；只有显式给出的屏幕源 ID 才触发采集切换。源 ID 来自日志里的 `screens[].id`，不是 GUI 的本地标签编号，也不是 Windows 显示器编号。连接会占用一个官方主控会话；只对已授权的测试机器运行。

初连要求 CONTROL 打开、视频解码且初始画质请求成功。每次切屏要求相应的切屏回复成功、目标源有轨道绑定、定向画质请求成功，随后至少继续解码 5 帧。输出 `probe {...}` 汇总，不包含认证、输入、剪贴板和 SDP 正文。`artifacts/live-display-validation/uu-session-*.jsonl` 记录屏幕、回包通道和采集源通知；即使失败也先释放连接，报告 `cleanup stopped=true`。

2026-10-19 实测开发机 → Windows 测试主机：原生 AMD vDisplay（ID 0）与 Zako HDR（ID 1），`0 1 0 1` 往返四次全部通过，解码错误 0、清理完成。实际回包中 `CaptureChange`、`CaptureConfigResponse` 和 `CaptureSettingResponse` 均来自 TEXT_DATA_CHANNEL；屏幕清单同时出现在 CONTROL 与 TEXT。

该程序验证实际连接/协议/解码，不代替 Windows GUI、键鼠、DPI、音频或帧间隔的人机验收。此前 `stream_check` 的强制复用会话（UU_FORCE_JOIN=0）与默认新会话行为不同；它的性能结果不能代替本探测的显示策略和切屏验收。
