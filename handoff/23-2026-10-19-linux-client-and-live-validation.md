# 交接23：显示器实机闭环、Linux客户端与自动测试

2026-10-19。最新授权：用户让 x358h 休眠，随后告知 laptop-arch 在线，要求把 Linux 控制端做到能运行、自建自动测试环境，工作到22:30，期间无需用户操作。此前“用户自行实验”的限制已被本次明确授权覆盖。不要唤醒 x358h。

## 入口与机器

- 工作区 `/home/yfy/Repo/uu_remote`；每项功能用中文提交，正文包含修改原因、实现方式和验证。
- laptop-arch：`yfy@192.168.35.100:22`，sudo免密；Arch、KDE Wayland、Tiger Lake Intel UHD、iHD26.3.5、`/dev/dri/renderD128`。用户会话 `XDG_RUNTIME_DIR=/run/user/1000`、`WAYLAND_DISPLAY=wayland-0`、Xwayland `DISPLAY=:1`，本轮XAUTHORITY `/run/user/1000/xauth_kZNTGA`（重登会变化）。
- **Linux正式入口**：`~/.local/bin/uu-remote-rust` / 应用菜单 **UU Remote Rust**。CLI `~/.local/bin/uu-rust`。统一目录 `~/.local/opt/uu-remote-rust/current`，实际保留 `releases/<revision>`。安装包无凭据；已有登录已以600权限保存到该用户 `~/.local/share/uu-remote/state.json`，未打印/提交内容。
- 本轮正式包源码 **`f4b1258246c24e4c8401f413271e91d74ef226eb`**，本地包在 `artifacts/packages/uu-remote-linux-x86_64-f4b1258246c2.tar.gz`，远端同名文件在 `~/.local/state/uu-remote-rust-tests/`。后续测试工具/文档提交不改变这个版本的产品代码。KDE实际启动主页PID3123，标题版本 `f4b1258246c2`，1100×760，位于eDP-1；PID仅为本轮记录，重启后应重查。
- KVM-WIN11：`yfy@192.168.35.6:2202`，官方UU4.41.1.2335；设备ID `aeawvmepembcpca5`。原始布局：DISPLAY1 AMD vDisplay1920×1080@60、位置1920,0、非主；DISPLAY2 Zako HDR1920×1080@60、位置0,0、主；UU虚拟DISPLAY3..12关闭。
- x358h：`yfy@192.168.34.3:2202`，用户已要求休眠，未唤醒。Windows正式路径仍是 **D:\SMB\uu-gui-demo**；安装版仍38b97df，本轮源码修复尚未向睡眠机器同步。不要声称Windows正式包已经更新。

## 多显示器与连接根因（交接22后续）

1. 普通 ConnectOptions 字段10 `NORMAL=1` 会使官方主机在CONTROL建立前回放该控制器保存的超级屏/独占布局。这解释了为何客户端尚未发切屏请求，物理显示器就被关掉。默认Preserve改为 `ASSISTANCE=2`，保留相同桌面捕获和配对认证；只有显式独占虚拟屏时才NORMAL。不能只删初始化CaptureParams就当布局已解决。
2. CaptureChange(8)、CaptureConfigResponse(19)、CaptureSettingResponse(22)实际可由 **TEXT_DATA_CHANNEL** 返回，旧代码只在CONTROL解析。两个通道共用二进制/文本wire解析，切源及设置回包均能收到。
3. CaptureSettingResponse逐项errors是Error消息列表，需读error_code；有描述但code0仍成功。旧代码把非零描述字节误判为失败。
4. 快速点击切屏串行执行，保留最后目标；失败后的下一次选择显式激活，不能误信旧轨道绑定。诊断加入 `display_ready` 的实际等待耗时。
5. Socket.IO多附件接收状态以前放在局部变量，select定时器取消后丢失半包。现在状态归属连接，支持附件间Ping。旧版两项确定性测试均失败、补丁后通过。

