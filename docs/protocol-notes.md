# UU 远程 —— 协议逆向笔记

> 证据驱动。每条结论都标注了来源与复现方式。采样日期 2026-10-15。

## 0. 样本

| 目标 | 大小 | SHA-256 |
|---|---|---|
| `UURemote_Setup_4.42.0.2770.exe`（Windows，NSIS） | 89,666,144 | `daa4b299…f907c` |
| `uuyc.apk`（Android） | 26,899,062 | `e725bd6d…0a198` |
| `uuyc.pkg`（macOS，xar） | 88,780,400 | `9ec5d9f6…e6a8c` |

发布源（**无 Linux 客户端**）：`https://api.nrd.nie.163.com/api/v1/release/dl/<id>?channel=gwqd`，
id 1=Windows 2=Android 3=iOS 4=macOS 5=AndroidTV，其余 404/422。用本仓库
`uu release-check` 可实时验证（采样时为 Windows 4.43.0.3607）。

## 1. 客户端结构（Windows，安装后实测）

```
Program Files/Netease/GameViewer/
  GameViewer.exe              # 仅启动器：拉起 bin/GameViewer.exe 后自身退出
  GameViewerService.exe       # 注册为 Windows 服务（ImagePath 指向自身）
  bin/GameViewer.exe          # 主控 UI（Qt5 + WebView2）
  bin/GameViewerServer.exe    # 被控端核心（串流/终端）
  bin/GameViewerLauncher.exe  bin/GameViewerHealthd.exe  bin/Upgrade.exe
  bin/nrd_renderer_ipc.dll    # NetEase 渲染 IPC
  bin/drivers/GameViewerIddDriver/   # 虚拟显示器（IDD，内核驱动）
  bin/drivers/uuycinput/             # 输入注入（内核驱动）
  bin/drivers/ViGEmBus/              # 虚拟手柄（内核驱动）
  bin/drivers/nevaudio/              # 虚拟声卡
  bin/WebView2Loader.dll  bin/MicrosoftEdgeWebview2Setup.exe
  bin/html/skeleton/...              # WebView2 承载的 UI 资源
```

要点：**主控 UI 与内核驱动无关**；IDD/输入/手柄三个内核驱动只在被控端起作用，
纯用户态无法在 Wine 里复现 —— 这是移植的关键分割线。

Windows 安装包载荷经加密（7-Zip 23/25 与原始 LZMA 暴力解码均失败），
macOS 的 xar `Payload` 同样加密，**只能执行安装器取文件**。

## 2. 媒体面 = 标准 libwebrtc（最重要的结论）

`uuyc.apk` 内的 `lib/arm64-v8a/libstreamer.so`（16.5 MB，未加密 ELF aarch64）里
直接包含 libwebrtc，证据分四类：

**a) 源码路径字符串**（编译期 `__FILE__`，最具决定性）

```
builds/nrd/nrd-streamer/webrtc/pc/ice_server_parsing.cc
builds/nrd/nrd-streamer/webrtc/modules/video_coding/h264_sprop_parameter_sets.cc
builds/nrd/nrd-streamer/webrtc/rtc_base/ssl_fingerprint.cc
builds/nrd/nrd-streamer/webrtc/third_party/opus/src/silk/check_control_input.c
```

内部代号 **`nrd-streamer`**（nrd = NetEase Remote Desktop）。

**b) SDP / ICE / SRTP 运行期字符串**

```
a=group:BUNDLE        ice-ufrag        ice-pwd        fingerprint
 candidate: <...>     rtpmap           sprop-sps/sprop-pps
Called with SDP without DTLS fingerprint.
Called with SDP without ice-ufrag and ice-pwd.
AllocationSequence: No relay server configured, skipping.
Applying the same SRTP recv parameters again. No-op.
All candidates gathered for pooled session.
```

**c) 原生导出**：`Java_org_webrtc_NetworkMonitor_nativeNotify*`（上游 `org.webrtc` JNI 名称）

