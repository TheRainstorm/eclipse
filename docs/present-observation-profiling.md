# 同帧原生 Present 诊断

此通路记录“实际解码图像→原生 Present 调用”的身份与原始 Windows 统计，供离线 profiler 与 PresentMon/ETW 关联。后台文件与默认 HUD 的 DXGI 回执估测共用同一次原始查询；文件自身仍只有原始证据，不把 API 返回、GPU fence 或 `SyncQPCTime` 当作显示完成。

原始文件记录默认关闭；双方已协商 E2E 时默认的内存回执估测不依赖此开关。Windows PowerShell 中可为承载被测串流的新进程设置：

```powershell
$env:ECLIPSE_CONTINUOUS_TRACE = '1'
$env:ECLIPSE_NATIVE_PRESENT_TRACE_FILE = Join-Path $env:TEMP ('uu-present-' + [guid]::NewGuid().ToString('N') + '.jsonl')
.\uu-gui.exe
```

输出目录须已存在，每次使用新文件名；文件以 `create_new` 创建。复用既有串流进程不会改变其环境。正常退出被测串流进程时，事件循环结束后会停止采样、排空队列并写最终记录，最多等待两秒；关闭单个viewport不会提前停止全进程采样。移除这两个环境变量后启动的进程恢复默认行为。

客户端在实际解码输出处冻结 `media_epoch`、轨道、输出 PTS，以及可用的 E2E FrameToken/接收 RTP 身份。该 sidecar 与邮箱图像一起替换、退役，经过图像转换继续保留；GUI不在绘制时重新查询最新输入。旧主机也可利用现有 `ECLIPSE_CONTINUOUS_TRACE=1` 的接收身份进行离线关联，这不表示它支持新 E2E 协商。

当前可确认提交关联的是原生 D3D 视频路径：转换图像的 `native_source_id + native_source_ready` 必须与同次 Present 的原始观测一致，且会话仍有效。相同图像重绘保留原身份。共同路径转 RGBA 时保留已冻结的 sidecar；裸 `send(Rgba)` 没有身份。GL/UI-overlay 的图像到原生 Present 桥尚未接通时，提交记录保持 `no_native_image`，不重建或猜测身份。纯 WGL SwapBuffers 尚无此原生 FFI 观测。

输出按一秒或256条批量写入；渲染线程只提交固定元数据，不序列化 JSON 或写盘。队列最多512条，文件预算32MiB；慢消费者会丢观测并报告计数，不等待串流。启用观测会额外查询 Windows 统计，性能对照应固定该开关。

`video_stats.video_e2e` 在既有会话统计周期记录 HUD 同一快照，包含样本、源/回执/丢观测数、方法及 unknown 原因，便于不选中 HUD 文字时核验。方法 `dxgi_receipt_interval` 明确为经验区间；多屏/竞争全屏的统计限制不因 HRESULT 为 S_OK 而消失。ETW 仍是独立显示时间对照，参见 [HUD](hud.md)。

窗口完整性以最终 `native_present_trace_end` 记录验证，检查 `complete=true`、`reason=normal_shutdown`、`attempted_rows=rows_written` 和零 `dropped_rows_total`。达到文件预算、写入失败、退出等待超时或强制终止进程时，最终记录会标明不完整，或无法产生该记录；缺少它不能称为完整采集。

每行批次的 `event` 为 `native_present_observations`、`version=1`、`endpoint=raw_submission_only`，包含 PID、`dropped_rows_total` 与 `rows`。每条记录包含：

- `association`：`matched` 只表示已提交图像身份匹配；其他值明确表示缺失、退役或图像不匹配。
- `frame`：实际输出 PTS、媒体 epoch、轨道，以及可选的冻结 `token`、`receive` RTP身份。缺少身份保持空值。
- `raw`：链地址/身份、surface代次、Present serial、QPC频率、真实 Present 前后QPC、HRESULT、sync interval与flags、`GetLastPresentCount`结果及查询区间、完整原始五项 `DXGI_FRAME_STATISTICS`、其 HRESULT 与查询区间。
- `ui_reused`、`pixel_inspection`：标明 UI 复用和生成像素读回模式，后者不应混入性能样本。

离线匹配先限定同一进程、同一交换链，并要求 PresentMon 的 `TimeInQPC` 唯一落入原生 Present 前后QPC区间。`CPUStartQPC`不是这个 API 边界。PresentMon 的显示层 PresentId 与 DXGI 调用计数可能属于不同计数域，须先验证关系，再作为附加约束；不能仅靠最近时间或最近输入编号匹配。缺失、重复或歧义均保持未知。

`GetFrameStatistics` 的错误、全零值及 disjoint 均保留原样，查询不会调用 `DwmFlush` 或新增 GPU 等待。`SyncQPCTime`对应调度器采样的刷新计数，不能直接解释为该 Present 的显示时刻。实际显示端点应来自经过唯一关联的 OS 显示证据；跨主机源时刻还必须经过带误差的时钟映射。系统显示端点仍不包含显示器像素响应。参考：[DXGI帧统计定义](https://learn.microsoft.com/en-us/windows/win32/api/dxgi/ns-dxgi-dxgi_frame_statistics)、[统计限制](https://learn.microsoft.com/en-us/windows/win32/api/dxgi/nf-dxgi-idxgiswapchain-getframestatistics)。

双方协商视频端到端扩展后，`ECLIPSE_CONTINUOUS_TRACE=1` 还会向现有会话诊断记录 `video_e2e_sources` 和 `video_e2e_clock`。前者保留通过当前媒体代次门禁的源身份批次及 host epoch，后者保留实际接受的四时间戳交换及各自时钟身份、频率。离线关联应逐项核验源时间、SSRC、RTP、媒体代次和时钟有效期；不能直接相减两台机器的 QPC。

这些事件覆盖通道中可用的源描述，旧 `HostTrace.Report.frames` 的最近或慢帧子集不能代表完整总体。通道丢弃、诊断队列拥塞和轮转仍可能造成缺样；已匹配样本的延迟与总体覆盖率必须分开报告，缺失记录不能证明该帧未产生或未显示。关闭该环境变量后不生成上述批次诊断，正常 HUD 测量仍使用独立的内存观测模型。
