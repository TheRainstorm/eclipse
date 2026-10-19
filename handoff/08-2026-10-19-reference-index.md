# 08 · 功能提交与开源实现参考索引

日期：2026-10-19。当前 HEAD 以 `git log -1` 为准。

用户要求每完成一个功能就 git 提交；后续沿用这个粒度，不再积累到交接末尾。
这轮把此前积累的改动按可编译、可验证的功能拆开，保留了最终工作区内容。

| 提交 | 内容 |
|---|---|
| `aa1185a` | 原有四份交接文档移动到 handoff/ |
| `9c20bbd` | Windows 画质请求完整字段、时间戳、自定义码率 bps |
| `6c90b7c` | 硬解选择/软回退、NV12 可见尺寸/裁剪、probe |
| `e66bff8` | RTP 重排、坏 AU 丢弃、NACK/PLI/FIR 恢复、有界解码/显示 |
| `43a259d` | Windows 中键 16 与真实按住/拖动/抬起、失焦清理 |
| `ad89c4e` | UU 参考布局的浅色主页、串流控件、只读/设置输入隔离 |
| `d3219d1` | Windows 分发包、交接 05–07、串流说明 |

拆分时用独立暂存快照检查依赖：码率修改 workspace 全目标编译、control_wire 4 测试；
硬解 media 30 + CLI 3 测试；接收 core 17 + 回环 2 + media 31；中键 GUI 2。
最终源码 `cargo test --workspace --locked` 共 74 项通过。Windows release 和远端测试沿用
交接 07 的最终二进制及哈希，本次索引文档不改变二进制。

## 源码索引

新增 [`docs/reference-index.md`](../docs/reference-index.md)，覆盖 Foundation Sunshine、
Moonlight V+（含固定版本 common-c）、uu-remote-linux，并补充 moonlight/deskflow。
按编号记录约 40 项能力：源码文件/符号/行号、实现机制、适用范围、本项目落点和验收方法。
原始参考工作区均只读，版本 SHA 及更新 diff 命令在索引中。

Moonlight 原目录的 common-c 子模块为空。本次在 `/tmp/uu-reference-moonlight-common-c`
独立检出 gitlink 固定的 `8a5d9562f2d557f4acf6e2f722e86502a68e5dd7` 阅读网络实现，
没有初始化或修改用户的参考工作区；索引对应链接固定到该 SHA。

## 关键结论与下一步

- 原中键同时存在错误值 3 与只在 click 结束合成 press/release 两个问题。
  当前 Windows 路径实测 16；U 的 viewer 仍用 4，不可把同协议项目视为所有字段都适配。
- Foundation 的 RS FEC、自动保护和总带宽预算值得参考，但需要发送端提供正确校验包。
  当前 UU 仍无 FEC/完整 RTX；U 的 vendor RTX 路径同样只读取后丢弃。
- 优先做可重复丢包/乱序/恢复测试与分阶段指标，确认官方 NACK/修复格式的实际作用。
- 然后实现 Moonlight 式 GPU 呈现与显示节拍，及规则型自动码率；每个功能独立验收提交。
- 音频、光标、剪贴板、文件/终端/端口映射优先参考 U 的同协议实现；能力未完成前不启用。
- 原始项目的许可与本项目不同；索引只记录技术思路和引用，未导入其实现代码。

x358h 可继续测试：`192.168.34.3:2202`。仍只向 KVM-WIN11 发起测试连接；
不得连接 FYYUAN-5NET3-53。软解弱网卡顿、参考帧误差、VAAPI 真机验收等限制见交接 07。
