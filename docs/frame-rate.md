# 帧率协商与超级屏

更新：2026-10-19。已实现请求60/120/自定义帧率及显式超级屏切换；本轮没有自动连接、启动GUI或控制用户设备，实际高帧率由用户验收。

## 为什么此前只有30fps

不能仅凭接收端代码认定唯一原因。用户已确认同一KVM-WIN11在官方客户端超级屏模式可到60，因此旧测试约30fps不代表官方host的统一上限。本轮确认旧客户端存在这些问题：

- 初始ConnectOptions没有CaptureParams，只有后续设置；依赖host默认采集参数。
- DecoderCap和streamer display_info固定60，无法完整表达120请求；GUI调整码率时又硬写60。
- 帧率有旧枚举与实际整数两个字段，120没有对应的旧枚举；不能把120当枚举发送。
- Exact分辨率把screen_id填为1，会选择物理屏1；应为-2保留当前采集源，包括超级屏。
- Windows硬解输出类型固定30fps，每帧写33.33ms时长。这是错误的格式/时长描述，不是已证实的网络收帧限速器；已取消，保留真实PTS和既有重协商。

接收/解码/新帧唤醒没有30fps定时丢帧限制。静态桌面、远端采集、编码、网络恢复以及本地呈现仍可能使实际帧率低于目标。

## 超级屏是否必须

WebRTC/H.264本身不要求超级屏。物理屏采集能否提供60/120由显示源、采集实现和host策略决定，修改请求不能强迫官方host提供额外独立画面。

[网易官方App Store说明](https://apps.apple.com/cn/app/uu%E8%BF%9C%E7%A8%8B-%E8%BF%9C%E7%A8%8B%E5%8A%9E%E5%85%AC-%E6%B8%B8%E6%88%8F%E4%B8%B2%E6%B5%81/id1642306791?platform=ipad)将超级屏描述为突破物理屏分辨率与帧率限制的功能。安装/启用驱动不等于当前串流已使用该采集源；进入/退出是独立RPC，不是capture_type或帧率枚举。

本客户端不会因选择高帧率而自动改变远端屏幕。用户可在帧率菜单明确请求进入/退出超级屏；成功响应只说明host接受请求，实际屏幕和帧率仍看后续元数据/视频。

## 当前用法

1. 控制中心 → 帧率，选择30/60/90/120/144，或填写5–144整数并确认；菜单确认后返回主选项。
2. 同一页显示当前远端屏幕类型/上报Hz。需要时点击请求切换超级屏，分辨率使用当前选择，跟随远端时优先当前屏幕物理尺寸；未知时等待视频尺寸，不虚构1080p。
3. Ctrl+Alt+S打开HUD，区分目标FPS、远端屏幕Hz、实际Rx接收/De解码/Rd新画面提交。请求确认不等于实际达标。
4. 用持续动态内容分别验证60、120、自定义75。若Rx仍30且屏幕是物理屏，先看屏幕切换结果；若Rx60/120而De或Rd明显落后，则检查解码/呈现而非继续提高请求。

CLI或进程环境可用UU_FPS=120指定初始值，默认60。旧UU_FPS=1/2/3/4仍映射30/60/90/144；新GUI/API使用实际整数。质量、分辨率、码率变更保留所选帧率。144只是当前请求范围上限，不表示所有机器或4K分辨率均能达到144。

## 官方字段依据与参考入口

本地官方Windows客户端`artifacts/win-client/bin/GameViewer.exe`的内嵌protobuf描述符，SHA256：`0f8169fa326cfd0b7bd85767ddf2fef683f372711deb29e9f211c096e76e0125`。以下是本项目整理的字段语义，不提交官方二进制或完整提取物。

| 项目 | 官方字段 | 本项目入口 |
|---|---|---|
| 旧帧率枚举 | 0 UNKNOWN、1=30、2=60、3=90、4=144；无120枚举 | `control_wire::CaptureFrameRate`，自定义/120枚举0并传实际count |
| 初始采集请求 | ConnectOptions.3 CaptureParams：1枚举、11 fpsCount；DecoderCap.1上限 | `socketio::encode_app_data_at_fps` |
| 运行时采集设置 | Message21/RpcRequest2，CaptureSettingRequest：1枚举、18 fpsCount、4 screen_id、10分辨率类型 | `control_wire::capture_setting_request_at_fps` |
| 分辨率类型 | 1保持、2跟随本地、3跟随远端、4明确尺寸 | Exact保留screen_id=-2，发送类型4及物理宽高 |
| 当前远端屏幕 | Message7 ScreenSources：screens1、current_screen_id2；Screen.id1、fps2、current_resolution4、screen_type5、dpi9 | `screen_control::screen_sources`，按当前ID找屏幕，不取第一块物理屏 |
| 进入超级屏 | Message21/RpcRequest26：reason1=MANUAL、resolution2、dpi_scale3、fps消息4内fps_count1 | `screen_control::super_screen_request` |
| 进入响应/退出 | Message22/RpcResponse22；退出Request14、Response10；错误码字段1，0成功 | `screen_control::super_screen_response`，请求ID与模式都匹配 |

描述符起始偏移：main.proto 63307440、defines.proto 63309920、connect.proto 63319360、rpc_main.proto 63334800、general.proto 63342192、rpc_message.proto 63355904。私有解析结果保留在artifacts，后续官方更新要先比较新描述符，不能依赖这些固定偏移。

uu-remote-linux的`src/capture_setting.rs`也区分枚举与fpsCount，可参考其同协议host处理；但其旧字段和官方Windows当前描述符不完全一致，不整体搬用。Moonlight/Sunshine的FPS配置需要发送端采集/编码配合，同样不能把目标值当成实测；本项目的虚拟屏切换必须适配UU自己的RPC。

硬解改动依据：[微软H.264 Decoder](https://learn.microsoft.com/en-us/windows/win32/medfound/h-264-video-decoder)允许省略未知输入帧率并在获得码流后报告格式改变；[Chromium MF输入样本处理](https://chromium.googlesource.com/chromium/chromium/+/d3f4c0027364e81fad704fbca8cce4940140a9ac/media/mf/h264mft.cc)仅在时长已知时设置压缩样本时长。没有从这些项目导入实现代码。

## 验证与边界

协议10项检查覆盖旧60帧抓包字节兼容、120/自定义count、初始化能力、保留当前屏幕、元数据选择、超级屏RPC和响应。GUI14项已有输入/多窗口/显示器检查及Windows GUI/media测试代码交叉编译通过。

本轮未实测超级屏切换响应、切屏是否换视频轨、切换后是否需重新连接，以及真实60/120吞吐。没有实现未经验证的自动重连或伪造画面帧率。当前CPU RGBA转换/上传、VSync及丢包恢复仍会影响高帧率；GPU直接呈现与完整frame pacing依然未完成。
