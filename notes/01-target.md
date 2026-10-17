# 目标机档案：fujistu-arch（2026-10-15 实测）

| 项 | 值 |
|---|---|
| 主机别名 | `fujistu-arch` → `fujistu-arch.op1:22`，用户 `yfy` |
| 系统 | Arch Linux，kernel 6.17.8，**已 185 天未重启** |
| 架构 | **x86_64**（Intel Celeron 6305 @1.8GHz，2 核/2 线程） |
| 内存 | 3.5 GB + zram 3.4 GB swap |
| 磁盘 | 单盘 13.4 GB NVMe，`/` 已用 9.4G / 可用 ~3.0G |
| 网络存储 | `//nas-smb/Disk1`（28T 可用）、`//nas-smb/Archive` —— 需要时可把大目录放这里 |
| GPU | Intel Tiger Lake-LP UHD Graphics G4（`/dev/dri/card1`, `renderD128`） |
| 编解码工具 | `vainfo`、`intel_gpu_top`、`glxinfo` 已装 |
| 桌面 | KDE Plasma 已装（wayland `plasma`/`sway`，x11 `plasmax11`）；`sddm` 正在跑 |
| 当前会话 | **无用户图形会话**：只有 sddm greeter（tty2）+ 若干 SSH 会话 → 实际是**无头**使用 |
| sudo | **免密码** |
| 已有 | `~/.vnc`（VNC *viewer*，说明习惯用 VNC 看远端桌面） |

## 对方案的直接影响

1. **不是 ARM64** —— 无需 box64/FEX；直接走 x86_64 Wine 路线，与既有先例同构。
2. **无头** —— 必须自带显示层（Xvfb / Xorg dummy / 或让用户登录 Plasma）。
   这是与既有先例（Ubuntu 桌面会话）最大的差异，也是本项目的主要新增工作。
3. **磁盘紧张（~3 GB）** —— Wine(600M)+mono+客户端+WebView2 约 1.3–1.6 GB，
   勉强可行；必要时把 Wine prefix 放到 NAS。
4. **无 NVIDIA** —— 走 CPU/OpenH264 编码（或 Intel VA-API），既有先例的 NVENC
   二进制补丁路径在本机不适用，反而是**简化**。

## 包管理现状

- sync db 可用；`wine 11.19-1`、`wine-mono`、`wine-gecko`、`xorg-server-xvfb`、`winetricks`
  的依赖**全部已满足**，安装它们不会牵动系统其余 287 个待升级包 → 风险很低。
