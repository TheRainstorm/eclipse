# 性能、延迟与测量

[文档总索引](../README.md)

统计口径、可复现实验、已经测量的结果和仍待验证的优化。阅读数据时保留各文章的硬件、采样窗口与时钟边界。

## 先理解指标与适用范围

- [原生串流延迟统计](native-latency.md)
- [延迟优化做了什么，哪些设备受益](optimization-scope.md)
- [Windows 视频延迟与计时范围](windows-latency.md)
- [从“三帧延迟”到可解释的时间线：一次桌面串流 profiling](streaming-latency-profiling.md)

## 测量工具与时间线

- [主机逐帧测量](frame-tracing.md)
- [Intel AV1 编码与采集快照的离屏对照](../architecture/native-encoding.md#测量采集快照复制的影响)
- [同帧原生 Present 诊断](present-observation-profiling.md)
- [自动化实机延迟实验](automated-latency-lab.md)
- [输入时间线与原生重复呈现](input-timeline-and-native-presents.md)
- [R0 主机测量首批验证](r0-host-measurement.md)
- [混合 DPI 导致外屏 Render 升高：定位、修复与实测](mixed-dpi-render-latency.md)

## 优化、实验与研究

- [CPU 颜色转换优化与验证](conversion-performance.md)
- [Windows capture/encode overlap experiment](capture-overlap-experiment.md)
- [帧率协商与原生屏幕](frame-rate.md)
- [Eclipse 与 Sunshine + Moonlight：延迟、稳定性对比与重构方案](streaming-latency-and-stability-plan.md)
- [电脑交互式串流低延迟技术与文献综述](电脑交互式串流低延迟技术与文献综述.md)
