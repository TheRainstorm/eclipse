# 交接文档 —— uu_remote（UU 远程的原生 Rust 重写）

最后更新：2026-10-17 23:05 ｜ 语言：中文 ｜ 状态：**鉴权/登录/媒体/GUI 已完成；远端视频连续解码、键盘与鼠标输入均已打通**

---

## 0. 一句话现状

用 Rust 原生实现网易 UU 远程（`uuyc.163.com`）的客户端，目标 Windows + Linux 双端。

- ✅ **鉴权（签名）完全破解**，并线上验证
- ✅ **登录全链路跑通**：自注册设备 → 扫码登录 → 拿 JWT → 拉设备列表（`uu devices` 返回 `code:0`）
- ✅ **会话/媒体面跑通**：Socket.IO 信令 + WebRTC，Rust 端收到并解码 1920×1080 H264
- ✅ **Windows GUI 端到端可用**：设备列表、扫码登录、远程画面、鼠标、全屏/多显示器
- ✅ **键盘通道定位完成**：官方键盘走 **TURN-over-TLS 到中继**，不是直连被控端 TLS
- ✅ **Rust TURN/PCK 中继**：已实现 TLS、Allocate、CreatePermission、Send/Data
  Indication，并在真实 room/join 会话中拿到 `0x0103` Allocate Success
- 🚧 **最后一个缺口**：被控端仍未通过低层键盘钩子确认收到 Rust 发出的键盘帧

---

## 1. 关键成果（可直接复现）

```bash
cargo test --workspace        # 43 个测试全绿
cargo run --bin uu -- devices # 返回 code:0 + 账号下真实设备
cargo run --bin uu -- self-loop --frames 120   # 自有媒体管线自检（UDP + H264 + PSNR）
cargo run --bin uu -- kbd-test aeawvmepembcpca5 --text HELLO
```

`uu devices` 实测输出：

```
code: 0
  FYYUAN-5NET3-53    aeawviiagqbbnfza  CONNECTED
  X358H              aeawvih7lubbnfna  CONNECTED
  KVM-WIN11          aeawvmepembcpca5  DISCONNECTED
  LAPTOP-ARCH        aeawvuhsnibevndc  DISCONNECTED
```

---

## 2. 缺口 1：请求签名（已完成）

### 算法

```
headers   = 所有 x-param-* 请求头（排除 x-param-sign），小写化、按名升序、
            渲染成 k=v 用 & 连接（空值也保留）
body      = 实际发送的 JSON 字符串；无 body 时为空串
message   = METHOD + pathWithQuery + headers + body
signature = hex(HMAC-SHA256(message, key))
```

### 两套密钥（不可混用）

| 用途 | 密钥 |
|---|---|
| **原生客户端（我们要用）** | `REMOVED_SERVICE_KEY_0000` |
| Web bundle（仅网页） | `REMOVED_SERVICE_KEY_0000` |

### 请求头真实取值（Windows 构建）

```
X-Param-PLAT: 1              X-Param-VN: 4.40.1.2090
X-Param-VC: 2090             X-Param-PKGN: com.netease.uuremote
X-Param-CHN: gwqd            X-Param-LANG: zh-CN
X-Param-CNT: zh-CN           X-Param-REL: prod
X-Param-OPR: (空)            X-Param-system-id: (空)
X-Param-TS: <unix 秒>        X-Param-client-id: <uuid>
X-Param-device-id: <注册返回> X-Param-user-id: <登录后>
```

### 证据

- 挂钩 `BCryptCreateHash/HashData/FinishHash` 抓到 53 组 (key, message, digest)，**17/17 全部匹配**
- 错误码演进可作判据：`1001 无效参数` → `1003 无效签名` → `1151 已是最新版本`（签名通过）
- 实现：`crates/uu-core/src/sign.rs`（含真实向量测试）

---

## 3. 缺口 2：登录（已完成）

