# Windows 编译与打包

在 Linux 仓库根目录执行。脚本使用 Python 3.11+ 标准库，无需安装 pip 依赖。
Windows x64 包包括 GUI、CLI、MinGW 运行库、安装器、使用说明和版本校验清单。

## 一次性准备 Linux 编译环境

已有 Rust 稳定版（cargo、rustup）的 Ubuntu / Debian：

```bash
sudo apt-get install build-essential pkg-config cmake nasm mingw-w64 libssl-dev python3
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
artifacts/packages/uu-client-windows-x64-<提交号前12位>.zip
artifacts/packages/windows-latest.json
```

ZIP 解压后直接是 `uu-gui.exe`、`uu.exe`、两个运行 DLL、`README.txt`、`install.ps1`、`version.json`。
`version.json` 包含完整源提交号、UTC 打包时间和各文件 SHA256。
`windows-latest.json` 指向最近一次成功完成的 Windows 包，并记录 ZIP 的 SHA256。
源码或 `tools/windows` 有未提交改动时拒绝打包，避免版本号与实际源码不一致；提交后再运行即可。
构建沿用 Cargo 配置的目标目录（支持 `CARGO_TARGET_DIR`），输出目录可用 `--output-dir /path/to/packages` 指定。
新包统一输出到忽略 Git 的 `artifacts/packages`；仓库旧 `dist/uu-gui-win64` 不再更新。

## 安装

将 ZIP 复制到 Windows 的临时目录，右键「全部解压」，
在解压后的目录打开 PowerShell：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\install.ps1 -InstallDir "$env:LOCALAPPDATA\Programs\uu-client"
```

`-InstallDir` 指定安装位置。安装前退出旧客户端，完成后从桌面 **uu-client** 重新打开。
安装器校验文件、归档旧版和更新快捷方式；升级不覆盖登录态和设置。

## 脚本回归检查

```bash
python3 -m unittest discover -s tools/windows -p 'test_*.py' -v
```
