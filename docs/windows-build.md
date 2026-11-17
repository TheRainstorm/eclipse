# Windows 编译与打包

主程序可直接检出构建，无需下载 UU 插件源码。完整包同时构建并附带匹配 SDK 的壁纸合集，默认关闭；UU 插件仍独立分发，见[插件说明](plugins.md)。

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

Windows 本机编译需准备 Rust 的 `x86_64-pc-windows-gnu` 工具链、64 位 MinGW（win32 线程模型）、CMake 3.27+、NASM、Git 和 Python 3.11+，并在当前终端的 PATH 中提供这些工具。脚本会使用本机的 `gcc`、`g++`、`objdump` 和 `windres`；无需 Visual Studio。MSVC 工具链暂不适用于此打包脚本。

```bash
python3 tools/windows/package.py
```

脚本执行 `cargo build --release -p uu-gui -p uu-cli --target x86_64-pc-windows-gnu --locked`，
从固定提交构建 MIT 许可的 Intel VPL dispatcher 和 PyroWave Vulkan 运行库，检查 EXE / DLL 的 MinGW 动态依赖并收集运行库，再生成：

```text
artifacts/packages/eclipse-windows-x64-<提交号前12位>.zip
artifacts/packages/windows-latest.json
```

ZIP 解压后直接是 `eclipse.exe`、`eclipse-cli.exe`、两个 MinGW 运行 DLL、`libvpl.dll`、`libpyrowave-shared-1.dll`、`uu_plugin_sdk.dll`、`std-<编译器哈希>.dll`、`README.txt`、`install.ps1`、`LICENSE`、`LICENSE-MIT`、`LICENSE-APACHE`、`THIRD-PARTY-NOTICES.txt`、`version.json`。
壁纸合集位于 `plugins/wallpaper-collects/`，其清单及动态库一起纳入完整包校验。用户无需额外下载安装，在设置中启用即可。
`version.json` 包含包格式版本、完整源提交号、dispatcher 源提交、UTC 打包时间和各文件 SHA256。新版为格式 5，包含共享 SDK 身份和运行库清单；安装器与同步脚本仍接受格式 1–4 的旧包。
`windows-latest.json` 指向最近一次成功完成的 Windows 包，并记录 ZIP 的 SHA256。
源码或 `tools/windows` 有未提交改动时拒绝打包，避免版本号与实际源码不一致；提交后再运行即可。

重复执行会先核验已有同提交 ZIP 的完整文件清单、哈希和目标平台，符合时直接复用并更新最新包指针，不调用 Cargo 或下载运行库源码。`--rebuild` 可强制执行构建流程，仍保留 Cargo 缓存；显式提供 `--vpl-archive` 时也执行构建流程。`cargo check` 和 `cargo test` 分别产生检查与测试产物，不等于已经生成可安装的 release 包。
构建沿用 Cargo 配置的目标目录（支持 `CARGO_TARGET_DIR`），输出目录可用 `--output-dir /path/to/packages` 指定。
新包统一输出到忽略 Git 的 `artifacts/packages`；仓库旧 `dist/uu-gui-win64` 不再更新。

Intel VPL 源码固定为 `674d015bcb294bc39fa276e99a652ea045423e82`，下载归档的 SHA256 为 `9d5d3c221b9d41468e04add6855cf5eec6df06dae7be649f84e1f846c98e38fb`。首次构建需要下载约 13 MB；以后在 Cargo 目标目录的 `vpl-runtime` 中复用并重新校验源码缓存。离线环境可提前下载该归档，再传入 `--vpl-archive /path/to/source.tar.gz`，同样校验完整哈希。构建不安装驱动。实际 HEVC / AV1 能力仍取决于 Intel GPU 和系统 GPU runtime；直接使用 `cargo build` 时也需让标准 `libvpl.dll` 位于程序目录或系统受信任的 DLL 搜索目录。

## 解压运行

将 ZIP 解压到可写目录，双击 `eclipse.exe` 即可运行。默认在程序旁自动创建 `eclipse-data`，无需先执行 PowerShell 脚本。更换数据目录可在“应用设置 → 常规 → 配置存储”中迁移并重启。[数据目录与迁移](configuration-storage.md)

作为被控端时，在“服务管理”启用本机服务，按系统提示授权。用户配置默认便携不代表 Windows 服务和驱动本身可以随目录复制到其它电脑。

## 可选安装脚本

需要安装到指定位置并创建系统快捷方式时，在解压目录打开 PowerShell：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\install.ps1 -InstallDir "$env:LOCALAPPDATA\Programs\Eclipse"
```

`-InstallDir` 指定安装位置；`-SourceDir`、`-InstallDir` 和 `-ArchiveRoot` 的相对路径按当前 PowerShell 目录解析。安装目录不能为盘根。安装后关闭旧主页和串流窗口，再从桌面 **Eclipse** 打开新版。
安装器先校验全部文件，再归档旧版和更新快捷方式；升级不覆盖登录态和设置，也不结束现有进程。已加载的 EXE / DLL 默认先移入安装目录内的 `.upgrade-archive`，确保安装路径经过目录联接或位于其它盘时仍在同一卷重命名，新进程使用新版。`-ArchiveRoot` 可自行指定归档位置；保留运行中程序时应选同一卷。发生共享冲突或复制失败时，自动恢复此前修改的包文件并报告错误；保留备份位置供检查。

## 脚本回归检查

```bash
python3 -m unittest discover -s tools/windows -p 'test_*.py' -v
```

Windows 还可运行 `powershell.exe -NoProfile -ExecutionPolicy Bypass -File tools/windows/test_installer.ps1`。它只在独立临时目录生成测试包，验证共享冲突回滚、配置保留与旧包兼容，不修改已有安装。

PyroWave 运行库同样要求 CMake 3.27+（包括 Linux 上的 Windows 交叉构建），通过 Git 获取固定的上游和最小 Granite 依赖，使用预生成 shader。无需安装 Vulkan SDK 或 shader 编译器；GPU 功能由运行时驱动提供。完整包包含 `libpyrowave-shared-1.dll` 和许可；构建来源、限制与生成图复测见 [PyroWave](pyrowave.md)。

开发分支的 GUI 文件名为 `eclipse.exe`，CLI 为 `eclipse-cli.exe`。打包会在分发副本上删除不需要的符号，保留 DLL 导出与运行时依赖；Cargo 构建目录保留完整符号用于排障。旧版本的文件名和包格式仍按其原清单校验。
