# 交接24：Linux硬解延迟、色彩与丢包恢复

2026-10-19 用户继续授权打磨至23:10。机器、账号配置、测试隔离规则沿用交接23；x358h保持睡眠。

## 正式入口

Linux正式包升级为 **217e5400e1770e684969721be20dbd2b9816360e**。

- laptop-arch应用菜单 **UU Remote Rust**，或 `~/.local/bin/uu-remote-rust`。
- `~/.local/opt/uu-remote-rust/current`指向该版本，旧版本保留；包在本地与远端私有测试目录的 `uu-remote-linux-x86_64-217e5400e177.tar.gz`。
- Windows D盘正式包本轮未更新，仍不能声称已同步睡眠的x358h。

## 已完成

1. **硬解减少内部等待**（28d0861）：cros-codecs 0.0.6收到完整AU后仍等下一AU才提交picture，且无重排流等DPB再输出。增加完整AU提交，不flush、不重置引用；SPS无重排且非隔行时立即输出，独立reference句柄仍保留。固定BSD源码于vendor并保留许可证。120 AU的1080p60样本从AU2首出/118帧变为AU0首出/120帧，无错误和软解回退。该结果是内部排队证据，不是测得端到端延迟减少33ms。
2. **B帧边界修正**：实测发现Dpb::clear还会丢max_num_reorder_frames，导致IDR后误入无重排路径，修正clear保留限制。B帧90 AU样本原版85帧/修改后86帧，无错误且保持VAAPI；仍有尾帧需要flush，未宣称所有B帧零等待。增加保留参考面、B帧/隔行不提前输出、IDR保留限制的回归。
3. **持续统计与故障验收**（59ca25c）：每5秒记录有界video_stats元数据，含后端、Rx/De/Rd、解码/呈现耗时、解码增长、坏帧、恢复、丢包注入、输入错误，不含视频或键盘内容。套件在显式故障模式要求真的丢包、后续解码增长、不等待IDR、硬解未回退和解码错误0。
4. **黑位与颜色修正**（217e540）：实取KVM-WIN11 SPS为video_full_range_flag=0、matrix_coefficients=1（有限范围BT709）、max_num_reorder_frames=0，旧转换固定全范围BT601。Linux带VAAPI构建读取VUI并按流保留/更新范围及矩阵，I420/NV12硬解与软解回退共用标量/SSE4.1转换；CLI首帧输出同样读色彩参数。支持Full/Limited 601/709，旧默认接口保持原行为。Windows暂未接入VUI解析，不能宣称Windows颜色已修复。

## 验证

- 媒体47项测试通过；Windows交叉检查通过（有既有字体unused_mut及上游cros警告）。向量尾部、pitch、I420/NV12和有限范围黑/白/709红色均覆盖。
- 修复前硬解与软解RGB逐字节一致，但有限范围测试图对FFmpeg参考PSNR只有24.598dB，平均绝对差14.784。修复色彩后PSNR **51.011dB**，平均绝对差 **0.403**；同一源、同一首帧、FFmpeg neighbor参考，不能混同有损编码PSNR。
- 第一轮150ms周期性丢包+相对输入实机测试：实际注入2262包，后续decoded2388→2677，decode errors=0/input errors=0/waiting_idr=0，保持VAAPI，4次切屏44/161/44/99ms，exit0/graceful_close=true。人为丢包产生的damaged计数是预期，不能当未丢过包；结束loss_ppm=0也只是当时窗口。
- 证据集中 `artifacts/linux-polish/`：低延迟样本、B帧前后、媒体回归、Windows检查、官方SPS trace、故障恢复及发布清单。含私有视频和凭据的目录继续忽略，不入库。

## 最终发布包复核

217e540正式安装包完成完整Wayland套件（含相对模式、剪贴板、键盘、150ms周期丢包、25秒持续运行），exit0/graceful_close=true，切屏60/178/43/150ms；实际注入1840包后decoded1559→1837、VAAPI保持，末段58.82解码/58.23渲染FPS，decode4.558ms、errors0、input_errors0、waiting_idr0。安装二进制哈希与清单一致，临时凭据已删除。最后复核两个原生60Hz屏幕、Zako主屏均保持，本轮测试表单关闭及两项计划任务已清理。详见artifacts/linux-polish/final-live.log。

## 继续优化

- VUI色彩解析目前仅Linux+vaapi构建可用；后续可抽出与硬解无关的通用SPS元数据解析供Windows及其它Linux构建共享，并补BT2020/HDR能力协商。
- 解码仍通过VAImage回读、CPU颜色转换和纹理上传，没有零拷贝。可继续参考Moonlight VAAPI/EGL路径。
- 故障恢复短测通过不等于跨网络长期稳定；工作电脑→x358h仍需其醒来后再测。
