# 网易 UU 远程 —— 侦察记录（证据驱动）

> 采集时间：2026-10-15 · 采集主机：ares（本机）· 目标主机：fujistu-arch

## 1. 官方分发包（无 Linux）

`https://uuyc.163.com/download/` 只提供 5 个平台：Windows / macOS / Android / iOS / AndroidTV。

真正的发布 API（从本机探测，非猜测）：

| id | 返回 | 说明 |
|---|---|---|
| `dl/1?channel=gwqd` | `UURemote_Setup_4.42.0.2770_0923142351_gwqd.exe` | Windows x64 |
| `dl/2?channel=gwqd` | `uuyc_4.42.0.apk` | Android |
| `dl/3?channel=gwqd` | `itms-apps://itunes.apple.com/app/id1642306791` | iOS |
| `dl/4?channel=gwqd` | `uuyc_4.43.0_nochannel.pkg` | macOS (xar) |
| `dl/5?channel=gwqd` | `UURemote-tvProduction-3.5.0-30500-2b0f97d4.apk` | AndroidTV |
| 其他 id | 404 / 422 | 不存在 |

端点模板：`https://api.nrd.nie.163.com/api/v1/release/dl/<id>?channel=gwqd`
（同样的模板字符串也存在于 Android APK 的 dex 中。）

**结论：网易未发布任何 Linux / ARM64 客户端。** 网上声称提供 `*.deb` / `*.rpm` / AppImage
的“官方”页面均为第三方仿冒站，不要使用。

## 2. 已下载的样本

| 文件 | 大小 | SHA-256 |
|---|---|---|
| `artifacts/UURemote_Setup_4.42.0.2770.exe` | 89,666,144 | `daa4b299589ba69db5f70a1ae4725abff83cb0f935a9c632432ed70b9a1f907c` |
| `artifacts/uuyc.apk` | 26,899,062 | （见 `artifacts/SHA256SUMS`） |
| `artifacts/uuyc.pkg` | 88,780,400 | （见 `artifacts/SHA256SUMS`） |

### Windows 安装包结构

- 外层：PE32 (GUI) + **Nullsoft Install System** 自解压，数据块位于 `0x38E00` 之后。
- 数据块头部：`00 00 00 00 | EF BE AD DE | "NullsoftInst" | first_hdr_size=0x3B9F2`。
- 7-Zip 23.01 与 7-Zip 25.01（`7zz -tNsis`）**都无法解包**；对偏移 20/24/28/32 做
  props(0..224) × dict(6 种) 的原始 LZMA 暴力解码也全部失败 → **载荷被加密/自定义压缩**。
- 结论：必须**实际执行安装器**（Wine / Windows 虚拟机）才能取得客户端文件。

### macOS 包结构

- 外层 xar，内含 `Payload`（88,744,900 B, offset 20284）。
- Payload 首字节为高熵随机数据，内部无 gzip/xz/cpio/bzip2 签名 → **同样被加密**。

### Android APK（未加密，价值最高）

`lib/arm64-v8a/` 下 19 个 **Linux ABI 的 ELF aarch64 共享库**（Android 即 Linux）：

- `libstreamer.so`（16.5 MB，串联引擎）
- `libstreamer-jni.so`、`libinputmanager.so`、`libntunisdk.so`
- `lib39285EFA.so`（混淆命名，已 strip）、`libnrdsign.so`（签名校验）
- `libsqlcipher.so`、`libmmkv.so`、`libsentry*.so`、`libmarsxlog.so`

`libstreamer.so` 的 `DT_NEEDED` 表明它直接调用 Android 专有 API
（`libmediandk.so` 的 `AMediaCodec_*`、`ANativeWindow_*`、`libEGL/libGLESv2`、`libOpenSLES`），
因此**不能直接搬到桌面 Linux**，但其中的**网络/协议代码可以在桌面上逆向**。

## 3. 协议线索（来自 APK dex）

- `ICE_NETWORK_RELAY`、`setSwitchRelay(Z)` → 使用 **ICE + 中继（relay）** 模型（形似 WebRTC/TURN）。
- 控制面域名：`api.nrd.nie.163.com`、`api-ipv4.nrd.nie.163.com`、`api-pre.nrd.nie.163.com`、
  `api-dcdn.nrd.nie.163.com`、`api-event.nrd.nie.163.com`。
- Web 侧：`uuyc.163.com`、`uuyc.webapp.163.com`（`/netwaker` 唤醒、`/recharge`、`/message-center`）。
- 特征包名：`com.remote.*`，`AndroidUUYCBridge`、`UUYCContract`。

## 4. 既有先例（prior art）

| 项目 | 方式 | 平台 | 许可 |
|---|---|---|---|
| `GuoWQ222/uu-remote-for-linux` | Wine + 原生集成（托盘/输入/画面/编解码），含语义化二进制补丁 | Ubuntu x86_64 | 0BSD |
| `panxuc/uur` | Rust 兼容层，PE ABI 适配器 + PipeWire/portal，架构文档完整 | Linux x86_64 | MIT |
| `lachlanchen/uu-remote-ubuntu-bridge` | Wine + FreeRDP + 输入 broker | Ubuntu | — |
| `GaryOAO/UUWay` | 适配层（API 到 Linux 实现） | Linux | — |

**共同点：都不重新实现协议，而是让官方 Windows 客户端跑在 Wine 里，再用原生层替换
“桌面捕获 / 输入注入 / 托盘 / 编解码”。** 这也是我们的现实路径。