```
1) POST /api/v1/device/windows/init
   {name, client_id, system_id, machine_guid, os, base_board, cpu,
    video:[list], mac, memory, screen}
   → {device_id, alias, controllable, controlled_support, ...}

2) POST /api/v1/qrcode/gen/login        （无 body）
   → {qrcode_jump_url, token}

3) POST /api/v1/qrcode/login/status
   {login_status: 1..4, qrcode_jump_url, token}
   长轮询；1=等待（约 60s 超时），2=已扫码，4=流程结束（码已被消费）

4) POST /api/v1/login/by_qrcode
   {qrcode_jump_url, token}
   → {user_id, nickname, token(JWT), ...}
   ★ 必须在 status=4 之后调用；提前调用返回 1194

5) GET  /api/v1/device/list
   Authorization: Bearer <token> + X-Param-user-id
```

### 踩过的坑（都已修）

| 坑 | 现象 |
|---|---|
| `client_id` 必须是已注册 UUID | 否则 `1001` |
| `X-Param-device-id` 用注册返回的 device_id（不是 system_id） | 否则 `1001` |
| `by_qrcode` 调用时机 | 早于 `status=4` → `1194` |
| 两套密钥混用 | `1003 无效签名` |
| 终端二维码渲染 | 必须半块字符（`▀▄█`）；一模块一字符会拉成 2:1 扫不出 |

### 状态文件

`~/.local/share/uu-remote/state.json`：`identity / device_id / token / user_id`

---

## 4. 缺口 3：会话与媒体（媒体面已完成，键盘通道收尾）

### 总体架构（实测）

```
API  : /api/v1/room/join/by_device/{id}  -> signaling_server + token + iceServers
信令 : Socket.IO (EIO=4) over WebSocket，路径 /socket.io/?EIO=4&transport=websocket
媒体 : 标准 WebRTC（ICE/DTLS/SRTP），7 条 m-line
鼠标 : WebRTC data channel 上的 PCK 帧（实测有效）
键盘 : TURN-over-TLS 到中继，再以 PCK 帧转发给被控端（Rust 已实现，注入待确认）
```

### 动态抓 TLS 明文（已验证）

`streamer.dll`（当前 19,108,856 字节构建）的明文边界：

```
SSL_write  RVA 0xDD7F30   rcx=SSL*, rdx=plaintext, r8=len  -> rax=写入字节数
SSL_read   RVA 0xDD7690   rcx=SSL*, rdx=buf,       r8=len  -> rax=读取字节数
```

适配器调用点：`0xC9514 -> call 0xDD7F30`，`0xC9A58 -> call 0xDD7690`。
更下层 `0xCA36E/0xCA3EE` 看到的已经是 `17 03 03` TLS 密文记录。

Frida 脚本：`tools/win/wirecap.js` / `wirecap.py`（只读，挂上也验证过不会崩）。

⚠️ 旧版 artifact（18,618,360 B）的 `0xC7DC0/0xC8010/0xD26BF0` 在当前版本不可用。

### 信令：Socket.IO + WebRTC

WebSocket 帧里是 Socket.IO EIO=4 文本事件：

| event | 方向 | 内容 |
|---|---|---|
| `success` | S→C | `client_id`, `force_relay`, `iceServers`（stun/turn） |
| `forward_setting` | S→C | `device_capability`（显示器、编码能力）、`sender_para_info` |
| `soac` | 双向 | SDP offer/answer + trickle ICE candidate |
| `control` | C→S | 会话控制 |
| `release` | C→S | 释放会话 |
| `ice_finished_log` | C→S | ICE 完成上报 |

SDP 是 **gzip 压缩的二进制附件**（Socket.IO binary event）。实测：

- controller offer：`audio + 5x video + application(webrtc-datachannel)`
- controlled answer：5 路 video 都是 `sendonly`（每台显示器一路）
- 视频 codec：H265(96) / H264(98)，带 RTX/RED/ULPFEC/flexfec/rs-fec
- 音频：opus
- data channel：`m=application 9 UDP/DTLS/SCTP webrtc-datachannel`，
  带 `a=x-uuremote-mix-kcp:2`、`a=x-uuremote-quic`

### PCK 控制帧（实测）

28 字节头（除非注明，均为小端）：