**d) dex**：`Lorg/webrtc/{NetworkMonitor,Logging,NetworkChangeDetector,...}`，
以及应用层包装 `Lcom/remote/streamer/controller/PeerConnectionState;`

音频为 **Opus**（`WebRtcOpus_*`），视频为 **H.264/HEVC**（`h264_sprop_parameter_sets.cc`），
加密为 **DTLS-SRTP**。

> 结论：媒体面不需要自研协议。Rust 侧可直接用 `webrtc`(webrtc-rs) / `str0m`
> 这类实现承载 ICE+DTLS-SRTP，把工作量集中到**信令与鉴权**。

## 3. 控制面 / 信令

| 域名 | 用途 |
|---|---|
| `api.nrd.nie.163.com` | 主 API（含发布源 `/api/v1/release/dl/`） |
| `api-ipv4.nrd.nie.163.com` | IPv4 专用入口 |
| `api-pre.nrd.nie.163.com` | 预发布 |
| `api-dcdn.nrd.nie.163.com` | CDN 相关 |
| `api-event.nrd.nie.163.com` | 事件/埋点 |
| `uuyc.webapp.163.com` | `/netwaker`（网络唤醒）、`/recharge`、`/message-center` |
| `uuyc.163.com` | 官网 / EULA |

中继开关：dex 里存在 `ICE_NETWORK_RELAY`、`setSwitchRelay(Z)`。

## 4. 鉴权（最大风险点）

* Android 带 `libnrdsign.so`（签名校验库）；Windows 客户端链入 **CryptoPP**。
* 登录走 WebView2 承载的 Web SSO，另有设备级签名/凭证。
* 相关签名算法与设备指纹**尚未还原** —— 这是能否与官方服务互通的**前置阻塞**。


## 2.6 请求签名格式与兼容性验证

**结论：服务端接受我们独立构造的签名。**

```
GET /api/v1/release/fetch/update  ->  {"code":1151,"msg":"当前已是最新版本"}
```

（错误码从 `1001 无效的请求参数` → `1003 无效签名` → `1151 已是最新版本`，
这条错误码序列把"参数值"和"签名"两个问题精确分开了。）

### 算法

```
headers  = 所有名字以 x-param- 开头的请求头（排除 x-param-sign）
           小写化、按名字升序排序、渲染为 k=v、用 & 连接
           （空值也要保留，例如 x-param-opr=）
body     = 实际发送的 JSON 字符串；无 body 时为空串
message  = METHOD + pathWithQuery + headers + body
signature= hex(HMAC-SHA256(key, message))
```

### 签名凭据由用户在本地提供

原生接口与 Web 接口的凭据不同，不能互换。本项目不分发第三方生产密钥，
源码、测试、安装包都不包含有效密钥。客户端仅在运行时读取用户有权使用的本地配置；
配置方式见 [连接凭据](connection-credentials.md)。缺少配置时明确报错，不尝试默认密钥或远端取密钥。

### 请求头真实取值（Windows 构建）

```
X-Param-PLAT: 1                      X-Param-VN: 4.40.1.2090
X-Param-VC: 2090                     X-Param-PKGN: com.netease.uuremote
X-Param-CHN: gwqd                    X-Param-LANG: zh-CN
X-Param-CNT: zh-CN                   X-Param-REL: prod
X-Param-OPR: (空)                    X-Param-system-id: (空)
X-Param-TS: <unix 秒>                X-Param-client-id: <uuid>
X-Param-device-id: <16 位>           X-Param-user-id: <登录后才有>
```

### 验证边界

规范串和 HMAC 格式曾与实际交互进行本地对照；公开仓库不保留生产密钥及依赖它的测试向量。
实现见 `crates/uu-core/src/sign.rs`，公开测试采用 RFC 4231 和虚构请求，验证排序、空值、签名头过滤与原始请求体。
凭据读取、权限及缺失处理见 `crates/uu-core/src/signing_key.rs`。

### 实现状态

