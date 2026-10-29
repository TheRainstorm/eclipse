uu-client — Linux

安装：python3 install.py
启动：~/.local/bin/uu-client，或应用菜单 uu-client。
CLI：~/.local/bin/uu-rust

首次使用：默认 Eclipse 局域网，无需第三方账号。目标电脑启动服务后默认接收配对申请，将控制端八位码输入主机确认，不自动批准。
已有官方被控端选择“兼容设备”后登录；原账号与设置保留。
原生连接提供 IP 直连、UU 寻址与自建连接服务，统一使用设备授权和共享设置。

共享支持 X11 与 Wayland。Wayland 通过系统门户选择画面及许可键鼠，需要 PipeWire、xdg-desktop-portal 和对应桌面后端；Sway等wlroots桌面在屏幕授权后可使用同合成器提供的虚拟键鼠，按所选输出与缩放定位；明确拒绝输入授权时保持只观看。不支持绕过系统授权的无人值守采集。
视频支持 H.264 / HEVC / AV1，按实际驱动和控制端能力协商。H.264 CBR 优先 VA-API，不可用时自动模式可回退 OpenH264；HEVC / AV1 / VBR 需要 Intel VPL dispatcher（libvpl.so.2）及 GPU runtime，不会静默换成 H.264。客户端可请求5–240帧并动态调整帧率和码率，4K60等组合的实际吞吐受格式预算、GPU和驱动限制。
兼容 UU 被控端可在应用设置选择优先 HEVC，实际硬解探针成功后才声明支持。
串流菜单提供画面、编码、声音和输入设置；新连接默认加入已有工作区，可移到独立窗口。
最多四个原生连接分别观看，一次仅一个控制端持有键鼠和手柄控制权。切屏、失焦、断线会释放本会话输入。
原生图文剪贴板默认关闭，需双方许可及前台焦点，图片PNG压缩后上限8MiB；旧端沿用文本协议。
新版原生两端可协商有预算上限的XOR视频FEC，每组最多恢复一个丢失包；不兼容UU官方FEC。
原生手柄和陀螺仪使用 SDL 读取，主机通过虚拟 DS4 注入；Linux 主机需可访问 /dev/uhid，应用不会自动改设备权限或安装驱动。

运行需要 libva、libgbm、libpulse、libpipewire-0.3、libxkbcommon、libxkbcommon-x11、ALSA（桌面推荐 PipeWire ALSA 插件）、OpenGL/EGL。
登录/设置：$XDG_DATA_HOME/uu-remote 或 ~/.local/share/uu-remote。
支持 Wayland/X11；Wayland 系统快捷键捕获取决于合成器支持 shortcuts-inhibit 协议。
默认保留被控端原有显示布局，独占虚拟屏仅在显式选择时启用。

快捷键：Ctrl+Alt+Shift+S 信息；F/X 全屏；Q 退出串流；M 静音；H 悬浮入口；Z 捕获。
升级使用版本目录和原子链接切换，保留用户配置与现有进程；重开程序后使用新版。