```
0   3   "PCK"
3   1   0x02
4   2   "V3"
6   2   0x0200
8   1   ptype: 0x51 DATA / 0x52 ACK / 0x55 DATA2 / 0x57 HEARTBEAT
9   4   seq (u32 BE)
13  4   timestamp (u32 LE)
17  2   channel (u16 LE；鼠标=屏幕序号，键盘=0)
19  1   reserved
20  4   kind (u32 LE)
24  4   payload_len (u32 LE，包含末尾 4 字节 trailer)
28  ..  payload + 03 00 01 00
```

注意：旧文档把 seq 写成 offset 16、u16，是不对的；以当前
`crates/uu-proto/src/pck.rs` 和抓包为准。

实测帧类型：

| 方向 | ptype | kind | channel | payload |
|---|---|---|---|---|
| S→C | 0x51 | 0 | 0 | protobuf hello（40B） |
| C→S | 0x51 | 2 | 0 | capability protobuf（26B + trailer） |
| C→S | 0x51 | 8 | 1/2/3 | `mouse_move_relative` JSON |
| C→S | 0x51 | 7 | 0 | `mouse_move_absolute` JSON |
| C→S | 0x51 | 8 | 0 | `{"heartbeat":"kbd_heartbeat"}` |
| C→S | 0x51 | 8 | 0 | `kbd_press` / `kbd_release` JSON |
| S→C | 0x52 | 1..N | 0 | ACK，payload_len=0 |
| 双向 | 0x57 | 0 | 0 | 37 字节传输心跳 |

官方 capability hello（客户端 kind=2）的 protobuf 前缀：

```
1a18221608061003180320022801300238024003480150015801
```

### 键盘通道（最新结论）★★★

**不要再直连被控端的 TCP host candidate 做 TLS。** 实测流程是：

1. 被控端 TCP host candidate（例 `192.168.35.6:63347`）先走
   **ICE-TCP STUN**：`u16 BE 长度 + STUN Binding Request`。
2. 我们发的 Binding Request：
   - USERNAME = `<remote_ufrag>:<local_ufrag>`
   - `0xC057=999`、`0xC070=0`
   - ICE-CONTROLLING = `0x8029`，tiebreaker 随机 u64
   - PRIORITY = `0x6e0020ff`
   - MESSAGE-INTEGRITY = HMAC-SHA1，key 是 **remote_pwd**
   - FINGERPRINT
3. 对端可能回 `487 Role Conflict`，把 0x8029 换成 0x802A 重试即可。
4. 我们 bind 成功后，对端会再发一个 Binding Request
   （USERNAME = `<local_ufrag>:<remote_ufrag>`，MI key 是 **local_pwd**）。
   **必须先回一个 Binding Success（0x0101）**，否则后面的握手不成立。
5. 回完对端 check 之后，这个 TCP 端口并不说普通 TLS：
   它返回一个 **0x04 前缀的自定义协议**。抓到的最长首包 3435 / 12606 字节，
   开头形如：

   ```
   04 <16B random> 00 01 11 00 <len_lo> <len_hi> ...
   ```

   该协议尚未还原，推测是中继↔被控端的原生链路，不是客户端该直接说的协议。

官方客户端真正的 PCK 承载方式是：

```
客户端 --TLS--> TURN 服务器（turns:<ip>:443）--中继--> 被控端
```

- 官方 PCK 帧是某个 `SSL_write` 上的明文，映射到的目标 IP:443 是 TURN 服务器；
  同一会话里另有一个 SSL 对象在跑 TURN 的 Send/Data Indication。
- 因此正确方向是：先对 TURN 服务器做 Allocate，再 CreatePermission 到被控端的
  relay candidate，然后把 PCK 帧作为 TURN DATA 发给被控端。
- 当前 `iceServers` 形如：

  ```json
  {"urls":"turns:112.28.240.178:443?transport=tcp",
   "username":"1792325537:aecgvuzaa4bhcnvs",
   "credential":"..."}
  ```

  用户名/密码每次 `room/join` 都会变，nonce 很短命，必须在同一次会话内立即使用。

