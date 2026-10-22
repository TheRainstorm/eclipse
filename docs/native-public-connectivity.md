# 原生公网直连与可替换 STUN 服务

当前方向是使用公开允许第三方使用的标准 STUN 服务，或自己 VPS 上的服务，实现原生两端的公网直连。新增 UU 云接入及复用其私有中继的计划已经取消。本文是替代方案，当前原生产品仍为局域网模式，尚未实现这里的公网信令与 STUN 配置入口。

## 推荐架构

```mermaid
flowchart TB
    H[原生主机] <-->|认证后交换 SDP 和 ICE 候选| S[自建信令服务 WSS]
    C[原生控制端] <-->|认证后交换 SDP 和 ICE 候选| S
    H -.->|发现公网映射地址| ST[可替换 STUN 服务]
    C -.->|发现公网映射地址| ST
    H <==>|直连的加密视频 声音与输入| C
```

STUN 与信令是不同职责，可以放在同一台 VPS 上，也可以分别使用自建信令和公开 STUN。STUN 只帮助发现地址和执行连接检查，不转发持续视频。信令负责找到获授权的对端、交换 SDP 和候选；连接建立后媒体直接在两端之间传输。WebRTC 没有规定信令必须使用某一家服务。[WebRTC 建连文档](https://webrtc.org/getting-started/peer-connections)

因此，**换 STUN 服务不需要更改视频或输入协议**。设计上应允许配置一个或多个 STUN URL，将服务与设备身份、配对和媒体组件分开。若不希望使用 UU 服务，该原生公网路径可以完全不依赖 UU 账号、设备上线或房间 API。

## 可替换连接组件应该分成两类

用户可以将这一层理解为负责建连与可选转发的中间件，但实现时应分别配置：

- **信令适配器**：处理设备寻址、身份认证、SDP/候选投递及会话取消。不同服务的消息和登录方式可能不同，需要匹配的协议适配器。
- **ICE 服务列表**：配置标准 STUN/TURN URL 和需要的中继凭据，交给同一套 WebRTC 处理。它可以与信令使用完全不同的服务商。

UU 等服务可能将两者打包下发，但不应因此把软件的通用网络接口绑定到某一家。建议界面分别提供“信令协议与服务器”“STUN 列表”“可选 TURN 及凭据”，支持只直连和允许自有中继的明确策略。这些配置入口目前尚未实现。

### 哪些有标准协议

| 部分 | 标准化范围 | 是否只改服务器地址就能替换 |
|---|---|---|
| STUN/TURN | 地址发现、中继分配及数据传输协议 | 通常可以，仍需匹配传输方式、认证与服务政策 |
| SDP/ICE | 会话描述、候选和连通性选择机制 | 描述和检测可复用，但不包含设备账号及房间管理 |
| WebSocket/WSS | 消息承载与连接传输 | 不能据此推断上面的业务消息相同 |
| WebRTC 信令业务 | WebRTC 没有强制统一的房间、配对和消息 API | 不可以；PeerJS、UU 或自有服务各需适配 |

WebRTC 明确将信令交给应用决定，因此不存在一个保证所有 WebRTC 应用“只填 URL 就互通”的通用信令服务器。[WebRTC 对信令的说明](https://webrtc.org/getting-started/peer-connections#signaling)

### VPS 应部署什么软件

| 软件组合 | 用途与适用边界 |
|---|---|
| 配套的轻量原生信令服务＋coturn | 推荐的产品方向：项目提供匹配原生设备身份和会话协议的信令程序及部署包，用户部署后填写地址；该配套信令服务尚未实现 |
| PeerServer＋coturn | 已有开源选项：PeerServer 为 PeerJS 处理会话元数据和候选信令，媒体不由它代理；本项目仍需实现 PeerJS 信令适配及原生身份绑定 |
| 只有 coturn | 提供标准 STUN/TURN，不提供本项目需要的设备配对、房间与 SDP 投递业务 |

PeerServer 可以独立运行或使用 Docker 部署，但它不是现有原生客户端已经支持的入口。选现成软件仍有客户端适配工作，不能把“已经装好服务”当作两端已完成互通。[PeerServer](https://github.com/peers/peerjs-server)、[PeerJS 连接说明](https://peerjs.com/client/getting-started)

## UU 作为可选适配的边界

保留一个供应商适配器的位置在技术上可行，但 UU 信令需要账号、设备和房间流程，不是把官方 WSS 地址填入自建协议客户端。只研究其备用适配，不代表恢复已经取消的 UU 云接入实现；当前没有新增该入口。

从现有兼容代码能够明确区分：

| 服务 | 当前已知的寻址方式 | 端口含义 |
|---|---|---|
| UU 业务 API | 项目配置为 `https://api.nrd.nie.163.com` | 未显式指定时为 HTTPS/TCP 443；它不是实际 Socket.IO 节点地址的固定别名 |
| UU 实际信令节点 | 加入房间响应的 `signaling_server`，随后访问其 `/socket.io/?EIO=4&transport=websocket` | 必须采用返回 URL 的端口；WSS 未显式指定端口时才使用 TCP 443 |
| UU STUN/TURN | 具体会话事件下发的 `iceServers[].urls`，中继还可能附带凭据 | 以实际 URL 为准，不能从标准常用端口推断其部署端口 |

源码入口：[业务 API 域名](../crates/uu-proto/src/lib.rs)、[加入房间与实际信令 URL](../crates/uu-core/src/controller.rs)、[ICE 配置来源](../crates/uu-core/src/media.rs)。地址样本不能证明服务对任意第三方公开，也不能当成长期固定配置；已有日志里的动态 IP 与会话资料不作为公开脚本默认值。

uu-remote-linux 在没有云端 ICE 配置时列出的 Google、小米等 STUN 是它的备用服务，并不是 UU 的 STUN 域名。不要将参考项目的默认服务列表误认为官方下发列表。[上游会话 ICE 实现](https://github.com/VallierDev/uu-remote-linux/blob/50ed54d77a9084d18797061650bed9f85293c421/src/session_ice.rs)

## 公开服务与自己的 VPS

Cloudflare 的官方文档明确介绍了其公开 STUN 地址，可作为可配置选项：

```text
stun:stun.cloudflare.com:3478
```

它提供 UDP STUN 服务。这里引用的是公开 STUN，不是将其 TURN 或 SFU 当作免费中继；部署时仍需按服务当时的使用条件和可达性选择。[公开 STUN 说明](https://developers.cloudflare.com/realtime/sfu/get-started/connection-patterns/#network-connectivity)、[地址与端口](https://developers.cloudflare.com/realtime/turn/#service-address-and-ports)

自己的 VPS 可用开源 coturn 提供 STUN。coturn 同时实现 STUN 和 TURN，但可以设置 `stun-only`，只响应 STUN，关闭 TURN 转发功能。应用端随后使用自己的标准地址，例如：

```text
stun:stun.example.com:3478
```

`example.com` 是示例域名，需要替换为自己的地址。VPS 的 STUN 监听端口需要可从目标网络访问；仅提供 STUN 不需要为媒体建立 TURN 分配。具体安装与运行选项见 [coturn 官方说明](https://github.com/coturn/coturn/blob/master/README.turnserver)。

推荐保留服务列表和明确的失败提示，某个 STUN 不可用时尝试其它配置。不同 STUN 可能得到不同候选，并不保证增加服务数量就一定能打通某种 NAT。能直接使用的公网 IPv6 等地址也应参与 ICE 检查，不应强制每次依赖 STUN 才允许直连。

## 只用 STUN 的边界

STUN 返回公网映射地址，不代表远端一定能向该地址发送成功。某些映射与过滤行为、双重 NAT、UDP 限制或防火墙规则会使直连失败。**不配置 TURN 时，应接受“有些网络无法连接”这个结果，不能承诺任意两台设备都能公网互通。** STUN 本身不是完整的 NAT 穿透方案。[STUN 规范](https://www.rfc-editor.org/rfc/rfc8489.html)

如果以后需要提高成功率，可以独立增加自己 VPS 上、经过认证的 TURN 作为可选回退。选择中继后视频才会经过 VPS 并消耗相应转发带宽；这与 VPS 只运行信令和 STUN 的资源需求不同。当前用户选择先讨论直连方案，不自动启用或部署 TURN。

## 本项目需要改什么

| 部分 | 改动方向 |
|---|---|
| 信令 | 新增自托管、经认证的公网信令服务，两端主动连出 WSS，交换会话描述与候选 |
| 网络设置 | 将 STUN URL 列表与信令地址作为独立配置，可使用公开服务或自建地址，不绑定供应商 |
| ICE | 公网模式允许 host、srflx 及必要的对等反射候选，执行实际连通性检查；直连模式不配置 relay 服务 |
| 身份与权限 | 保留原生可信设备和授权语义，绑定会话与 DTLS 身份，获得许可后才采集和注入输入 |
| LAN 入口 | 保留现有局域网发现与严格候选校验；公网模式采用自己的连接规则 |
| 诊断 | 展示实际选中路径、传输类型及失败阶段，不能把获得 srflx 地址当作直连已成功 |

当前原生信令要求直接连接主机 TLS/TCP 地址，ICE 也只接受与信令对端地址一致的 host 候选，所以不能只在设置里加一条 STUN URL 就声称完成公网连接。需要把公网信令、候选策略、身份绑定和会话关闭一起实现，继续复用已有编解码和呈现组件。

若后续开始实现，应先用生成画面验证自建信令、同机及跨 NAT 直连、STUN 不可达和连接超时，再接正式界面与桌面控制。成功标准是实际选中无 relay 的候选对并持续传输加密媒体；不以“已连接 STUN”代替整链路验收。
