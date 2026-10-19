# 原生系统音频（开发中）

v0.3.0 发布包尚无原生音频。当前源码已加入采集和编码基础，后续接入自有加密媒体通路；UU 兼容模式的音频保持独立。

Windows 使用默认输出设备的 WASAPI loopback；Linux 通过 PulseAudio 或 PipeWire Pulse 服务查找默认输出对应的 monitor。不会将默认输入设备作为备用来源。图形主机将以单独选项明确开启共享声音。

采集统一为48 kHz立体声、10 ms Opus帧、128 kbps。Windows 接受设备提供的整数或浮点格式，处理单声道、立体声与常见多声道布局；非48 kHz使用带限 sinc 重采样，保持跨数据块的相位。捕获队列与 PCM 缓冲保持有界，复用播放侧的短淡入淡出，并在普通设备抖动时等待完整 PCM 帧。

编码器启用 Opus 带内 FEC 选项并设置损失估计，但是否携带冗余取决于实际编码模式。接收器的 `concealment_attempts` 表示 FEC/PLC 尝试，不能解释为已经证明的 FEC 恢复数量，也不代表支持视频 FEC。

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