TURN 已用 Python PoC 验证：

- TLS 1.2 到 `112.28.240.178:443` 成功（SNI=IP、无 ALPN、无客户端证书）
- 未认证 Allocate（method 0x0003 + REQUESTED-TRANSPORT=17）→ `401`，
  带 `REALM=mumu.com` + `NONCE`
- 带 USERNAME/REALM/NONCE/MESSAGE-INTEGRITY 的 Allocate → `0x0103 Success`，
  返回 XOR-RELAYED-ADDRESS 和 LIFETIME=600
- long-term key = `MD5(username:realm:password)`，MESSAGE-INTEGRITY=HMAC-SHA1
- PoC：`/tmp/turnvariants.py`（工作）；`/tmp/turntest3.py` 因 nonce/连接复用
  处理不当会 401，移植 Rust 时必须每次 401 后立刻用新 nonce 重发，不能跨连接复用。

**为什么不用 WebRTC data channel 发键盘**：data channel 确实能把 PCK 帧送到
被控端（鼠标已验证），但键盘帧被忽略。KVM 上装了低层键盘钩子
（`C:\Users\yfy\kbdhook.exe`，elevated），在 8 组 kind/ptype/key 字段变体测试中
只看到一次 `VK_F24(135)` 的按下/抬起（连接/断开产生的合成事件），没有任何
我们发送的 VK。结论：键盘必须走上面的 TURN/PCK 通道。

### 已验证的 Linux 控制器原型 ★★★

脚本：`tools/linux_controller/proto.py`（Python + aiortc，参考实现）。
实测在 ares 上收到 KVM-WIN11 的 **1920×1080 H264 画面**（`/tmp/uu_frame_0.png`）。

完整步骤：

1. `POST /api/v1/room/join/by_device/{id}` 拿 `token` + `signaling_server`。
2. WebSocket 连接：
   ```
   wss://<signaling_server>/socket.io/?EIO=4&transport=websocket&t=<unix>
   X-NRD-AUTH: <room token>
   streamer_flag: {"sdp_flags":{"gzip_sdp":true}}
   streamer_version: V4.7.3
   User-Agent: WebSocket++/0.8.2
   ```
3. Engine.IO `0{...}` → 发 `40` → 收 `40{"sid":...}`。
4. 发 `control`（Socket.IO **binary event**，1 个附件）：
   ```json
   ["control", {
     "app_control_id": "<uuid>",
     "app_data": {"_placeholder": true, "num": 0},
     "geo_exit_ip": "<public ip>",
     "streamer_data": "{\"control_id\":\"...\",\"device_capability\":{...},\"ice_id\":\"\"}"
   }]
   ```
   `app_data` 是 protobuf，**附件前必须加 1 字节 0x04**。
5. 收 ACK `43["success",{iceServers,client_id,ice_id,...}]`。
6. 收 `forward_setting`（device_capability）+ `forward_setting`（signal_app_data）。
   **必须等这两个事件之后再发 offer。**
7. 建 WebRTC：5×video(recvonly) + audio + data channel。
8. 发 offer：gzip(SDP) **加 0x04 前缀**，作为 `soac` 二进制附件：
   ```json
   ["soac", {"client_id":"<controlled client_id>","data":{
     "app_control_id":"...", "gzip_sdp":{"_placeholder":true,"num":0},
     "ice_id":"...", "ice_network_type":0, "sdp":"", "type":"offer"}}]
   ```
9. 收 answer（同样是 0x04 + gzip），解压后 `setRemoteDescription`。
10. ICE 连上后等 ~2s，发
    `42["ice_finished_log",{"app_control_id":"...","ice_id":"..."}]`。
11. 视频轨开始出帧。实测 answer 选择 H264 `profile-level-id=42001f/42e01f`，
    第一路 video 为活跃显示器，其余 4 路无媒体。

### Rust 侧控制流（已跑通）

`crates/uu-core/src/socketio.rs` 已实现 Socket.IO 客户端（Engine.IO v4、
`X-NRD-AUTH`、`45n-` 二进制事件）。`uu control <device>` 现在会：

