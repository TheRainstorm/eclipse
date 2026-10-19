# Windows 编译、打包与同步

在 Linux 仓库根目录执行。脚本使用 Python 3.11+ 标准库，无需安装 pip 依赖。
Windows x64 包包括 GUI、CLI、MinGW 运行库、安装器、使用说明和版本校验清单。

## 一次性准备 Linux 编译环境

已有 Rust 稳定版（cargo、rustup）的 Ubuntu / Debian：

```bash
sudo apt-get install build-essential pkg-config cmake nasm mingw-w64 libssl-dev python3 openssh-client
rustup target add x86_64-pc-windows-gnu
```

当前打包脚本使用 `x86_64-w64-mingw32-gcc` / `g++` 的 **win32 线程模型**，从该工具链自动查找运行 DLL。
若检查提示缺少 `libwinpthread-1.dll`，通过下面的工具选择对应的 `*-win32` 编译器后重新构建：

```bash
sudo update-alternatives --config x86_64-w64-mingw32-gcc
sudo update-alternatives --config x86_64-w64-mingw32-g++
```

## 编译与打包

```bash
python3 tools/windows/package.py
```

脚本执行 `cargo build --release -p uu-gui -p uu-cli --target x86_64-pc-windows-gnu --locked`，
检查 EXE / DLL 的 MinGW 动态依赖，自动收集运行库，再生成：

```text
artifacts/packages/uu-remote-windows-x64-<提交号前12位>.zip
artifacts/packages/windows-latest.json
```

ZIP 解压后直接是 `uu-gui.exe`、`uu.exe`、两个运行 DLL、`README.txt`、`install.ps1`、`version.json`。
`version.json` 包含完整源提交号、UTC 打包时间和各文件 SHA256。
`windows-latest.json` 指向最近一次成功完成的 Windows 包，并记录 ZIP 的 SHA256。
源码或 `tools/windows` 有未提交改动时拒绝打包，避免版本号与实际源码不一致；提交后再运行即可。
构建沿用 Cargo 配置的目标目录（支持 `CARGO_TARGET_DIR`），输出目录可用 `--output-dir /path/to/packages` 指定。
新包统一输出到忽略 Git 的 `artifacts/packages`；仓库旧 `dist/uu-gui-win64` 不再更新。

## 自动同步到 Windows（推荐）

Windows 需要已启用 OpenSSH Server，SSH 用户需要有目标目录的写权限。
你的 x358h 使用 `192.168.34.3:2202`，登录用户为 `yfy`；机器开机后执行：

```bash
# 可先确认 SSH 能登录；首次连接核对主机指纹
ssh -p 2202 yfy@192.168.34.3
# 在远端输入 exit 返回 Linux，再同步最新打包产物
python3 tools/windows/sync.py --host 192.168.34.3
```

默认从 `windows-latest.json` 找包，通过 `scp` 上传，校验 ZIP 后在远端临时目录解压，
执行安装器，将正式目录统一为 **`D:\SMB\uu-gui-demo`**，更新桌面 **UU Remote Rust** 快捷方式，
校验安装后的文件，并清理上传的 ZIP 和临时解压目录。每次同步使用独立临时文件名。
SSH 认证沿用 ssh-agent / `~/.ssh/config`；没有密钥时可能需要分别为上传和安装输入密码。

安装前建议退出旧客户端，避免运行 DLL 被占用。安装器不会结束正在运行的程序，
旧文件归档到 `%USERPROFILE%\uu-gui-archive`。同步完成后从桌面入口重新打开，
主页底部版本号应与脚本输出的提交号一致。

常用选项：

```bash
# 仅校验本地产物并显示操作，不连接远端
python3 tools/windows/sync.py --host 192.168.34.3 --dry-run

# 另一台机器 / 用户 / SSH 端口 / 私钥
python3 tools/windows/sync.py --host 192.168.1.20 --user alice --port 22 --identity ~/.ssh/id_ed25519

# 指定历史包或自定义输出目录中的包
python3 tools/windows/sync.py --host 192.168.34.3 --package /path/to/uu-remote-windows-x64-xxxx.zip

# 自定义安装位置（PowerShell 在远端运行；此处是 Linux shell 的单引号）
python3 tools/windows/sync.py --host 192.168.34.3 --install-dir 'D:\SMB\uu-gui-demo'
```

`--no-shortcut` 可以保留当前快捷方式，用于在独立目录试装。
Windows 上首次配置 OpenSSH 时，确认其默认 shell 是标准 cmd 或 PowerShell，未额外改变 SCP 会话工作目录。

## 不使用 SSH：手动复制安装

通过 SMB / U 盘将 ZIP 复制到 Windows 的临时目录，右键「全部解压」，
在解压后的目录打开 PowerShell：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\install.ps1
```

安装位置同样是 `D:\SMB\uu-gui-demo`。不要直接解压覆盖正在使用的正式目录；
通过安装器安装才能校验文件、归档旧版和统一快捷方式。未启用 SSH 时，这条路径也可以使用。

## 脚本回归检查

```bash
python3 -m unittest discover -s tools/windows -p 'test_*.py' -v
```