| 项 | 状态 |
|---|---|
| 签名算法 | ✅ 已实现；公开测试只用标准向量与虚构数据 |
| 线上验证 | ✅ `uu api /api/v1/release/fetch/update` → 1151 |
| 登录流程（QR） | 🚧 端点已知，**请求/响应 body 形状待抓** |
| 设备列表 / 建会话 | 🚧 端点已知，需登录态 |

已知登录相关端点：
```
/api/v1/qrcode/gen/login      /api/v1/qrcode/login/status    /api/v1/login/by_qrcode
/api/v1/login/by_mobile       /api/v1/security/mobile/code   /api/v1/user/info
/api/v1/user/logout           /api/v1/guest/create           /api/v1/user/ticket/for/wvlogin
```


## 2.7 登录流程 —— 端到端已验证 ★★★

整条链路已在真实账号上跑通（`uu login` → `uu devices` 返回 `code:0`）。

```
1) POST /api/v1/device/windows/init
   body: {name, client_id, system_id, machine_guid, os, base_board,
          cpu, video[list], mac, memory, screen}
   → {device_id, platform, alias, controllable, controlled_support, ...}
   作用：注册本机并取得 device_id（client_id 用自己生成的 UUID 即可）

2) POST /api/v1/qrcode/gen/login            （无 body）
   → {qrcode_jump_url, token}                （二维码 URL 与轮询 token）

3) POST /api/v1/qrcode/login/status
   body: {login_status, qrcode_jump_url, token}
   → 长轮询。login_status 取值 1..4（其它值 422）：
       1 = 等待扫码（无扫码时约 60s 超时返回 1）
       2 = 已扫码
       4 = 流程结束（二维码已被消费）
   实测：未扫码时四个值都会阻塞，所以它不是一个状态回传字段。

4) POST /api/v1/login/by_qrcode
   body: {qrcode_jump_url, token}
   → {user_id, nickname, token, coin_balance, ...}
   ★ 必须在 status=4 之后调用；提前调用返回 1194「扫码登录过程出现异常」

5) GET /api/v1/device/list
   headers: Authorization: Bearer <token>  +  X-Param-user-id: <user_id>
   → {current_device, my_binded_devices[]}
```

### 实现要点（踩过的坑）

| 坑 | 说明 |
|---|---|
| `client_id` 必须是注册过的 UUID | 随机 UUID 调 `gen/login` 会 1001 |
| `X-Param-device-id` 必须是注册返回的 device_id | 用 system_id 会 1001 |
| `by_qrcode` 的调用时机 | 早于 `status=4` 会 1194 |
| 凭据作用域不可混用 | 原生接口需要对应的用户本地凭据，不使用 Web 接口凭据 |
| 终端二维码渲染 | 必须用半块字符（`▀▄█`）；一模块一字符会被拉成 2:1 无法扫描 |

### 当前实现

```
uu login    注册（如需）→ 出二维码 → 长轮询 → 换 token → 存 state.json
uu devices  用保存的 token 拉设备列表
uu api      带签名的手工探测（调试/发现新接口用）
```
状态文件：`~/.local/share/uu-remote/state.json`（identity / device_id / token / user_id）


## 2.8 会话与媒体面（room/join 已通，TLS 内协议待解）

### 已验证的会话建立

```
1) GET  /api/v1/device/{device_id}/preferences
2) POST /api/v1/room/join/by_device/{device_id}   {"force_join":false}
   → {signaling_server, signaling_list[], token, report_url,
      report_server_address, report_token, ws_connect_timeout_ms,
      streamer_retry_delta_ms, max_reconnect_delta, international_connect}
3) 媒体/控制通道：主控端直接 TCP 连到被控端
   实测：192.0.2.10:50986 → 192.0.2.20:63794  ESTABLISHED
   被控端 GameViewerServer 在该端口 LISTEN（另有 UDP socket）
```

### 传输层事实

