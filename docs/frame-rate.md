# 帧率协商与原生屏幕

更新：2026-10-19。此前实现按用户要求移除全部超级屏操作、合并官方样式画质菜单，并恢复原有连接初始化方式。保留60/120/自定义帧率请求和只读屏幕信息，实际高帧率与输入问题由用户验收。

后续用户证实ICE/DTLS connected而SCTP connecting，初始化回退未解决。此前实现已在DTLS依赖中复现并修复握手结束后的接收死锁，用户确认补丁版`f74fc9d`恢复键鼠，见[DTLS 补丁说明](../vendor/webrtc-dtls/UU-PATCH.md)。本次输入恢复不等于实际60/120帧率已验收。

## 为什么此前只有30fps

不能仅凭接收端代码认定唯一原因。用户已确认同一Windows 测试主机在官方客户端超级屏模式可到60，因此旧测试约30fps不代表官方host的统一上限。本轮确认旧客户端存在这些问题：

- 此前实现曾加入初始CaptureParams；用户随后报告视频正常但CONTROL 45秒未打开。因该初始化路径未经实机验证，此前实现已回退，帧率继续由CONTROL就绪后的CaptureSettingRequest设置。不能把未传初始CaptureParams直接认定为30帧原因。
- DecoderCap和streamer display_info固定60，无法完整表达120请求；GUI调整码率时又硬写60。
- 帧率有旧枚举与实际整数两个字段，120没有对应的旧枚举；不能把120当枚举发送。
- 早期Exact分辨率固定screen_id=1，后来改成-2。此前实现核对官方被控端后更正：-2在多轨道路径是广播现有轨道，不是通用“保持当前屏幕”。现在采集设置绑定实际选中屏幕ID。
- Windows硬解输出类型固定30fps，每帧写33.33ms时长。这是错误的格式/时长描述，不是已证实的网络收帧限速器；已取消，保留真实PTS和既有重协商。

接收/解码/新帧唤醒没有30fps定时丢帧限制。静态桌面、远端采集、编码、网络恢复以及本地呈现仍可能使实际帧率低于目标。

## 超级屏是否必须

WebRTC/H.264本身不要求超级屏。物理屏采集能否提供60/120由显示源、采集实现和host策略决定，修改请求不能强迫官方host提供额外独立画面。