实机证据：x358h休眠前，从其试验客户端连接KVM-WIN11验证保留两个原生60Hz屏幕、CONTROL及解码；之后开发机headless与GUI各多轮切AMD/Zako、快速连点。开发机GUI一轮10次ready为142/354/101/368/99/100/283/99/225/103ms，无timeout。KVM低级钩子吞掉专用F23，实收down1/up1，证明Preserve策略未把输入变只读。用户工作电脑→x358h路径仍未复验，不能推断所有机器均已修复。

## Linux已落地与实测

- **VAAPI实修**：旧后端只初始化过。实机发现cros-codecs用ChromeOS专用GBM标志，Mesa NV12分配失败；换VA分配后，上游通用DMA去平铺又越界。现在用驱动分配/导出的DMABUF池，通过VAImage映射读图；严格检查NV12平面、pitch和尾长，不补零伪造坏帧。laptop离线120 AU输出118帧、无错误、最终保持VAAPI。内部提交/尾帧延迟仍有优化空间，尚未零拷贝。
- **音频**：CPAL以前只在Windows编译，Linux根本没有系统输出。现在Linux/Windows共用有界PCM队列和播放设备恢复，Windows保留专用MMCSS。本轮也修复短暂断粮后过早反相续播及锁争用后的跳变；6项音频回归通过。laptop独立PipeWire空声卡实录440Hz，150包、RMS约0.108；不改默认声卡。
- **中文字体**：Fontconfig选具体文件和TTC face，补Arch文泉驿路径；laptop中文正常。
- **键盘框架补丁**：egui-winit将Ctrl+C/V/X按下转成剪贴板事件，丢失原始按键；Ctrl+Alt+X也被误判剪切。vendor/egui-winit仅增加可选preserve-key-events、Alt匹配修正及Linux Super修饰保留；仅Linux启用该特性，保留上游许可证。普通GUI剪贴板事件仍可用，串流收到完整按下/抬起。
- **全屏捕获**：参照Moonlight `input.cpp::updateKeyboardGrabState` 的捕获/全屏条件，用现有Wayland连接的surface+seat申请shortcuts-inhibit；失焦、退出全屏和析构释放，不创建全局键盘钩子。Wayland普通键盘继续走egui，不能冒充Windows原生hook。窗口模式Super留本地。
- **鼠标与焦点**：Linux相对模式使用Locked而非Confined；初始focused=false，防止窗口尚未拿到焦点时被误判失焦。远端实测相对增量后光标由(120,62)到(209,109)，主机加速度可使实际位移不等于原始60/30。
- **剪贴板**：Linux arboard启用Wayland data-control，支持X11回退；后台线程、30KiB文本上限、只读、失焦停用及防回环。独立Wayland会话验证中英标点、双向和各门控全部通过，不读取用户KDE剪贴板。
- **窗口**：Wayland没有全局坐标，旧Geometry直接None，使80%尺寸逻辑整段不运行。现在用局部视口尺寸继续计算；位置由合成器管理。新增回归通过。
- **转换CPU优化**：NV12/I420到RGBA加SSE4.1，标量回退，保持16.16 BT601逐字节一致。开发机固定CPU离线1080p约20ms→1.35ms，4K约79ms→5.6ms；详见conversion-performance.md，不能当Windows真实FPS。

## 自动测试与证据

运行命令及依赖集中在 [Linux客户端说明](../docs/linux-client.md)。

