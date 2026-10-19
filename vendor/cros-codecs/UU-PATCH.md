# cros-codecs 0.0.6 局部补丁

来源：crates.io cros-codecs 0.0.6（ChromiumOS，BSD-3-Clause；原LICENSE保留）。

H264新增finish_access_unit：严格RTP重组完成一个AU后提交当前picture，不flush、不清除DPB、不重置解码状态。SPS明确max_num_reorder_frames=0且非隔行时，显示已完成picture并保留独立reference句柄；其它流继续原DPB重排。避免实时流等待下一帧才提交和等DPB满才输出。仅本项目VAAPI在完整AU结束调用。

B帧实测补充：Dpb::clear原来丢失max_num_reorder_frames，修正为和max_num_pics/interlaced一起保留。否则IDR后会错误进入无重排路径。回归检查参考面在显示后仍存活，B帧和隔行不走立即显示。