[网易官方App Store说明](https://apps.apple.com/cn/app/uu%E8%BF%9C%E7%A8%8B-%E8%BF%9C%E7%A8%8B%E5%8A%9E%E5%85%AC-%E6%B8%B8%E6%88%8F%E4%B8%B2%E6%B5%81/id1642306791?platform=ipad)将超级屏描述为突破物理屏分辨率与帧率限制的功能。安装/启用驱动不等于当前串流已使用该采集源；进入/退出是独立RPC，不是capture_type或帧率枚举。

此前实现按用户的新需求增加两种显示器模式：默认保持现有布局；用户选择UU虚拟屏独占后，在下次连接明确请求官方手动超级屏，退出时仅为本次成功申请的独占请求恢复。普通切屏只选择现有源与轨道，不进入独占模式。屏幕名称、主屏标记、video_track_index和真实screen_id来自远端清单。

## 当前用法

1. 设置菜单 → 画质。上半部为自动原画、原画30M、超清14M、高清8M与自定义；明确码率通过quality=6及真实bps发送，标签不等于实测速率。
2. 同一面板下半部直接选择144/120/90/60/30帧，自定义帧率支持5–144。分辨率和自定义参数侧开子面板，确认后返回主菜单；没有绿色常驻确认文字。
3. Ctrl+Alt+S打开HUD，区分目标FPS、远端屏幕Hz、实际Rx接收/De解码/Rd新画面提交。请求确认不等于实际达标。
4. 顶部出现“输入连接中”表示CONTROL尚未打开，即便视频已更新也不能操作。45秒超时会显示ICE/DTLS/SCTP/CONTROL状态。悬停HUD可查看本地输入发送/失败计数；这不代表远端已注入。
5. 用持续动态内容分别验证60、120、自定义75。若Rx仍30，先看远端实际采集能力；若Rx60/120而De或Rd明显落后，则检查解码/呈现。

CLI或进程环境可用UU_FPS=120指定初始值，默认60；默认保留远端分辨率，不用分辨率请求隐式修改布局。旧UU_FPS=1/2/3/4仍映射30/60/90/144；新GUI/API使用实际整数。质量、分辨率、码率变更保留所选帧率。144只是当前请求范围上限，不表示所有机器或4K分辨率均能达到144。

## 官方字段依据与参考入口

本地官方Windows客户端`artifacts/win-client/bin/GameViewer.exe`的内嵌protobuf描述符，SHA256：`0f8169fa326cfd0b7bd85767ddf2fef683f372711deb29e9f211c096e76e0125`。以下是本项目整理的字段语义，不提交官方二进制或完整提取物。

| 项目 | 官方字段 | 本项目入口 |
|---|---|---|
| 旧帧率枚举 | 0 UNKNOWN、1=30、2=60、3=90、4=144；无120枚举 | `control_wire::CaptureFrameRate`，自定义/120枚举0并传实际count |
| 连接能力 | ConnectOptions.4 DecoderCap：1上限；初始CaptureParams当前不发送 | `socketio::encode_app_data`，CONTROL建立后再请求实际fpsCount |
| 运行时采集设置 | Message21/RpcRequest2，CaptureSettingRequest：1枚举、18 fpsCount、4 screen_id、10分辨率类型 | `control_wire::capture_setting_request_at_fps` |
| 分辨率类型 | 1保持、2跟随本地、3跟随远端、4明确尺寸 | Exact绑定选中screen_id，发送类型4及物理宽高；默认类型1保持远端分辨率 |
| 当前远端屏幕 | Message7 ScreenSources：screens1、current_screen_id2；Screen.id1、fps2、current_resolution4、screen_type5、dpi9 | `screen_control::screen_sources`，按当前ID找屏幕，不取第一块物理屏 |

描述符起始偏移：main.proto 63307440、defines.proto 63309920、connect.proto 63319360、rpc_main.proto 63334800、general.proto 63342192、rpc_message.proto 63355904。私有解析结果保留在artifacts，后续官方更新要先比较新描述符，不能依赖这些固定偏移。

uu-remote-linux的`src/capture_setting.rs`也区分枚举与fpsCount，可参考其同协议host处理；但其旧字段和官方Windows当前描述符不完全一致，不整体搬用。Moonlight/Sunshine的FPS配置需要发送端采集/编码配合，同样不能把目标值当成实测；本项目的普通切屏和可选虚拟屏独占已分离，见此前实现。

硬解改动依据：[微软H.264 Decoder](https://learn.microsoft.com/en-us/windows/win32/medfound/h-264-video-decoder)允许省略未知输入帧率并在获得码流后报告格式改变；[Chromium MF输入样本处理](https://chromium.googlesource.com/chromium/chromium/+/d3f4c0027364e81fad704fbca8cce4940140a9ac/media/mf/h264mft.cc)仅在时长已知时设置压缩样本时长。没有从这些项目导入实现代码。

## 验证与边界

协议针对性检查覆盖旧60帧抓包字节兼容、120/自定义count、初始化能力、保留当前屏幕、元数据选择和原生屏幕ID映射。本地迟到ICE回环验证设置、输入心跳、鼠标和键盘一起传送；GUI14项已有检查和Windows交叉编译通过。

CONTROL超时已通过此前实现DTLS接收竞态补丁解决，用户确认键鼠恢复；初始化回退本身未解决问题。真实60/120吞吐尚未实测。没有自动连接、操作用户GUI、自动重连或伪造画面帧率。当前CPU RGBA转换/上传、VSync及丢包恢复仍会影响高帧率；GPU直接呈现与完整frame pacing依然未完成。
