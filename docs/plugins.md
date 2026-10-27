# 协议插件

Eclipse 的原生串流、设备身份与授权、采集、编解码、输入所有权、声音及帧队列由引擎提供。UU 账号 API、签名、注册、房间与 Socket.IO、兼容控制端及专用 CONTROL/TEXT 协议放在 `plugins/uu-compat`，通过 `uu-core::plugins` 注册连接服务；依赖方向只有插件 → 引擎。

当前插件采用 Rust 构建时链接与显式注册，不是可热加载的 DLL，也不承诺跨版本二进制 ABI。默认构建启用 `uu-compat` 功能，保留已有配置与连接方式。核心库可以独立构建、测试，不加载插件时注册表为空。

`plugins/` 是独立公开仓库 [uu-client-plugins](https://github.com/TheRainstorm/uu-client-plugins) 的 Git 子模块。主项目固定具体提交，不跟随插件分支自动升级。首次获取源码和更新主项目后应初始化相应提交：

```sh
git clone --recurse-submodules https://github.com/TheRainstorm/uu-client.git
# 已有检出：
git submodule update --init --recursive
```

插件 crate 在主 workspace 内构建并继承版本及依赖；单独克隆插件仓库不能独立运行 Cargo。插件仓库维护代码、许可及固定 SDK 的回归工作流，主项目维护应用集成、Cargo.lock 和发布。先提交并推送插件改动，再提交主项目的子模块指针。CI 递归检出子模块；打包拒绝未初始化、提交不符或包含未提交源码的插件检出。GitHub 自动生成的主仓库源码压缩包不含子模块内容，构建请使用递归克隆。

```sh
cargo build --release -p uu-gui -p uu-cli --no-default-features
cargo test --release -p uu-core
```

不启用默认功能的应用不能发起 UU HTTP、WebSocket 或兼容会话，原生 IP 和自建连接服务仍可使用。`ECLIPSE_UU_PLUGIN=0` 可在启动时禁用默认构建中的 UU 插件；后台主机也遵守此开关。旧设备配置中的 `uu` 路由标识保留，以便以后重新启用；它不使核心自动连接第三方服务。

插件调用引擎的受限媒体接口，共用有界接收、解码及帧所有权。兼容剪贴板的碎片组装状态由该会话持有，核心剪贴板仅保存许可、内容与计数。插件注册必须显式进行，重复 ID 被拒绝；原生公钥验证不会因为未识别 SDP 而降级。

## 来源与许可

此目录首先迁移本仓库已有的兼容实现，并保留原有来源说明。拆目录、可选链接或分包都不自动消除既有许可义务。需要继续依据实际代码来源审查发布内容；本次没有复制 GPL 参考项目的被控端实现。新增第三方实现应有独立来源、许可证和分发说明，不能把“插件”作为省略这些义务的理由。

UU 官方客户端兼容被控入口与原生公钥协议不同。其能力与实测范围另见 [官方客户端兼容说明](official-mobile-client-feasibility.md)；启用 UU 寻址不代表官方手机已经可以串流。
