# 里程碑记录

## M1 — 官方 Windows 客户端已在 fujistu-arch (Arch Linux) 上跑起来 ✅ 2026-10-15

**已验证事实（不是推测）：**

1. `wine 11.19-1` 安装成功（依赖全部满足，**未做整机升级**，未触碰其余 287 个待升级包）。
2. Wine prefix：`~/.local/share/uu-remote/wine`（`WINEDLLOVERRIDES="mscoree,mshtml="`
   跳过 mono/gecko，省 ~350 MB；实测该客户端不需要 .NET）。
3. `wine UURemote_Setup_4.42.0.2770.exe /S` **静默安装成功，退出码 0**。
4. 安装位置：`drive_c/Program Files/Netease/GameViewer/`，prefix 共 666 MB。
5. `GameViewerService.exe --service` 实际**作为服务运行**（Sentry/crashpad 上报目录已生成）。
6. `wine GameViewer.exe` **GUI 成功渲染**（见 `artifacts/xvfb_shot1.png`：启动画面“网易UU远程”）。
7. 客户端以 **controller（主控端）** 身份启动，生成了
   `log/client/log/{connection_log,streamer_log}_controller_*.slog` → 网络与串流模块都跑到了。

**未解决（下一步）：**
- 客户端窗口在 splash 之后退出（疑似缺 WebView2 运行时，登录/主界面是 WebView2 承载的 HTML）。
- Xvfb 下无 DRI3 → `llvmpipe` 软件渲染（`wined3d_guess_card No card selector`）。
  视频解码要走软件 → 对 2 核 Celeron 是硬伤，需接真实 Xorg + Intel VA-API。

**复现命令：**

```bash
# 目标机 fujistu-arch
sudo pacman -S --needed wine wine-mono wine-gecko xorg-server-xvfb winetricks
export DISPLAY=:99 WINEPREFIX=$HOME/.local/share/uu-remote/wine WINEDLLOVERRIDES="mscoree,mshtml="
Xvfb :99 -screen 0 1920x1080x24 &
wineboot -u
wine /tmp/UURemote_Setup_4.42.0.2770.exe /S
cd "$WINEPREFIX/drive_c/Program Files/Netease/GameViewer" && wine GameViewer.exe
```

## 客户端组件清单（安装后实测）

| 组件 | 作用 | Linux 侧怎么办 |
|---|---|---|
| `GameViewer.exe` | 主界面（Qt + WebView2） | 直接跑 Wine |
| `bin/GameViewerServer.exe` | 被控端核心 | 直接跑 Wine |
| `bin/GameViewerService.exe` | Windows 服务 | Wine 服务（已跑通） |
| `bin/nrd_renderer_ipc.dll` | 渲染 IPC | 观察 |
| `bin/drivers/GameViewerIddDriver/` | **虚拟显示器驱动 (IDD)** | ⚠️ Wine 无法加载内核驱动 → 需用 Xorg dummy / Xvfb 自建虚拟显示器 |
| `bin/drivers/uuycinput/` | 输入注入驱动 | ⚠️ 换成 XTest / uinput |
| `bin/drivers/ViGEmBus/` | 虚拟手柄 | 需要时换 `uhid`/uinput |
| `bin/drivers/nevaudio/` | 虚拟声卡 | 换 PipeWire/PulseAudio |
| `bin/html/skeleton/` | WebView2 UI 资源 | 需要 WebView2 运行时 |

> 这张表就是本项目“逆向 + 移植”的技术核心：**内核驱动类组件（IDD/输入/虚拟手柄）
> 在 Wine 下必然不可用，必须由 Linux 原生层替代**——这与既有先例的做法一致。
