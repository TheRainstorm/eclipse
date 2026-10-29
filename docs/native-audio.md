# 原生系统音频

v0.4.0 起接入自有加密媒体通路与图形端播放；UU 兼容模式的音频保持独立。

Windows 使用默认输出设备的 WASAPI loopback；Linux 通过 PulseAudio 或 PipeWire Pulse 服务查找默认输出对应的 monitor。不会将默认输入设备作为备用来源。主机在“共享此设备”中勾选“共享系统声音”后开启，默认关闭。命令行真实显示器主机增加 `--audio`；合成画面只能显式使用 `--synthetic-audio`，不能借此采集系统声音。

采集统一为48 kHz立体声、10 ms Opus帧、128 kbps。Windows 接受设备提供的整数或浮点格式，处理单声道、立体声与常见多声道布局；非48 kHz使用带限 sinc 重采样，保持跨数据块的相位。捕获队列与 PCM 缓冲保持有界，复用播放侧的短淡入淡出，并在普通设备抖动时等待完整 PCM 帧。

编码器启用 Opus 带内 FEC 选项并设置损失估计，但是否携带冗余取决于实际编码模式。接收器的 `concealment_attempts` 表示 FEC/PLC 尝试，不能解释为已经证明的 FEC 恢复数量，也不代表支持视频 FEC。

## 传输、恢复与播放

音频使用独立的加密 RTP 音轨，不进入可靠有序的键鼠控制队列。双方支持时采用 [RFC 2198 RED](https://www.rfc-editor.org/rfc/rfc2198.html#section-3)，在当前 Opus 包内携带前一个包的完整编码数据；一个孤立丢包可由后续包恢复。包体保持在数据报预算内，冗余无法容纳或间隔过长时发送当前数据。代价是音频有效负载大约增加一倍，连续丢包仍需 FEC/PLC。

接收器先去重和重排；只有从冗余数据成功解码的包计入 `audio_recovered`。Opus 的 FEC/PLC 尝试另计 `audio_concealed`。源码双进程检查中，人工丢弃59包、冗余恢复59包，未触发 PLC，视频解码错误为零。它是音频冗余恢复，视频仍使用 NACK/RTX 和关键帧恢复。

声音设备不可用时保留视频，主机后台尝试重新打开输出采集；停止会话会关闭音频工作线程与设备。客户端复用本地音量、静音和 Ctrl+Alt+Shift+M，保留偏好，切换使用短渐变。旧版主机拒绝音频协商时继续视频；旧版控制端未请求音频时，主机不启动声音采集。

`native-probe` 只解码音频并记录指标，不打开输出设备。`--audio-loss-every 5` 只在该探测会话的接收端模拟每5包丢1包，不修改网络；`--report` 提供音频接收、解码、注入损失和实际恢复计数。

## 隔离验证

```bash
cargo build --release -p uu-media --example audio_capture_probe --locked
python3 tools/native/audio_smoke.py \
  --probe ./target/release/examples/audio_capture_probe \
  --output ./artifacts/native-audio-test
```

脚本创建独立的 PulseAudio 服务、私有认证 cookie 和空输出设备；默认输入设置为另一条静音源，再向输出播放生成的测试音。采集必须得到输出信号，并在模拟丢包后继续解码。结束时验证采集流释放，再关闭自己的服务与播放进程。测试不修改工作桌面的默认音频设备。

Windows 或自己的交互桌面可显式运行 `audio_capture_probe --system-output --expect-signal`，在默认输出播放声音；工具只解码并输出统计，不播放捕获内容，也不保存 PCM。`--synthetic` 使用程序生成的测试音；`--drop-every 20` 检查定期缺包的隐藏处理。

实现依据：[CPAL WASAPI loopback](https://github.com/RustAudio/cpal/blob/v0.15.3/src/host/wasapi/mod.rs)、[PulseAudio monitor 说明](https://www.freedesktop.org/wiki/Software/PulseAudio/Documentation/Developer/Clients/WritingVolumeControlUIs/)、[rubato 重采样 API](https://docs.rs/rubato/0.16.2/rubato/)。未引入参考项目的 GPL 实现代码。
