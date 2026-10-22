# Windows 编译与打包

在仓库根目录执行，支持 Linux 交叉编译及 Windows 本机 GNU 编译。脚本使用 Python 3.11+ 标准库，无需安装 pip 依赖。
Windows x64 包包括 GUI、CLI、MinGW 运行库、Intel VPL dispatcher、安装器、使用说明、双许可与第三方通知、版本校验清单。

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

Windows 本机编译需准备 Rust 的 `x86_64-pc-windows-gnu` 工具链、64 位 MinGW（win32 线程模型）、CMake、NASM、Git 和 Python 3.11+，并在当前终端的 PATH 中提供这些工具。脚本会使用本机的 `gcc`、`g++`、`objdump` 和 `windres`；无需 Visual Studio。MSVC 工具链暂不适用于此打包脚本。

```bash
python3 tools/windows/package.py
```

脚本执行 `cargo build --release -p uu-gui -p uu-cli --target x86_64-pc-windows-gnu --locked`，
从固定提交构建 MIT 许可的 Intel VPL dispatcher，检查 EXE / DLL 的 MinGW 动态依赖并收集运行库，再生成：

```text
artifacts/packages/uu-client-windows-x64-<提交号前12位>.zip
artifacts/packages/windows-latest.json
```

ZIP 解压后直接是 `uu-gui.exe`、`uu.exe`、两个 MinGW 运行 DLL、`libvpl.dll`、`README.txt`、`install.ps1`、`LICENSE`、`LICENSE-MIT`、`LICENSE-APACHE`、`THIRD-PARTY-NOTICES.txt`、`version.json`。
`version.json` 包含包格式版本、完整源提交号、dispatcher 源提交、UTC 打包时间和各文件 SHA256。新版为格式 2，安装器与同步脚本仍接受格式 1 的旧包。
`windows-latest.json` 指向最近一次成功完成的 Windows 包，并记录 ZIP 的 SHA256。
源码或 `tools/windows` 有未提交改动时拒绝打包，避免版本号与实际源码不一致；提交后再运行即可。
构建沿用 Cargo 配置的目标目录（支持 `CARGO_TARGET_DIR`），输出目录可用 `--output-dir /path/to/packages` 指定。
新包统一输出到忽略 Git 的 `artifacts/packages`；仓库旧 `dist/uu-gui-win64` 不再更新。

Intel VPL 源码固定为 `674d015bcb294bc39fa276e99a652ea045423e82`，下载归档的 SHA256 为 `9d5d3c221b9d41468e04add6855cf5eec6df06dae7be649f84e1f846c98e38fb`。首次构建需要下载约 13 MB；以后在 Cargo 目标目录的 `vpl-runtime` 中复用并重新校验源码缓存。离线环境可提前下载该归档，再传入 `--vpl-archive /path/to/source.tar.gz`，同样校验完整哈希。构建不安装驱动。实际 HEVC / AV1 能力仍取决于 Intel GPU 和系统 GPU runtime；直接使用 `cargo build` 时也需让标准 `libvpl.dll` 位于程序目录或系统受信任的 DLL 搜索目录。

## 安装

将 ZIP 复制到 Windows 的临时目录，右键「全部解压」，
在解压后的目录打开 PowerShell：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\install.ps1 -InstallDir "$env:LOCALAPPDATA\Programs\uu-client"
```

`-InstallDir` 指定安装位置。安装前退出旧客户端，完成后从桌面 **uu-client** 重新打开。
安装器先校验全部文件，再归档旧版和更新快捷方式；升级不覆盖登录态和设置，也不结束现有进程。已加载的 EXE / DLL 默认先移入安装目录内的 `.upgrade-archive`，确保安装路径经过目录联接或位于其它盘时仍在同一卷重命名，新进程使用新版。`-ArchiveRoot` 可自行指定归档位置；保留运行中程序时应选同一卷。发生共享冲突或复制失败时，自动恢复此前修改的包文件并报告错误；保留备份位置供检查。

## 脚本回归检查

```bash
python3 -m unittest discover -s tools/windows -p 'test_*.py' -v
```

Windows 还可运行 `powershell.exe -NoProfile -ExecutionPolicy Bypass -File tools/windows/test_installer.ps1`。它只在独立临时目录生成测试包，验证共享冲突回滚、配置保留与旧包兼容，不修改已有安装。
