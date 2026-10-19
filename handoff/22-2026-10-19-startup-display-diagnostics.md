# 22 · 初连误激活与实机诊断

用户在工作电脑运行GUI，反馈KVM-WIN11初连只剩本地编号3，手动开启屏幕2后切屏仍卡“正在切换采集源”；连接x358h也失败，补充称为“等待WebRTC控制”。两个问题未闭环，不得把交接20/21的合成回环通过当作实机成功。继续不自动启动GUI、连接、输入或终止用户进程。

## 已修正的启动行为

`38b97df`：旧代码拿到初始清单，只要当前源的轨道为负值，就自动发CaptureConfig激活；首次设置发送后只要纹理未到，也再次调用切屏。这两个条件都不足以证明必须切源，会主动打断远端已在建立的默认采集。

启动现在优先观察已绑定的当前源、轨道0或其它已绑定源，最后才使用未绑定的当前源/主屏占位；初始编号为空和首帧尚未到不再触发主动切屏。保留用户明确选择屏幕与热插拔恢复路径。不因另一台主机还没建立CONTROL就改DTLS库或回退已验收补丁。

## 实际主机版本

只读SSH核对并下载到私有 `artifacts/display-hosts/`，不入库：

- KVM-WIN11：`E:\Program\Netease\GameViewer\bin\GameViewerServer.exe`，4.41.1.2335，SHA256 `2ed66758bfa72bab8b79779c974c37aff61ec2d8f56c6ab93eb066c46d87bdec`。
- x358h：`D:\Programs\GameViewer\bin\GameViewerServer.exe`，4.40.1.2090，SHA256 `6b45ee2d02a3b1238c6465d2568a3481eb8356e8c15e3523d1bed28d80aafc43`。

此前本地a88e…文件不是任一实际主机同一哈希。两台实际文件的Screen/CaptureChange/CaptureConfig/CaptureConfigResponse描述符字段一致，Windows client枚举均为3；真实二进制仍有flag8切源及CaptureChange分支。该结论不能代替运行时请求确已送达、回包内容及轨道状态的证据。WMI显卡枚举不等于活跃显示布局；SSH会话的EnumDisplayDevices返回空，不据此宣称显示器禁用或布局正确。

## 新增诊断

GUI每次连接优先在exe旁生成 `uu-session-<PID>-<毫秒时间>.jsonl`；不可写则回退配置目录的diagnostics。工作电脑若直接运行D盘共享目录exe，Linux端可以从x358h读取该文件。专用线程写文件，有界队列64条，单文件最多256KiB，不阻塞CONTROL/解码热路径。

只记录固定调用点的版本、显示模式和FPS、ICE/DTLS/SCTP/CONTROL状态变化、轨道名/index/mid、屏幕清单的ID/轨道/名字/类型/尺寸、选择请求、切屏响应、CaptureChange、画质确认和超时阶段。不记录输入/剪贴板、认证、SDP或candidate正文，不使用通用网络原文日志。

连接x358h的失败必须先看transport是否到ICE/DTLS/SCTP/CONTROL哪一步；KVM切屏必须检查select_display后有无对应display_reply、capture_change及settings_reply，而不是继续只猜通知缺失。用户最后一条补充的错误文本混入上一轮解释，未得到完整错误字符串，因此不要把它当作明确SCTP或ICE诊断。

## 验证与交付

核心46项与GUI29项通过后，新增“当前源未绑定而已有轨道可用”的启动回归单独通过（当前核心共47项）；Windows release成功。实际构建源 `38b97dfd48dd2c2b562e11f896cdfdfc8a01d5af` 已同步安装到 `D:\SMB\uu-gui-demo`，六项文件SHA256、source_commit、桌面UU Remote Rust目标及工作目录一致。GUI SHA256 `c71a38ce8c48ed4da16f2a61a4c7b737bcea9a11b7a7ac88f13be0c75ff0372d`。未终止用户旧窗口。

已异步请用户在工作电脑关闭旧主页/串流并重开38b97df，分别复现KVM切屏和x358h连接；需要继续收集真实元数据日志后定位。私有证据前缀为 `artifacts/ui-dpi-validation/display-startup-`，实际主机字段/分支为 `display-{kvm-win11,x358h}-proto.json` 和 `display-actual-{kvm,x358h}-static.txt`。