- 该 TCP 连接是 **TLS**（抓到的记录头 `17 03 03 00 xx`）。
- `streamer.dll` **静态链接了 `OpenSSL 1.1.1w+quic`**，并含 `DTLS`/`SRTP` 字符串；
  它不走系统 `libssl`/`Schannel` 导出函数 —— 所以常规挂钩抓不到明文。
- 文件内还包含 libwebrtc 风格的 `OpenSSLAdapter`（`SSL_write couldn't write ...`）。

### 已定位的函数（当前 19,108,856 字节构建，Windows 测试客户端 实测）

真正的 TLS 读写入口（动态验证，不是猜测）：

```
SSL_write   RVA 0xDD7F30   rcx=SSL*, rdx=plaintext, r8=len  -> rax=写入字节数
SSL_read    RVA 0xDD7690   rcx=SSL*, rdx=buf,       r8=len  -> rax=读取字节数
```

它们由适配器调用：

```
OpenSSLAdapter::Write wrapper 0xC9514  -> call 0xDD7F30
OpenSSLAdapter::Read  wrapper 0xC9A58  -> call 0xDD7690
socket writer 0xCA36E / reader 0xCA3EE  （此处已是 TLS 密文记录）
```

⚠️ 旧版（18,618,360 字节 artifact）里的 `0xC7DC0/0xC8018/0xD26BF0` 在当前版本不可用。
⚠️ `.rdata` 里的 `{name, 0x14xxxxxx}` 表是构建期符号表，**不是**可解引用的函数指针，
    `0x1420C000 → +0x20C000` 在当前构建里并不是 `SSL_write_ex`。

### TLS 内控制协议 —— 已抓到明文 ★★★

动态方法：Frida 挂 `streamer.dll+0xDD7F30`（写）与 `+0xDD7690`（读），
在 **加密前/解密后** 拿明文。脚本：`tools/win/wirecap.js` / `tools/win/wirecap.py`。

实测在加密边界能看到三类明文消息（抓包点相同，但 PCK 与 TURN 可能来自
不同的 TLS 连接）：

1. `PCK` 控制帧（JSON 输入事件、ACK、心跳）
2. TURN Send/Data Indication（STUN 0x0016/0x0017，含 XOR-PEER-ADDRESS + DATA）
3. `13 88 13 88` 分块消息（统计/窗口信息，JSON 跨帧拼接）

#### PCK 帧头（28 字节，小端）

```
off  size  field
0    3     "PCK"
3    1     0x02              version
4    2     "V3"              proto
6    2     0x0200            flags
8    1     type              0x51 DATA / 0x52 ACK / 0x55 DATA2 / 0x57 HEARTBEAT
9    4     seq (u32 BE)
13   4     timestamp (u32 LE)
17   2     channel (u16 LE)
19   1     reserved
20   4     kind (DATA) / ack (ACK)
24   4     payload_len (u32 LE)
28   ...   payload + 03 00 01 00
```

鼠标移动实测（PCK 通道，type=0x51, kind=7/8）：

```json
{"abs_x":0.7096247960848288,"abs_y":0.8268115942028985,
 "action":"mouse_move_absolute","screen_id":1}
```

`mouse_move_absolute` 实测 kind=7、channel=0；
`mouse_move_relative` 实测 kind=8、channel=屏幕序号（1/2/3）。
WebRTC data channel 上的鼠标用 kind=15/16 也可生效，这是两条不同的承载路径。

ACK（type=0x52，28 字节）会回显被确认帧的 timestamp 与 seq，
并在 offset 20 给出下一个期望序号。

心跳（type=0x57，37 字节）固定为 `PCK\x02V3\x02\x00` + 计数器 + 0 填充。

#### TURN / ICE

```
00 16 00 78 21 12 a4 42 <12B txid> <STUN attrs>
```

- `0x0016` = 客户端→服务端 channel（TURN ChannelData）
- `0x0017` = 服务端→客户端 channel
- 第 3-4 字节 = STUN 消息长度（big-endian）
- 内部是标准 STUN Binding Request/Response，含 `0x0012 XOR-PEER-ADDRESS`、
  `0x0013 DATA`、`0x0024 PRIORITY`、`0x8028 FINGERPRINT`

