# 目标机改正：laptop-arch（主控端）

> 更正：目标机是 `laptop-arch`（`laptop-arch.op1` = 192.168.35.100:22，用户 yfy）。
> 之前误在 `fujistu-arch` 上做的工作已全部清理（prefix/进程/临时文件已删除，磁盘回到 3.8G 可用；
> 仅保留 `wine` 等软件包，未做整机升级）。

## laptop-arch 画像（实测）

| 项 | 值 |
|---|---|
| 系统 | Arch Linux，kernel 7.2.6，`pacman -Qu` = 0（完全同步） |
| CPU/内存 | Intel Celeron 6305（2 核）/ 3.5 GB + 2G swapfile + 3.5G zram |
| 磁盘 | 39 GB，可用 ~22 GB（充裕） |
| GPU | Intel Tiger Lake-LP UHD G4（**仅核显**，无 NVIDIA） |
| 桌面 | **KDE Plasma Wayland 正在运行**（kwin_wayland + plasmashell，`XDG_SESSION_TYPE=wayland`） |
| Xwayland | `DISPLAY=:1`，`XAUTHORITY=/run/user/1000/xauth_aPlZdO` |
| OpenGL | **硬件直渲染**（`Mesa Intel(R) UHD Graphics (TGL GT2)`，OpenGL 4.6） |
| 其他 | `vainfo`/`glxinfo`/`ffmpeg` 已装；无 AUR helper；sudo 免密 |

对主控端的意义：**这正是既有先例所针对的“桌面会话”环境**，且核显可走 VA-API 解码。

## 已完成并验证

1. `wine 11.19` 装好；prefix `~/.local/share/uu-remote/wine`。
2. 官方客户端 4.42.1.2835 静默安装成功（`UU_INSTALL=0`）。
3. **WebView2 运行时安装成功**：`bin/MicrosoftEdgeWebview2Setup.exe /silent /install`
   → `drive_c/Program Files (x86)/Microsoft/EdgeWebView/Application/155.0.4283.45/`。
4. `GameViewerService` 注册为 Wine 服务并运行；`wine net start` 可见。
5. 在**真实 Plasma 会话里**启动客户端 → 出现 `GameViewer` 窗口（splash，352×245）。

## 已套用的兼容配方（源自已验证先例，逐条实测生效）

| 设置 | 值 | 位置/命令 | 作用 |
|---|---|---|---|
| 全局 Windows 版本 | `win10` | `HKCU\Software\Wine` / `Version` | 客户端主程序默认 |
| **服务端版本** | **`win7`** | `HKCU\Software\Wine\AppDefaults\GameViewerServer.exe` / `Version` | **关键**：否则 UU 会往 Wine 设备库装 IDD/gvInput/ViGEm 内核驱动，导致 server 在 IPC 起来前自杀 |
| WebView2 版本 | `win8` | `HKCU\Software\Wine\AppDefaults\msedgewebview2.exe` / `Version` | WebView2 兼容 |
| X11 焦点 | `UseTakeFocus=N` | `HKCU\Software\Wine\X11 Driver` | 避免抢焦点 |
| 主控窗口 | `UseXIM=N`、`UseTakeFocus=Y` | `HKCU\Software\Wine\AppDefaults\gameviewer.exe\X11 Driver` | 输入法/焦点协议 |
| 中文字体 | `MS Shell Dlg` → `Noto Sans CJK SC` | `HKLM\...\FontSubstitutes` | 托盘中文不豆腐块 |
| 事件日志 | **`wevtapi=n` + 原生 shim DLL** | `bin/wevtapi.dll`（2446 B，仅导出 `EvtQuery/EvtNext/EvtRender/EvtOpenPublisherMetadata/EvtClose`） | Wine 内建 wevtapi 不足；缺它客户端退出 |
| 其他 | `winemenubuilder.exe=d`；服务 `Start=2`；`unset WEBVIEW2_*` | | |

## 当前卡点

客户端结构：**根目录 `GameViewer.exe` 只是启动器**（创建 `bin/GameViewer.exe` 后正常退出），
真正的 Qt 主控 UI 是 `bin/GameViewer.exe`。

现象：套用上述配方后，controller 会话**确实启动**了
（新增 `log/client/log/{log,connection_log_controller,streamer_log_controller}_20261015221331*_588.slog`），
但 `bin/GameViewer.exe` 在 ~60–75 秒后退出，只留下 splash 窗口和 crashpad 进程，无 SEH 崩溃。

结论：**这不是崩溃，是主动自退**。UU 自身日志为压缩格式（`SLOG` 头，无法直接阅读）。
下一步方向：
- 抓 `bin/GameViewer.exe` 的退出路径（`WINEDEBUG=+relay` 定向或 hook `ExitProcess`）；
- 或采用先例已解决的完整集成（其 `wevtapi.dll` shim 的 `DllMain` 会预加载
  `uu-remote-input-hook.dll`；主控端另有 `UU_REMOTE_CONTROLLER_MODULE`），
  说明**主控 UI 依赖它们注入的运行时模块**。

## 追加实验结果（均未解决）

| 尝试 | 结果 |
|---|---|
| 原生 `wevtapi.dll` shim（2446 B，5 个 Evt* 导出） | 已装入 `bin/`，`cmp` 校验通过 |
| 全局 win10 + server=win7 + webview2=win8 | 已写入并读回校验 |
| X11 `UseTakeFocus=N`、主控窗口 `UseXIM=N`/`UseTakeFocus=Y` | 已写入 |
| `MS Shell Dlg` → `Noto Sans CJK SC`（本机有 45 个 CJK 字体） | 已写入 |
| `MicrosoftEdgeWebview2Setup.exe /silent /install` | ✅ 运行时 155.0.4283.45 装成 |
| **DXVK 3.1.1**（d3d11/dxgi/d3d10core → system32，`d3d11=n;dxgi=n`） | 已装入；仍退出 |
| Vulkan | `vulkan-intel` 1:26.2.3 已装，ANV ICD 存在 |

现象不变：`GameViewerService.exe --service` + `crashpad_handler` 存活，
`bin/GameViewer.exe`（Qt 主控 UI）在启动 controller 会话后 ~60–75 s 主动退出，
只留 352×245 splash 窗口。SEH 无异常 → **主动自退，非崩溃**。

> 兜底方案：本机（ares）已留 `/tmp/uu_l`（GuoWQ222 源码，含其可运行启动器脚本与 wevtapi
> 汇编源码）与 `/tmp/uur`（panxuc/uur，Rust + Arch 打包），可作为后续对照与参考。
