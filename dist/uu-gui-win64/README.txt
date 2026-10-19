网易UU远程 — Rust 原生客户端（Windows x64）

运行：
  1. 双击 uu-gui.exe；首次运行如有防火墙提示，允许 UDP/TCP。
  2. 首次启动显示二维码，用手机扫码登录。
  3. 左侧选择设备，点击“进入桌面”；连接后切到串流界面，设置图标可调码率。
  4. Ctrl+Alt+F 切换全屏，F1 切换 HUD；顶栏返回或 × 图标断开会话。

解码：
  - 默认优先 D3D11VA（Media Foundation），失败自动回退 OpenH264。
  - HUD 显示实际后端、GPU 和回退原因。
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
  - 默认自定义码率 55 Mbps；观看模式不转发键鼠输入。
  - 接收端有短时重排/NACK、坏帧丢弃和按需关键帧恢复；FEC 尚未接入。
  - 压缩帧和显示队列有界，积压时优先显示最新画面。
  - 垂直同步默认开启；UU_VSYNC=0 可关。
