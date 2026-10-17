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

帧率与硬件解码：
  - 客户端软件解码（OpenH264）在本机 1080p 有约 200fps 余量，解码不是瓶颈；
    实际帧率主要取决于被控端发送的帧率（静态桌面时官方被控端会跳帧，约 20fps，
    画面有运动时才可能到 60fps）。
  - 想用硬件解码：安装 ffmpeg 并把它放到 PATH，然后：
        set UU_VIDEO_DECODER=ffmpeg
        set UU_FFMPEG_HWACCEL=d3d11va
        uu-gui.exe
    （UU_FFMPEG_HWACCEL 也可用 auto / qsv / cuda；硬件不可用会自动回退软件）