1. 调用 `room/join` 拿 token；
2. 连 `wss://.../socket.io/?EIO=4&transport=websocket&t=<unix>`；
3. 发 `control`（带 `0x04` 前缀的 protobuf app_data）；
4. 实测收到 `success`（ICE servers/client_id/ice_id）、`forward_setting`
   （device_capability）和带二进制的 `signal_app_data`。
5. `crates/uu-core/src/media.rs`（webrtc-rs 0.11）已建 5×video + audio +
   data channel，发 offer、收 answer、加 candidate；
   实测 `peer state: connected`，并持续收到 H264 RTP。
6. RTP 已用 `H264Packet` + `SampleBuilder` 解包，接 `uu-media::H264Decoder`；
   实测解出 **1920×1080** 首帧（PPM/PNG 验证），并每 2 秒发 PLI 请求关键帧。

### Rust 侧新增模块与当前缺口

已完成并接入 controller：

- `crates/uu-core/src/stun.rs`
  - STUN Binding Request/Response，HMAC-SHA1 + FINGERPRINT
  - 对端 Binding Request 的 Success Response（用 local_pwd）
- `crates/uu-core/src/sdp.rs`
  - 从 offer/answer 解析 local/remote ufrag、pwd、passive TCP candidate
- `crates/uu-core/src/pck.rs`
  - `PckTls::ice_tcp_connect`：TCP + STUN + 回应对端 check
  - `dial_ice_tcp` / `dial_ice_tcp_raw` / `probe_after_stun`
  - `connect_stream_muxed`：TLS 握手中透明处理 STUN 帧
- `controller.rs`
  - 键盘帧优先走 PCK，失败回退 data channel
  - `UU_PCK_MODE=off|raw|probe|tls|turn|turntls`、`UU_PCK_FRAME=be16|le16`
  - `UU_FORCE_JOIN=1` 可以踢掉官方会话，便于独占被控端测试
- CLI
  - `uu kbd-test <device> --text ...`
  - `uu kbd-probe <device>`（8 组键盘编码变体）
  - `uu pck-dial --host --port --remote-ufrag --remote-pwd --local-ufrag --local-pwd`

下一步（按顺序）：

1. 对真实会话继续确认 TURN PCK 的远端路由，重点对照官方两条 TLS
   （TURN 控制连接和 PCK 承载连接）的首包顺序。
2. 用 KVM 的 `kbdhook.exe` 和记事本完成 VK 注入验收。
3. 验收后把 GUI 的键盘状态从“通道已就绪”提升为“输入已确认”，再重建 Windows 包。

## 4.5 GUI 与 Windows 构建

新增 `crates/uu-gui`（eframe/egui）：

- 左侧设备列表（状态点、别名、device_id、壁纸缩略图、搜索/刷新），
  右侧远程画面，支持多显示器下拉选择。
- 首次启动扫码登录（二维码渲染 + 轮询 + token 持久化）。
- 连接按钮启动 `uu-core::controller`，视频帧通过 channel 送到 GUI，
  中央面板实时显示。
- 鼠标移动/点击已转换为 PCK 帧，经 WebRTC data channel 发送（实测有效）。
- 键盘修饰键（Ctrl/Shift/Alt/Win）会按 `modifiers` 状态同步按下/抬起；
  断线、失焦、重连时会主动发送所有修饰键的 `kbd_release`，用于清掉远端
  “Alt 卡住”的状态。`kseq` 从 1 开始连续递增。
- 键盘帧在 controller 内优先路由到 PCK；PCK 未就绪时才回退 data channel。
  当前 PCK 还没打通，所以 GUI 键盘仍不能真正注入。

Windows 交叉构建：

```
rustup target add x86_64-pc-windows-gnu
sudo apt install mingw-w64
cargo build --release -p uu-gui --target x86_64-pc-windows-gnu
```