- `tools/linux/wayland_smoke.py`：私有Sway、自己的虚拟键鼠、凭据副本和截图；全程不向真实KDE注入按键。`--relative-mouse`测试Locked；`--soak-seconds`延长运行；`--keyboard-fixture`只在远端专用文本框启动后使用。
- `uu-gui --example wayland_input`：标准US物理键位、持续keyboard/pointer。wtype动态符号键码不等于真实物理键；临时设备退出还会让无头seat失去最后一台键盘而人为失焦。Sway IPC cursor也不能替代真的pointer，这些假象已经从套件移除。
- `tools/windows/keyboard-fixture.ps1`：只读自己文本框的固定测试内容；F22激活，禁用该控件IME，Ctrl+C/V/X拦截计数不改系统剪贴板，定时关闭、恢复前台。默认45秒，长稳可设Seconds；远端任务必须交互会话。
- `tools/linux/audio_smoke.py` + `uu-media --example audio_probe`：实际Opus→CPAL→专用空声卡，录音确认RMS/440Hz，结束卸载自己模块。
- 本地 `artifacts/linux-validation/wayland-10.log`：整套exit0/graceful_close=true，切屏47/48/44/144ms，VAAPI1080p，无timeout；`input-unicode-10.json`远端逐字等于`aA;:,.?/[]{}+-_`，copy/paste/cut均true。之前全角标点来自远端IME，不是输入丢失，测试控件禁用IME后精确匹配。
- `wayland-relative-11.log`：正式已安装包，完整套件通过，切屏48/144/212/99ms；`input-relative-proof-11.json`证明实际光标移动。该轮关闭后文本为空已定位为测试表单自动Dispose后重新读取Text的问题，修复验收脚本保留最后有效快照。随后wayland-soak-12正式包相对模式连续60秒，文本及before_copy/paste/cut均逐字匹配，光标(209,109)→(298,155)，四次切屏48/35/43/48ms，退出0且无timeout。
- `soak-final-12.png`：60秒运动画面末帧HUD约57 Rx / 56.7 De / 59.2 Rd FPS，Decode4.60ms、Render5.18ms、RTT2.7±0.8ms、丢包0%、坏帧0、解码错误0；这是单次短测快照，不能当长期性能保证。
- 工作区完整150项测试通过；之后新增Wayland无坐标窗口回归，GUI32项再通过，总计151项对应覆盖。最终Windows交叉检查通过（有原有unused_mut字体候选警告）。没有为了凑数反复重跑整个套件。
- Linux安装器实际SHA256/ldd校验通过，KDE窗口标题版本、尺寸、输出和存活已经KWin核对，stderr为空。
- 开发机后台证据在 `artifacts/live-display-validation/`，含布局watch、GUI截图、信令元数据、钩子输入证明、前后基准和Windows编译日志。包含私有材料的artifacts忽略，不入git。

## 最终清理与安装复核

- 真实交互桌面EnumDisplayDevices/Settings最终复核：仍只有AMD和Zako两个显示器激活，均1920×1080@60，Zako主屏，全部UU虚拟屏关闭；未为了清理改变布局。证据 `artifacts/linux-validation/final-active-displays.json`。
- 安装包篡改测试：私有副本追加坏数据后安装器拒绝，正式current不变，也未创建目标前缀。
- PipeWire测试空声卡剩余0，私有Wayland运行目录均已清理；KVM本轮6个uu_live测试任务已注销，官方UU服务未停止。Linux KDE主页保留运行。
- 使用正式CLI和默认用户配置再次查询设备列表，code=0、5台绑定设备且包括KVM-WIN11，说明主页的正式登录配置可用。

## 后续优先级与边界

1. x358h醒来后同步正式Windows D盘包，并重验用户工作电脑→x358h；本轮不能代替该路径。
2. Linux真实KDE长时间游戏、相对鼠标手感、多DPI跨屏、其它GPU和X11系统键抓取；Wayland compositor可保留自己的紧急退出组合键。
3. VAAPI stateless当前有内部尾帧/提交等待，输出仍经CPU NV12/RGBA和上传；可参考Moonlight VAAPI/EGL实现推进，但不宣称已有零拷贝或持续120FPS。
4. FEC仍受官方发送端协商和实际修复包格式约束，不能单边把Sunshine RS/PyroWave接到UU；现有NACK/RTX和Opus concealment不冒充新增视频FEC。
5. 文件传输/图片剪贴板、真正Linux被控端采集与uinput仍非本轮完成项。不要因controller probe输出host uinput错误而误报Linux控制端不可用。
