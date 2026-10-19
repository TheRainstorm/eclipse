# 交接文档索引

按时间顺序编号，数字越大越新。新交接文档请接着编号，不要覆盖旧文档。

| 编号 | 文件 | 内容 |
|---|---|---|
| 01 | `01-base-handover.md` | 总纲：协议、登录、信令、输入、环境与机器表、历史逆向结论 |
| 02 | `02-2026-10-17-rust-turn.md` | TURN/PCK 打通，端到端首次出画面 |
| 03 | `03-2026-10-17-input.md` | 键鼠输入打通（echo 握手、输入通道） |
| 04 | `04-2026-10-18-bitrate.md` | 串流专用 UI + 码率控制（CaptureSettingRequest 字段含义） |
| 05 | `05-2026-10-19-hardware-decode.md` | 原生硬件解码：Windows D3D11VA / Linux VA-API + 回退与自测工具 |
| 06 | `06-2026-10-19-hardware-validation.md` | Intel/AMD 实机硬解验证、1088→1080 裁剪修复、Windows 分发包；x358h 新地址 |
| 07 | `07-2026-10-19-experience.md` | Windows 中键实测值 16、浅色参考版 GUI、RTP/NACK 恢复与显示队列优化；FEC 边界 |
| 08 | `08-2026-10-19-reference-index.md` | 功能拆分提交、三个本地项目源码参考索引、协议差异与后续实施顺序 |
| 09 | `09-2026-10-19-controls-rtx-auto.md` | 可隐藏悬浮菜单、官方自动档达18Mbps、RTX接收与严格恢复、Windows真实PTS |
| 10 | `10-2026-10-19-resize-hud-recovery-exit.md` | 窗口缩放、Moonlight HUD、侧边菜单确认、恢复竞争/高吞吐优化、退出清理 |

约定：

- 当前 x358h SSH 地址是 `192.168.34.3:2202`（2026-10-19 用户更新）；历史文档中的 `192.168.35.200` 已失效。

- 协议与逆向证据从 01 开始读；后续实测更正优先于旧结论。功能参考入口见 [`docs/reference-index.md`](../docs/reference-index.md)。
- 迁移前的旧文件名（仓库里已不存在，仅供检索历史记录和旧对话时对照）：
  `HANDOVER.md` → 01，`HANDOVER-RUST-TURN-2026-10-17.md` → 02，
  `HANDOVER-RUST-INPUT-2026-10-17.md` → 03，
  `HANDOVER-RUST-BITRATE-2026-10-18.md` → 04。