产物：`dist/uu-gui-win64/`（`uu-gui.exe` + `libstdc++-6.dll` +
`libgcc_s_seh-1.dll`）。已在 KVM-WIN11（Windows 11）上启动验证：
登录界面、中文字体、二维码按钮均正常。最新包也已上传到
`KVM-WIN11:C:\Users\yfy\uu-gui-test\` 和 `x358h:C:\Users\yfy\`。

**Windows 端到端已验证（2026-10-17）**：在 x358h 上运行 `uu-gui.exe`，
选择 KVM-WIN11 并连接，成功显示远端 1920×1080 桌面，状态“已连接”，
多显示器下拉、全屏、断开、鼠标均正常（首次需在 Windows 防火墙弹窗点“允许”）。
键盘输入待 PCK/TURN 通道打通后再做最终验证。

## 5. 代码结构

```
crates/uu-proto     帧格式(15 测试)、控制消息、媒体分片/重组、ID
crates/uu-media     RawFrame/EncodedFrame、NV12/BGRA→I420、OpenH264 编解码(12 测试)
crates/uu-capture   Capture trait + 合成采集源(无显示器可跑)
crates/uu-input     InputInjector + Linux uinput 实现 + Windows SendInput
crates/uu-core      Config、sign、SignalClient、UuApi、Session、self-loop、
                    socketio、media、controller、pck、stun、sdp
crates/uu-cli       probe/release-check/login/devices/api/control/self-loop/
                    pck-probe/pck-dial/kbd-test/kbd-probe
