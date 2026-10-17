网易UU远程 — Rust 原生客户端（Windows x64）

运行：
  1. 双击 uu-gui.exe
  2. 首次运行如弹出 Windows 防火墙提示，请点“允许”（需要 UDP/TCP）
  3. 首次启动会显示二维码，用手机网易UU/微信扫码登录
  4. 左侧选择设备，点击底部“连接”
  5. 连接后可全屏、切换显示器；底部“断开”结束会话

注意：
  - libstdc++-6.dll 和 libgcc_s_seh-1.dll 必须与 uu-gui.exe 放在同一目录
  - 登录态保存在 %USERPROFILE%\.local\share\uu-remote\state.json
  - 远端画面已修复连续解码；键鼠走 CONTROL_DATA_CHANNEL 裸 JSON（已实测可用）
  - 垂直同步默认开启（无撕裂）；UU_VSYNC=0 可关

帧率说明：
  - 客户端用进程内 OpenH264 软件解码，1080p 约 2.9ms/帧（几百 fps 余量），
    解码不是瓶颈；实际帧率主要取决于被控端发送的帧率（静态桌面时官方被控端
    会跳帧到 ~20fps，画面有运动时才拉高）。
  - 已移除外部 ffmpeg 子进程解码（延迟太高、不跟手）。
  - 硬件解码（Windows Media Foundation）规划为进程内后端，后续加入。
  - 按 F1 或顶栏 HUD 按钮可显示串流统计（源/解码/渲染 fps、码率、解码耗时、丢包）。
