# 本地连接凭据

源码与安装包不包含第三方生产签名密钥。MIT / Apache 许可不授予第三方服务或密钥的使用权，用户须自行取得有权使用的凭据；如果无法取得，本客户端暂不能完成官方服务登录与连接。项目不提供密钥、密钥提取程序或在线签名代办服务。

客户端在**运行时**读取：

- 环境变量 `UU_SIGNING_KEY_FILE` 指定的文件；或
- 默认数据目录下的 `signing-key`：`$XDG_DATA_HOME/uu-remote/signing-key`，未设置 XDG 时为 `~/.local/share/uu-remote/signing-key`。Windows 默认以用户个人目录作为 `~`。

文件使用 UTF-8 单行文本，可带 UTF-8 BOM 和行尾换行；内容不会写入版本清单、普通设置、日志或错误信息。Unix 文件权限必须仅允许本人访问，例如 `600`，不能放在公开共享目录。

设置文件位置，不要把实际密钥放在命令行：

```bash
export UU_SIGNING_KEY_FILE="$HOME/.config/uu-client/signing-key"
chmod 600 "$UU_SIGNING_KEY_FILE"
```

```powershell
$env:UU_SIGNING_KEY_FILE = Join-Path $env:USERPROFILE '.config\uu-client\signing-key'
```

先使用本地安全编辑器创建凭据文件，再启动客户端。Windows 应将文件放在自己的用户目录并限制其他账户访问。不要提交文件、把内容粘贴进 issue、使用含密钥的截图，或在命令参数中写出实际值。环境变量只携带路径；设置它不会自动创建凭据。

本地开发和发布使用同一套源码：构建脚本不会读取或嵌入凭据。不要使用 `include_str!`、编译环境变量、加密常量或私有 DLL 把第三方共享密钥重新打进发布包，这些方式仍可被提取。

这种方案解决的是**项目不再分发密钥**，并不让共享密钥变得不可提取，也不能撤回已经公开过的副本。既要公开客户端并开箱即用，又要让客户端长期隐藏它必须使用的共享密钥，无法仅靠客户端代码可靠实现。长期方案需要官方授权的公开认证机制，或本项目自有服务端的认证体系。
