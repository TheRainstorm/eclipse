网易UU远程 — Rust 原生客户端（Windows x64）

统一安装：
  - 正式目录固定为 %USERPROFILE%\uu-gui-demo，启动 uu-gui.exe。
  - 在新包目录执行 powershell -ExecutionPolicy Bypass -File .\install.ps1。
  - 安装器校验 version.json 的哈希，旧 rtx-auto / hud-recovery 与备份移到
    %USERPROFILE%\uu-gui-archive，桌面统一使用 UU Remote Rust 快捷方式。
  - 主页底部显示版本提交号，悬停可查看实际 exe 路径。

运行：
  1. 双击 uu-gui.exe；首次运行如有防火墙提示，允许 UDP/TCP。
  2. 首次启动显示二维码，用手机扫码登录。
  3. 左侧选择设备，点击“进入桌面”；连接后切到串流界面，设置图标可调码率。
  4. 界面按钮仅最大化窗口；Ctrl+Alt+F 切换全屏串流，Ctrl+Alt+S 切换 HUD；控制中心可调画质、分辨率和观看模式。
  5. 全屏悬浮入口可拖动；Ctrl+Alt+M 隐藏/恢复入口，菜单中“退出远控”断开。

解码：
  - 默认优先 D3D11VA（Media Foundation），失败自动回退 OpenH264。
  - Ctrl+Alt+S 显示顶部紧凑 HUD；悬停查看实际后端、回退原因及统计定义。
  - HUD 显示实际编码、Rx/De/Rd、RTT±波动、丢包、带宽、Render/Decode；保留丢包、重传、乱序、坏帧和恢复计数。
  - HUD 顶部居中，背景跟随文字范围，不显示远端未提供的 Encode 延迟。
  - Intel Arc B390 / AMD RX 7900 XTX 已实测；1080p 输出按可见尺寸裁剪。
  - 硬解目前仍有 GPU→CPU 回读，实测速度不一定优于软解。
  - 设置 UU_DECODER=sw 可强制软解；UU_DECODER=auto 恢复默认选择。
  - 当前实际帧率也取决于被控端；官方静态桌面会主动降帧。

离线自测（PowerShell，在本目录运行）：
  .\uu.exe probe --h264 C:\path\sample.h264
  .\uu.exe probe --h264 C:\path\sample.h264 --frame first-frame.ppm
  $env:UU_DECODER='sw'; .\uu.exe probe --h264 C:\path\sample.h264
  Remove-Item Env:UU_DECODER
  --frame 将首帧写成 PPM；探测同时打印尺寸、行跨距、帧数和最终后端。
  无输出帧或有解码错误时返回非零退出码。

注意：
  - libstdc++-6.dll 和 libgcc_s_seh-1.dll 必须与两个 exe 放在同一目录。
  - 登录态保存在 %USERPROFILE%\.local\share\uu-remote\state.json。
  - 键鼠走 CONTROL_DATA_CHANNEL 裸 JSON；Windows 路径中键值 16，支持按住拖动。
  - 默认官方自动画质、跟随远端分辨率；也可选 1080p/1440p/4K 或手动码率。
  - 观看模式不转发键鼠输入，悬浮菜单和设置操作不会穿透到远端。
  - 接收端有短时重排/NACK/RTX、坏帧丢弃和 IDR 参考链恢复；FEC 尚未接入。
  - Windows 硬解保留源 PTS，解码器重建不再把时间戳归零。
  - 连接后按实际视频物理像素 1:1 初始化窗口并居中；超出屏幕工作区时等比缩小。
  - 跨 DPI 屏幕移动时界面跟随系统缩放，窗口内视频画布保持物理尺寸并适配目标屏幕。
  - 非全屏时支持拖动四边/四角缩放窗口；确认画质后回到主菜单。
  - 退出会等待后台释放会话；取消连接和异常也执行清理。
  - 网络丢失不重建健康解码器；重传窗口随 RTT 调整，完整帧不额外等待。
  - 压缩帧和显示队列有界，新帧到达立即唤醒显示。
  - 垂直同步默认开启；UU_VSYNC=0 可关。
