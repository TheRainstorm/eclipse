# 逆向网易 UU 远程：从「只有画面」到键鼠可用

> 记录一次在原生 Rust 重写里攻克「键盘鼠标注不进去」的过程。
> 项目不依赖 Wine，目标是 Windows + Linux 双端。鉴权、扫码登录、会话、
> WebRTC 媒体面都已跑通，最后卡在输入。

## 起点：画面通了，键鼠没反应

在 Windows 测试客户端 上用我们的 `uu-gui.exe` 连官方被控端 Windows 测试主机：能连上、能看到桌面，
但鼠标不动、键盘不响应。文档里此前的结论五花八门——「鼠标走 data channel 的
kind 15 实测有效」「键盘走 TURN-over-TLS 中继」「被控端 TCP 端口在 STUN 后说的
是 `0x04` 自定义协议」——互相矛盾，而且没有一条能端到端复现。

所以第一件事不是改代码，而是**建立一个确定性的观测通道**。

## 先修画面：一个被忽略的 SPS 标志位

顺手先把「只出首帧、卡在 6 FPS」的问题解决了，因为冻结的画面会掩盖输入是否生效。

现象：RTP 包持续到达，OpenH264 却大量报 `Native:16/18`（`dsNoParamSets` /
`dsRefLost`），解出约 30 帧后停止。

关键实验：把 SampleBuilder 吐出的 access unit 原样落盘，用 ffmpeg 解码——
**536 帧全部无损解出**。也就是说码流是好的，问题在 OpenH264 的用法。

打开 OpenH264 的调试日志，真凶出现：

```
referencing pictures lost due frame gaps exist, prev_frame_num: 7, curr_frame_num: 10
```

网易的编码器为了码率控制会**主动跳帧**（`frame_num` 递增但那一帧不发），而 SPS 里
`gaps_in_frame_num_value_allowed_flag = 0`。ffmpeg 容忍这种不规范，OpenH264 严格
照做，一次 gap 就永久失步到下一个 IDR。

修复（`crates/uu-media/src/h264.rs`）：

1. 解码前把每个 SPS 的 `gaps_in_frame_num_value_allowed_flag` 改写为 1
   （位级解析 RBSP，处理 emulation-prevention 的 `00 00 03`）。
2. `DecoderConfig` 用 `Flush::NoFlush`（默认每次解码后 flush 会砸掉参考帧列表）。
3. 打开 error concealment：`set_option(DECODER_OPTION_ERROR_CON_IDC, 1)`。

结果：30 秒解出 600+ 帧，`decode error = 0`。

## 输入：三次走错方向

### 弯路一：直连被控端 TCP 端口做 TLS

文档说键盘走被控端 TCP host candidate 的 TLS。实测 STUN 绑定成功后，那个端口
回的是 `0x04` 前缀的自定义协议，不是 TLS。

### 弯路二：TURN-over-TLS

实现了完整的 TURN 客户端（Allocate / CreatePermission / Send Indication），
甚至真拿到了 `Allocate Success` 和 relayed address，但发过去的 PCK 帧被控端
没有任何反应，也收不到回包。控制面通了，数据面没通。

### 弯路三：把输入当成 KCP 隧道

在 `streamer.dll` 里挖到 `DcKcpTransport`、`DcKcpFecGenerator`、`kcp_sn`、
`cm256` 等字符串——网易魔改了 libwebrtc，在 data channel 里跑 KCP + Reed-Solomon
FEC。当时判断「输入走 KCP，得自己实现一套」，差点开一个大坑。

### 观测手段：让被控端自己说话

因为 SSH 会话读不到交互桌面的光标（`GetCursorPos` 恒返回 `0,0`），我改了两条观测路径：

- 在 KVM 上编译一个 `WH_MOUSE_LL` 钩子 `mousehook.exe`；
- 直接用**远端视频**看光标/界面变化（视频里确实有光标，而且时钟每秒在跳，
  证明画面是活的）。

判据也换成了确定性的：**点右下角开始按钮，看 Win11 开始菜单是否弹出**。
（顺带一提，`kbdhook.exe` 的低层键盘钩子一直可用，它能看到系统注入的 `VK_F24`
合成事件，是验证键盘的最佳标尺。）

## 破局：一份能互操作的开源实现

真正的转折来自横向搜索：`VallierDev/uu-remote-linux` 是一个纯 Rust 写的
**Linux 被控端**，能和官方 Mac/Windows 客户端互通。它既然能解析官方主控端发来的
输入，就一定有完整的线格式定义。

读它的 `src/viewer/wire.rs` 和 `src/control_protocol.rs`，答案清晰得让人拍腿：

1. **数据通道标签**必须是官方规定的 5 个名字：

   ```rust
   ["CONTROL_DATA_CHANNEL", "TEXT_DATA_CHANNEL", "STREAMER_DATA_CHANNEL",
    "FILE_DATA_CHANNEL", "BINARY_DATA_CHANNEL"]
   ```

   我们一直用 `webrtc-datachannel`——被控端当然不理会。

2. 通道打开后要先做 **SimpleAction echo 握手**。被控端会不停重发一个 36 字节的
   protobuf echo 请求，形状是：

   ```
   Message { 1: seq, 3: SimpleAction { 4: features } }   // action 缺省 = 0
   ```

   主控端必须回：

   ```
   Message { 3: SimpleAction { 1: 1, 2: {"seq": <请求seq>}, 4: features } }
   ```

   握手完成前，被控端丢弃一切输入。

3. **输入就是 CONTROL 通道上的裸 JSON**，没有 28 字节 PCK 头：

   ```json
   {"action":"mouse_move_absolute","abs_x":0.003,"abs_y":0.998}
   {"action":"mouse_press","button":1}
   {"action":"mouse_scroll","delta_x":0,"delta_y":120}
   {"action":"kbd_press","key":90}
   ```

## 落地

新增 `crates/uu-core/src/control_wire.rs`：一小段免依赖的 protobuf
varint 编解码 + echo 请求/响应 + 检测（还带了 3 个单测）。

`media.rs` 里创建 5 个数据通道，在 CONTROL 上：打开即发我们的 echo_request，
收到被控端的 echo_request 就回 echo_response。`controller.rs` 把 GUI/CLI 产出的
PCK 帧剥掉头尾，改成把 JSON 直接 `.send()` 出去。

验证那一刻：

```
# KVM 低层键盘钩子
22:58:48.010 vk=90 scan=44 flags=0   msg=256   # Z 按下
22:58:48.090 vk=90 scan=44 flags=128 msg=257   # Z 抬起
```

以及点开始按钮后，远端视频里 Win11 开始菜单直接弹了出来。

## 几个教训

1. **先建立可判真假的现象**。没有「点开始菜单」这种确定性判据，会一直靠猜。
2. **字符串只是线索，不是结论**。看到 KCP/FEC 就以为要实现 KCP，是过度外推；
   真正跑起来才发现输入根本不在那条路径上。
3. **互操作项目是金矿**。一个能和官方互通的开源实现，其协议定义胜过几小时
   静态分析。
4. **先自证，再互通**：自有的 self-loop 管线（采集→H264→UDP→解码→PSNR）一直
   是回归基线；这次 SPS/解码的修复也是先在它和 ffmpeg 对照下定位的。

至此，这个原生客户端已经能：连接官方被控端 → 连续看 1080p 画面 → 用鼠标键盘
操作。剩下的是音频、剪贴板、文件传输，以及原生采集后端。
