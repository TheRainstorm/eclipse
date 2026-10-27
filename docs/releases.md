# 构建与版本发布

大功能里程碑使用 `v主版本.次版本.修订号` 标签，标签版本必须与根目录 `Cargo.toml` 的 workspace 版本一致。正式安装包在 [GitHub Releases](https://github.com/TheRainstorm/uu-client/releases) 提供。

当前版本 **[v0.18.9](https://github.com/TheRainstorm/uu-client/releases/tag/v0.18.9)** 修复混合 DPI 外屏 Render 高延迟与悬浮输入菜单，并汇总此前本地 0.18.8 的 AMD AMF 输入复用、诊断、色彩、连接与输入修复，见 [0.18.9 发布说明](releases/v0.18.9.md)和[外屏延迟报告](performance/mixed-dpi-render-latency.md)。已有标签和产物保持不变，正式产物以 GitHub 上实际可用的发布为准。已有源码目录时可直接获取正式产物，无需本机 Rust 编译：

```sh
python3 tools/release/download.py
```

需要 Python 3.11+ 与已安装、登录的 GitHub CLI。工具核对标签提交、压缩包及逐文件校验和，再更新 `artifacts/packages/` 的平台最新包指针；不会安装或启动程序。

## 自动构建

`Build and release` 工作流在主分支提交、PR 和 `v*` 标签上运行，也可从 Actions 页面手动触发构建。当前固定 Rust 1.95.0；Actions 依赖固定完整提交号。

- Linux x86_64：Ubuntu 24.04 构建，启用 VA-API，运行工作区、独立SRTP密码向量/重放回归、原生两进程视频与独立 Sway 混合 DPI 窗口检查，生成 tar.gz。
- Windows x64：Linux 上使用 MinGW win32 线程模型交叉编译，生成 ZIP；随后在 Windows runner 校验所有文件哈希并运行 TLS/SRTP 原生两进程视频软件解码测试。
- 两个平台都运行凭据分发边界检查；构建不读取本地私有配置或 CI 服务密钥，测试使用虚构数据。v0.5.0 起明确分发 PC 兼容共享签名值，其它凭据仍禁止进入安装包。发布包包含许可文件、第三方通知与源提交清单。
- 分支和 PR 只生成保留 14 天的 Actions artifacts；标签构建全部成功后创建 GitHub Release，附上两端压缩包与 `SHA256SUMS`。如果维护者已发布经过本机原生验证的同标签完整包，工作流会重新下载，核对标签提交、压缩包和逐文件校验和，保留已发布资产；校验失败或遇到草稿时停止。带预发布后缀的标签发布为 prerelease，不替换最新稳定版本。

Linux 媒体还有独立的快速任务，只运行媒体库测试并生成带源提交、文件校验和的硬解对照探针，产物为 `linux-media-validation-fast`。它使显卡实机回归可以提前开始，正式发布仍等待完整两端检查；该任务只读恢复构建缓存，不与主任务争用缓存写入。

GitHub Actions 的 GUI/GPU 环境不能替代显卡、声音与桌面交互实测。Linux 预编译包要求兼容 Ubuntu 24.04 的 glibc 和所列桌面运行库；其它发行版可按 Linux 文档从源码构建。

0.6起，“关于软件”优先显示编译进程序的 Git 提交号；源码有本地修改会额外标明。相邻 `version.json` 用于核对安装包，清单与程序不一致或清单损坏时会提示。无 Git 信息的源码归档构建单独标识，不借用外层目录的仓库版本。复制版本诊断不包含本机路径或账号信息。

0.7起会话诊断统一写入用户状态目录的 `uu-remote/diagnostics`，不再优先写程序旁边；Linux遵循`XDG_STATE_HOME`。新建Unix目录/文件权限为0700/0600，Windows继承用户目录权限。诊断不包含键鼠内容、剪贴板、认证、SDP或网络候选原文；已有旧日志保留，不随升级自动删除。

开发版对长会话保留起始段和七段近期轮转记录，每段最多256 KiB、单会话最多2 MiB；`.tail-N`为循环槽位，按事件`at_ms`还原时间顺序。视频恢复原因见[串流质量与恢复](streaming-quality.md#原生串流的突发与恢复)。

## 发布步骤

1. 完成功能并单独提交，更新 README / 已知限制和 workspace 版本、Cargo.lock，并添加 `docs/releases/v版本号.md` 发布说明。
2. 确认主分支 CI 通过，并完成适用于功能的实机验证。
3. 给已验证的提交创建对应版本标签并推送。CI 校验标签与源码版本，重新构建并自动发布。

```bash
git tag -a v0.2.0 -m 'uu-client 0.2.0：实验性原生局域网观看'
git push origin v0.2.0
```

标签与发布包应保持不变。发现问题应修复后发新版本；不要为了替换二进制移动已发布标签。`SHA256SUMS` 用于检查下载完整性，不代替作者身份签名。

Windows ZIP 解压后运行安装器；Linux tar.gz 解压后运行 `python3 install.py`。升级前关闭旧窗口，安装升级不覆盖本地登录态和设置。UU 兼容模式从 v0.5.0 起恢复内置 PC 共享签名值，并保留[可选本地覆盖](connection-credentials.md)；v0.1–v0.4 安装包仍需本地配置。原生观看不依赖这份共享值。
