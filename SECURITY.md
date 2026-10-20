# 安全与凭据

开发版为兼容 PC 客户端而明确分发共享的请求签名值，并允许本地文件覆盖。该值是本分发包中公开可读取、可提取的兼容数据，不能作为用户身份或设备权限的替代；这不表示获得官方授权。范围与版本区别见 [连接签名配置](docs/connection-credentials.md)。

个人账号令牌、二维码凭据、原生邀请和本地覆盖文件始终属于私有数据，不进入源码、CI secrets 或安装包。除已明确记录的 PC 共享值外，不分发其它第三方服务密钥。

发现凭据或安全问题时，请使用 [GitHub 私密漏洞报告](https://github.com/TheRainstorm/uu-client/security/advisories/new)。请勿在公开 issue、截图、日志或 PR 中粘贴密钥、账号 token、设备访问凭据或原始抓包。

## 开发与发布检查

```bash
python3 tools/security/scan_secrets.py --history
python3 -m unittest discover -s tools/security -p 'test_*.py' -v
```

扫描工具检查意外提交的本地凭据文件、未批准的服务签名常量和源码中的 EXE / DLL，也识别改名后的原生邀请与账号状态 JSON。公开 PC 常量只按固定源码路径、常量名及精确指纹允许；修改值或复制到其它文件仍会报错。它是针对本项目分发边界的检查，不是完整的安全审计，也不会修改 GitHub 的扫描或推送保护。

账号状态在同一目录创建私有临时文件并原子替换，避免其它窗口读取到半写入的登录态；Unix 下新文件权限为600，更新时也会收紧旧文件权限。Windows 继承所在用户目录的 ACL。写入失败时保留原文件并清理本次临时文件。账号与二维码的 `Debug` 输出隐藏凭据和身份信息；这不替代本机账号保护或操作系统凭据保险库。

本地还可提供一个只保存在自己电脑上的凭据文件做精确匹配；扫描同时检查常见编码，输出仅包含位置与原因，不输出匹配内容：

```bash
python3 tools/security/scan_secrets.py --history \
  --forbid-file "$HOME/.config/uu-client/signing-key" \
  --artifact /path/to/release.zip
```

`--forbid-file` 可多次传入，显式禁止的值即使恰好是公开 PC 常量也会报错。**不要为了执行精确匹配，把个人或其它未批准密钥放进仓库或 CI secrets**。自动构建只使用公开源码；测试数据使用标准向量与虚构凭据。

## 历史清理说明

本仓库曾为移除签名凭据而重写历史，并移除了旧二进制。之后调整为明确分发 PC 兼容共享值，不恢复旧二进制、其它凭据或个人资料。功能提交顺序保留，提交号已改变。如果你持有旧克隆，请先备份自己未提交的工作，再重新克隆；不要把旧历史合并或推回公开分支。

重写历史无法撤回已下载的副本，也不能保证平台缓存已清除。泄露凭据的撤销或轮换需要由其服务提供方处理；平台缓存清理应按 [GitHub 官方流程](https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/removing-sensitive-data-from-a-repository)办理。