crates/uu-gui       uu-gui（eframe/egui，Windows 交叉构建）
tools/win           Frida 抓包/挂钩（wirecap、pcksniff、tlscap_all、pckmap_*）
docs/               protocol-notes.md（协议证据链）、roadmap.md
notes/              前期 Wine 调研（历史，仍有组件清单价值）
```

命令：

```
uu probe           本机能力体检
uu release-check   官方发布源探测
uu login           注册→二维码→长轮询→换 token
uu devices         设备列表
uu api <path>      带签名的手工请求（调试新接口）
uu self-loop       自有媒体管线自检（UDP + H264 + PSNR 门禁）
uu control <id>    完整会话 + 视频/鼠标调试
uu kbd-test <id>   连接后通过 controller 发送文本（键盘通道联调）
uu kbd-probe <id>  发送 8 组键盘编码变体，用于对照被控端注入
uu pck-dial ...    ICE-TCP + TLS 的 PCK 拨号探针
```

---

## 6. 环境与机器

| 机器 | 地址 | 说明 |
|---|---|---|
| **ares**（开发机） | 192.168.35.10 | Ubuntu 24.04，12 核 / 15 GB，仓库所在 |
| **x358h**（Windows 11，可随意折腾） | 192.168.35.200:2202，用户 yfy，管理员 | UU 装在 `D:\Programs\GameViewer`；被控/主控都测过 |
| **KVM-WIN11**（被控测试机） | 192.168.35.6:2202 | UU 装在 `E:\Program\Netease\GameViewer`；device_id `aeawvmepembcpca5` |
| laptop-arch | 192.168.35.100:22 | Arch x86_64，时有时无，仅作 Linux 端测试 |

### 工具链（ares）

```bash
export PATH="$HOME/.nvm/versions/node/v24.16.0/bin:$PATH"
export REA_PWNTOOLS_PYTHON=$HOME/.local/share/rea-venv/bin/python3   # pwntools+pyelftools+unicorn
export GHIDRA_INSTALL_DIR=$HOME/opt/ghidra_12.1.4_PUBLIC             # 已解压，882 MB
export REA_GHIDRA_STARTUP_TIMEOUT_MS=3600000                         # 18MB DLL 必须调大，默认 330s 会超时
```

REA：`rea-agents` v6.5.0（npm 全局）。

### x358h 上的动态分析工具

```
Python: C:\Users\yfy\AppData\Local\Programs\Python\Python311\python.exe
Frida:  17.24（已 pip 安装）
脚本:   C:\Users\yfy\{uucap*.py, scanwss.py, wssock.py, tlshook*.py, probe3.py}
```

运行 PowerShell：用 `/tmp/wps.sh`（x358h）与 `/tmp/wps2.sh`（KVM-WIN11），
内部用 `-EncodedCommand` 避免引号问题；**后台进程必须用 WMI `Win32_Process.Create`
或 `setsid`，否则 SSH 断开会被清理**。

KVM-WIN11 上用于验证输入注入的辅助工具：

```
C:\Users\yfy\kbdhook.exe    低层键盘钩子，日志到 C:\Users\yfy\kbdlog.txt
C:\Users\yfy\kbdhook.cs     源码（csc.exe 编译）
C:\Users\yfy\focus_*.ps1    把记事本调到前台
C:\Users\yfy\kvmshot.ps1    截图到 C:\Users\yfy\kvm_shot.png
```

---

## 7. Ghidra / 静态分析状态

- REA 已识别 Ghidra 12.1.4（`rea capabilities` 显示 available）
- 第一次 `rea analyze streamer.dll` **超时**（`provider_timeout`，默认启动时限 330 s）
- 已用 `REA_GHIDRA_STARTUP_TIMEOUT_MS=3600000` 重跑，输出 `/tmp/rea_s2.json`
  （预计 10–30 分钟；若仍失败，改用 Ghidra headless + 自定义 post-script 直接反编译
  `0x0BBF000`、`0x020C000`、`0x0C7DC0`）
- 当前键盘路线不依赖静态反编译：已经用 Python 把 TURN Allocate 跑通。
  只有在 TURN/PCK 仍不通时，才需要继续分析被控端 0x04 自定义协议。

---

## 8. 仍未完成 / 风险

- [x] **TURN-over-TLS 客户端**（Allocate / CreatePermission / Send / Data
      Indication）
- [x] 被控端 UDP relay/srflx candidate 的完整收集（trickle `soac candidate`）与批量 Permission
- [ ] PCK 键盘帧经 TURN 的端到端注入验证（KVM `kbdhook` + 记事本）
- [ ] GUI 最终键盘验证、重新构建并上传 Windows 包
- [ ] 音频、剪贴板、文件传输（非阻塞，后续补）
- [ ] NACK/RTX/FEC、多显示器切换稳定性
- [ ] 原生采集后端（PipeWire / X11 / WGC）仍是占位，只有 `synthetic`

风险与注意事项：

- `UU_FORCE_JOIN=1` 会踢掉 x358h 上的官方会话；测试前确认可以独占被控端。
- TURN nonce 很短命，401 后必须立即用新 nonce 重发；不要跨连接缓存。
- 被控端 direct TCP 端口在 STUN 后说的是 0x04 自定义协议，不是 TLS；
  不要再往这个端口发 ClientHello。
- KVM 上 elevated 的 WindowsTerminal 会抢焦点并可能触发 UIPI，键盘验证时
  用非 elevated 的记事本，并以 `kbdhook.exe` 的日志为准。
- x358h 的 GameViewer 被 force_join 踢掉后需要重新打开/连接，才能继续抓官方包。

## 8.5 最新验证记录（2026-10-17 晚）

| 验证项 | 结果 |
|---|---|
| 假 ICE 凭据向 `192.168.35.6:<tcp>` 发 STUN | 收到 `401 Unauthorized`，证明 2 字节长度前缀 + STUN 封装正确 |
| 用 answer SDP 的真实 ufrag/pwd 发 STUN | Bind Success；随后成功回应对端的 Binding Request |
| STUN 后直接发 TLS ClientHello | 对端回 `0x04` 自定义协议（3435/12606B），rustls 报 `InvalidContentType` |
| TURN-over-TLS Allocate（Python PoC） | `0x0103 Success`，relayed address + LIFETIME=600 |
| data channel 键盘 8 组变体 | KVM 低层钩子只记录到一次 `VK_F24(135)`，没有任何测试 VK |
| Windows GUI 视频/鼠标 | x358h → KVM-WIN11，1080p 画面、鼠标、全屏、多显示器均正常 |

当前没有保持中的测试会话；`force_join` 会踢掉官方会话，测试前再启动即可。

## 9. 合规提醒

- 不随仓库分发网易二进制（`artifacts/` 已 gitignore）
- 建议用小号测试；逆向用于互操作，但可能违反服务条款
