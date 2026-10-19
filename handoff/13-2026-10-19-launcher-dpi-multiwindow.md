# 13 · 主页跨屏修复与多设备独立串流窗口

用户正在场，要求自行进行实机体验测试。本轮仅进行必要的 GUI 针对性检查、Windows 构建及安装校验，不自动启动 GUI、不连接任何测试设备、不关闭用户窗口。

## 主页跨 DPI

用户指出交接12的修复仍未覆盖未串流主页，跨 DPI 或系统跨屏快捷键移动后会放大、超出目标屏幕。旧逻辑仅在串流时处理 DPI，位置也仅在尺寸超限时修正；使用 egui 上一帧坐标乘当前 DPI 反推显示器会混用坐标。

`16e6096` 改为从 eframe Frame 的 HWND 同步读取 GetDpiForWindow、GetWindowRect、GetClientRect、ClientToScreen、MonitorFromWindow 和显示器工作区。主页保留逻辑尺寸，换 DPI 或工作区时同时约束大小和位置，包含负坐标副屏；串流仍按物理视频尺寸处理。新增回归覆盖新原生 DPI 与旧 egui 坐标不一致、负坐标工作区、大小可容纳但位置越界。

## 保留主页与多设备

`d2a5aac` 将连接改为独立串流窗口：

- 普通启动始终保留设备主窗口，点击其他设备可继续创建连接。
- 每个串流使用同一安装路径 exe 的 `--stream-device <id> --stream-alias <name>` 参数启动独立进程；观看模式额外传 `--view-only`。
- 各进程分别拥有控制器、解码器、纹理、HUD、快捷键、键鼠焦点、窗口及退出清理，避免多设备互相覆盖状态。
- 子进程不重新加载设备列表，也移除继承的 UU_AUTOCONNECT，直接连接指定设备，防止递归启动。
- 同一主页再次连接已打开设备时激活其窗口，避免重复申请同一设备连接。进程结束后可重新连接。
- 关闭串流窗口或“退出远控”只释放当前设备；关闭设备主窗口不会强制结束已有串流。主页重开后不会重新接管旧进程的列表。
- 异常断开时串流窗口保留错误提示，用户可关闭后从主页重开。
- Windows 窗口标题包含设备名称，便于多个串流之间切换。

## 验证与交付

- `cargo test -p uu-gui --locked`：11项通过，包含 DPI 回归、快捷键、参数/观看模式、子进程不递归启动，以及回收已结束设备时不终止其他会话。
- `python3 tools/windows/package.py`：Windows GUI/CLI release 构建与打包通过。
- 多设备真实连接、主页拖动和系统跨屏快捷键的体验由用户验证；不要把交接12的串流实机结果当作本轮主页和多设备验收。
- 源版本 `d2a5aac`。x358h 唯一路径仍为 `C:\Users\yfy\uu-gui-demo\uu-gui.exe`，桌面 `UU Remote Rust`。安装后应由用户关闭旧版并重新打开桌面入口。
- GUI SHA-256：`43f035f9f48e4098bf4b796db77706b476d79de77f5b2aa37ed5f712adfc7a67`；manifest 记录完整源提交和包哈希。
- 构建日志：`artifacts/ui-dpi-validation/multiwindow-package.log`。没有新增实机串流测试或截图。
