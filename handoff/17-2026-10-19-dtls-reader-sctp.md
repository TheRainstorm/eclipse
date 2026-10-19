# 17 · DTLS接收竞态导致SCTP长期connecting

日期：2026-10-19。用户在交接16版本上再次报告：`ICE=connected，DTLS=connected，SCTP=connecting，CONTROL=connecting`，45秒后失败，官方UU正常。这说明上轮恢复连接初始化并未解决实机问题，不能再归因于菜单焦点、超级屏、权限或心跳；SCTP尚未打开，应用层输入还不能工作。

## 已定位并复现的底层缺陷

只读检查参考仓库`/home/yfy/repo/uu-remote-linux`的DTLS补丁，并核对本项目真正依赖的crates.io `webrtc-dtls 0.10.0`后发现：

1. DTLS接收任务读到握手记录，把一条通知送入握手状态机。
2. 状态机完成、设置握手成功、关闭完成通道并返回；它的结构仍持有通知接收端，未处理的确认发送端仍可留在队列中。
3. 原来的内部等待只监听通知确认和处理队列，**没有监听握手完成**。外层select即使监听完成，也无法打断已经进入的内部循环。
4. 唯一DTLS接收任务由此永久等待，后续加密应用数据不再向SCTP投递；SRTP视频接收路径独立，仍可更新，状态也仍显示DTLS connected。

这是代码与确定性测试证明的缺陷，与用户报告的分层状态吻合。用户在交付补丁版本后明确回复“恢复了”，本次键鼠失效的修复已获实机确认。没有通过延长超时或自动重连掩盖问题。

## 已提交的修复

`ff2ffba`：以crates.io原始发布包建立`vendor/webrtc-dtls`，Cargo patch固定到本地副本，生产依赖仍为0.10.0。
内部等待增加完成通知监听；外层在完成与通知同时就绪时优先退出过期等待。证书/指纹校验、加密、重放检测和SCTP协议均保持原有实现。
仅功能修改`src/conn/mod.rs`，另加本项目编写的回归用例；来源、许可证、修改范围和命令见[`UU-PATCH.md`](../vendor/webrtc-dtls/UU-PATCH.md)。参考项目只读，没有直接整库导入其GPL项目或旧rustls改动。

Windows打包完整性检查已包含vendor，避免未提交的依赖修改被错误标为某个源版本；分发README保留所采用MIT条款。此轮不改菜单、FPS范围、窗口或超级屏功能范围。

## 验证结果

- 新回归用例通过通道屏障，强制“通知已发送 → 握手结束 → 确认发送端仍存活”，再发送真实AES-GCM加密的合成应用数据。
- **修复前同一测试稳定失败**：300ms内收不到应用数据，失败位置为`completed DTLS must continue reading SCTP`。修复后立即通过。
- 上游正常双端DTLS握手、收发与关闭用例通过。
- 本项目迟到ICE回环通过：CONTROL最终打开，同时送达画质设置、输入心跳、鼠标及键盘消息。
- `cargo tree -i webrtc-dtls --locked`确认GUI和CLI均使用本地补丁；root Cargo.lock只替换该包的来源，没有借机整体升级WebRTC。

日志（私有，不入库）：`artifacts/ui-dpi-validation/dtls-race-before.log`、`dtls-race-after.log`、`dtls-application-after.log`、`dtls-control-startup.log`。

用户负责实机测试，本轮不自动启动GUI、不自动连接设备、不注入输入、不终止用户进程。统一安装入口继续为x358h `192.168.34.3:2202`的`C:\Users\yfy\uu-gui-demo\uu-gui.exe`，桌面`UU Remote Rust`；源版本与SHA256以分发包version.json为准。安装后用户关闭旧主页和串流窗口，重新打开才能使用新DTLS代码。

## 已交付版本

Windows release构建通过，已安装源版本`f74fc9db5f1c0967dd5a04b8cb79e03f5de1471b`。六个manifest文件均与本地包一致，快捷方式目标/工作目录已核对；旧版归档、暂存安装文件清理完成，没有关闭用户窗口。

GUI SHA256：`26e6aae7eb55a606402a95e18b6a25c7654d4d6f101a4c59f46e234d4e1913a3`。
CLI SHA256：`c08b169d0b8c8771b04d04bcc241b63aad0ea6125beffb895c0dda086d13688d`。
私有构建与安装证据：`artifacts/ui-dpi-validation/dtls-reader-package.log`、`dtls-reader-installed-verified.json`。

## 用户实机验收

2026-10-19，交付`f74fc9d`后，用户回复“恢复了”，确认本次串流键鼠已恢复。此结果与修复前反例、补丁后DTLS/CONTROL回环共同构成闭环证据。
本条验收覆盖本次输入恢复；60/120实际帧率、菜单外观与长时间稳定性结果另行记录。此次仅更新文档，不重建或替换已验收的Windows程序。
