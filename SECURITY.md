# 安全与凭据

本项目不分发第三方生产签名密钥，不在 CI 中配置生产签名密钥，也不将凭据编译进发布包。运行时配置方式见 [连接凭据](docs/connection-credentials.md)。

发现凭据或安全问题时，请使用 [GitHub 私密漏洞报告](https://github.com/TheRainstorm/uu-client/security/advisories/new)。请勿在公开 issue、截图、日志或 PR 中粘贴密钥、账号 token、设备访问凭据或原始抓包。

## 开发与发布检查

```bash
python3 tools/security/scan_secrets.py --history
python3 -m unittest discover -s tools/security -p 'test_*.py' -v
```

扫描工具检查意外提交的本地凭据文件、服务签名密钥常量和源码中的 EXE / DLL，也识别改名后的原生邀请与账号状态 JSON。它是针对本项目分发边界的检查，不是完整的安全审计。

账号状态在同一目录创建私有临时文件并原子替换，避免其它窗口读取到半写入的登录态；Unix 下新文件权限为600，更新时也会收紧旧文件权限。Windows 继承所在用户目录的 ACL。写入失败时保留原文件并清理本次临时文件。账号与二维码的 `Debug` 输出隐藏凭据和身份信息；这不替代本机账号保护或操作系统凭据保险库。

本地还可提供一个只保存在自己电脑上的凭据文件做精确匹配；扫描同时检查常见编码，输出仅包含位置与原因，不输出匹配内容：

```bash
python3 tools/security/scan_secrets.py --history \
  --forbid-file "$HOME/.config/uu-client/signing-key" \
  --artifact /path/to/release.zip
```

`--forbid-file` 可多次传入；**不要为了让 CI 执行精确匹配，把第三方生产密钥放进仓库或 CI secrets**。自动构建只使用公开源码与虚构测试数据。

## 历史清理说明

本仓库曾为移除不应分发的签名凭据而重写历史，并移除了包含凭据的旧二进制。功能提交顺序保留，提交号已改变。如果你持有旧克隆，请先备份自己未提交的工作，再重新克隆；不要把旧历史合并或推回公开分支。

重写历史无法撤回已下载的副本，也不能保证平台缓存已清除。泄露凭据的撤销或轮换需要由其服务提供方处理；平台缓存清理应按 [GitHub 官方流程](https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/removing-sensitive-data-from-a-repository)办理。