#### 13 88 13 88

分块消息头 28 字节，payload 是 JSON（可能跨帧）：
`{"connection_period_stats_event":{...}}`、`{"exe_name":"SearchHost.exe",...}`。
用于会话统计和前台窗口/进程上报。

#### 输入事件 action（实测抓包）

payload 是 JSON，末尾固定 4 字节 `03 00 01 00`（协议版本 3 + screen/flags）。

| action | kind | 字段 |
|---|---|---|
| `mouse_move_absolute` | 7（PCK） | `abs_x`(0..1), `abs_y`(0..1), `screen_id` |
| `mouse_move_relative` | 8（PCK） | `delta_x`, `delta_y`, `mousetype`(2) |
| `mouse_press` | 15（data channel 实测） | `button`（1=左键）, `screen_id` |
| `mouse_release` | 15（data channel 实测） | `button`, `screen_id` |
| `mouse_scroll` | 16（data channel 实测） | `delta_x`, `delta_y`（120 = 一格） |
| `kbd_press` | 8（PCK） | `key`(Windows VK 码), `kseq`, `interrept`("i") |
| `kbd_release` | 8（PCK） | 同上 |

实测样本：

```json
{"action":"mouse_press","button":1,"screen_id":1}
{"action":"kbd_press","interrept":"i","key":72,"kseq":9}
{"action":"mouse_scroll","delta_x":0,"delta_y":120}
```

`kseq` 是键盘事件序号，每个 press/release 递增。
`key` 是 Windows virtual-key code（VK_H = 72）。

### PCK / 键盘通道（2026-10-17 更新）★★★

**这里修正早期结论：键盘不是直连被控端 TCP 端口的 TLS。**
被控端 TCP host candidate 先走 ICE-TCP STUN（`u16 BE 长度 + STUN`），
STUN 成功后对端还会发自己的 Binding Request，必须先回 Success；之后该端口
说的是 `0x04` 前缀的自定义协议，不是普通 TLS。

官方键盘实际上走 **TURN-over-TLS 到中继**，再由中继转发给被控端。
PCK 帧格式：

```
kind=8, channel=0, ptype=0x51
{"action":"kbd_press","interrept":"i","key":<VK>,"kseq":<contiguous>}
{"heartbeat":"kbd_heartbeat"}     // 每 ~200ms
```

`kseq` 从 1 开始连续递增；`key` 是 Windows VK 码（72='H'，91=LWIN，
160=LSHIFT，162=LCTRL，164=LALT）。

以上是早期 TURN/PCK 路径的观测。当前客户端的键盘输入使用 WebRTC CONTROL 通道的原生 VK 消息。

### 待解

- [x] TURN `CreatePermission` + `Send/Data Indication` 的 Rust 实现
- [x] 被控端 relay/srflx candidate 的完整 trickle 收集与批量 Permission
- [ ] 被控端低层键盘注入的端到端验收
- 剪贴板/文件传输的 action
- 视频/音频传输通道（UDP/QUIC，或 TURN ChannelData 内）
- 被控端 `0x04` 自定义协议的用途（仅在中继路径仍不通时才需要继续逆）
## 5. 待办（下一轮 RE）

1. Ghidra（REA 的原生 provider）就绪后，对 `libstreamer.so` 做：
   - 交叉引用 `ice_server_parsing` / `h264_sprop` 字符串，定位会话参数装配点；
   - 找 `setSwitchRelay` 调用方，还原中继选择逻辑。
2. 在 dex 里追 `com.remote.streamer.controller.*`，还原**信令消息**（谁发 SDP、经哪个接口）。
3. 抓一次真机登录/建连的 HTTPS 流量（本机代理或 Frida），确定鉴权头与签名输入。
4. 提取 Windows 客户端的 `nrd_renderer_ipc.dll` 导出，确认主机侧渲染 IPC 形状。
