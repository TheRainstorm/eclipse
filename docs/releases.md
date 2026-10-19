# 构建与版本发布

大功能里程碑使用 `v主版本.次版本.修订号` 标签，标签版本必须与根目录 `Cargo.toml` 的 workspace 版本一致。正式安装包在 [GitHub Releases](https://github.com/TheRainstorm/uu-client/releases) 提供。

## 自动构建

`Build and release` 工作流在主分支提交、PR 和 `v*` 标签上运行，也可从 Actions 页面手动触发构建。当前固定 Rust 1.95.0；Actions 依赖固定完整提交号。

- Linux x86_64：Ubuntu 24.04 构建，启用 VA-API，运行工作区测试并生成 tar.gz。
- Windows x64：Linux 上使用 MinGW win32 线程模型交叉编译，生成 ZIP；随后在 Windows runner 校验所有文件哈希并实际运行 CLI。
- 两个平台都运行凭据分发边界检查；构建不读取服务密钥，测试使用虚构数据。发布包包含许可文件、第三方通知与源提交清单。
- 分支和 PR 只生成保留 14 天的 Actions artifacts；只有标签构建全部成功后，才创建 GitHub Release、附上两端压缩包与 `SHA256SUMS`。

GitHub Actions 的 GUI/GPU 环境不能替代显卡、声音与桌面交互实测。Linux 预编译包要求兼容 Ubuntu 24.04 的 glibc 和所列桌面运行库；其它发行版可按 Linux 文档从源码构建。

## 发布步骤

1. 完成功能并单独提交，更新 README / 已知限制和 workspace 版本、Cargo.lock，并添加 `docs/releases/v版本号.md` 发布说明。
2. 确认主分支 CI 通过，并完成适用于功能的实机验证。
3. 给已验证的提交创建对应版本标签并推送。CI 校验标签与源码版本，重新构建并自动发布。

```bash
git tag -a v0.1.0 -m 'uu-client 0.1.0：跨平台基础串流客户端'
git push origin v0.1.0
```

标签与发布包应保持不变。发现问题应修复后发新版本；不要为了替换二进制移动已发布标签。`SHA256SUMS` 用于检查下载完整性，不代替作者身份签名。

Windows ZIP 解压后运行安装器；Linux tar.gz 解压后运行 `python3 install.py`。升级前关闭旧窗口，安装升级不覆盖本地登录态和设置。首次使用仍需按[连接凭据](connection-credentials.md)配置有权使用的本地凭据，发布包不提供第三方服务密钥。
