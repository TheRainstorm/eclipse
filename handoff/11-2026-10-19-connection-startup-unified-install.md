# 11 · CONTROL 启动回归修复与 Windows 单一安装入口

日期：2026-10-19。用户反馈“连接不上”，实际错误为
`CONTROL channel did not accept initial capture settings`；并要求统一最新版本位置。
本交接优先于交接10的安装位置说明。

## 当前唯一正式入口

- x358h：`C:\Users\yfy\uu-gui-demo\uu-gui.exe`。
- 桌面快捷方式：`UU Remote Rust`，目标为上述 exe，工作目录为其父目录。
- 当前程序源版本：`e8def9d477dc`，主页底部显示；悬停显示真实 exe 路径，原生窗口标题也带版本。
- `version.json` 记录完整源提交、构建时间和每个分发文件的 SHA-256。
- 原 `rtx-auto`、`hud-recovery` 目录以及根目录的 `.pre-*`、`.before-*` exe 均移出正式目录，归档在
  `C:\Users\yfy\uu-gui-archive\20261019-134715-874\`。正式目录只保留一套 exe/DLL、README、安装脚本和 manifest。
- 仓库分发包仍为 `dist/uu-gui-win64/`；不要再创建多份可供用户启动的版本目录。

## 根因及修复

交接10末尾减少设置重发时，新增了致命的 `ensure!(setting_sent, ...)`。
SDP answer 到达不代表 ICE/DTLS/SCTP 已就绪。旧代码接着固定睡眠3.5秒，再尝试5次发送画质设置；
整个过程没有继续读取后续 `soac:candidate`。如果建立连接依赖这些迟到候选，CONTROL 无法打开，
随后设置发送失败又会把整个会话断开。此前同局域网的快速协商没有覆盖这个时序。

`6504045` 修复为：

- answer 后立即进入同一事件循环，持续处理 trickle ICE、信令和取消。
- 以真实 `RTCDataChannelState::Open` 判断 CONTROL 就绪；不再把“设置发不出去”当作连接失败。
- 通道打开并给 echo/初始化1.5秒后发送初始设置，失败通过计时器稍后重试，不阻塞协商。
- 运行时设置保留最新待发值；真正接通后才显示“已连接”。
- 等待控制通道有45秒上限，并区分真实 ICE/DTLS Failed；SDP answer 的等待也改为完整45秒期限，不再被每次10秒提前截断。
- 新测试用真实的本地 WebRTC/Socket.IO，把双方 SDP 候选移除、候选事件故意延后8秒（超出旧代码失败期限），验证之后仍能建立 CONTROL、收到设置并正常退出。

## 统一打包与安装

`e8def9d` 增加：

- `python3 tools/windows/package.py`：统一构建 GUI/CLI，拒绝有未提交源改动的包，生成分发目录与校验 manifest。
- 在 Windows 新包目录执行：`powershell -ExecutionPolicy Bypass -File .\install.ps1`。
- `install.ps1` 先校验所有包文件，再安装到固定目录、归档旧副本、修正本客户端快捷方式并创建桌面入口。
- 不停止用户进程；已在测试目录验证旧目录迁移、重复安装和哈希一致性。
- GUI 读取 exe 同目录 manifest 显示版本，没有 manifest 的开发构建明确显示“开发构建”。

## 验证与限制

- `cargo test --workspace --locked`：86项通过，包括新增的8秒延后 ICE 回归测试。
- Windows GUI/CLI release 构建通过，安装脚本在 x358h 实际执行成功，正式目录只保留一份。
- 从正式路径实际启动GUI，`UU_FORCE_JOIN=0`、明确连接 KVM-WIN11（`aeawvmepembcpca5`），确认 peer connected、CONTROL open、连续解码出帧；截图显示实际1920×1080画面，约22fps（低变化桌面），无人工丢包。
- 正式程序标题包含 `e8def9d477dc`，WM_CLOSE 后正常退出；随后从同一正式路径再次连接 KVM，peer/CONTROL/解码和正常退出第二次全部通过。
- 测试只使用 KVM-WIN11，未连接 FYYUAN，未结束官方客户端。
- 不代表解决交接10的高变化画面恢复停顿；本轮针对启动连接回归和版本分发混乱。

GUI SHA-256：`85a35b9a0940a5c10efd5e5bc286d9b427fd6698727ee38c1b6ca220e679b799`。
CLI SHA-256：`eca670ac5d33aca4c02593f550d181476d81d553262e2cf426582437f00fced9`。
私有实机截图与结果在 `artifacts/menu-stream-validation/unified-*`，不提交真实桌面内容。
