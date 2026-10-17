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
  - 远端画面已修复连续解码（此前只出首帧、约 6 FPS 冻结）
  - 键鼠已接通：输入走 CONTROL_DATA_CHANNEL 数据通道的裸 JSON
    （先做 SimpleAction echo 握手）
