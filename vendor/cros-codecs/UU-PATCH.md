# cros-codecs 0.0.6 局部补丁

来源：crates.io cros-codecs 0.0.6（ChromiumOS，BSD-3-Clause；原LICENSE保留）。

H264新增finish_access_unit：严格RTP重组完成一个AU后提交当前picture，不flush、不清除DPB、不重置解码状态。SPS明确max_num_reorder_frames=0且非隔行时，显示已完成picture并保留独立reference句柄；其它流继续原DPB重排。避免实时流等待下一帧才提交和等DPB满才输出。仅本项目VAAPI在完整AU结束调用。

B帧实测补充：Dpb::clear原来丢失max_num_reorder_frames，修正为和max_num_pics/interlaced一起保留。否则IDR后会错误进入无重排路径。回归检查参考面在显示后仍存活，B帧和隔行不走立即显示。

原生H.264编码补充：LowDelayH264把显式FrameMetadata.force_keyframe作为IDR请求，重置序列帧编号并重发SPS/PPS。上游默认只在计数器归零时标记IDR，强制请求产生的普通I帧不能满足丢包后从全新解码器恢复的要求。修改只影响H.264编码预测器，不改变其它编码格式或解码路径；由合成画面的VA-API编码/全新软件解码器探针验证。

原生编码互通补充：Baseline 低延迟预测器写明 Constrained Baseline 的约束标志；其单参考 I/P 结构在 VUI 中声明 max_num_reorder_frames=0 与对应参考缓冲上限。SPS builder 增加所需字段，不修改解码器对不支持 profile 的拒绝规则。约束组合见 RFC 6184 表5；使用实际双硬件持续解码及冷启动 IDR 探针验证。
