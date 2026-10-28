# 主机逐帧测量

媒体重构首先建立可比较的主机基线。本阶段提供帧身份、有限轨迹和分位数报告；尚未完成客户端解码/呈现追踪、GPU 时间查询、资源池或异步流水线重构，不能据此宣布实际串流延迟已经降低。

## 身份和时钟

每次媒体会话使用独立 epoch，输入采样和编码输出有各自的序号，编码输出按 PTS 匹配输入。延后返回的输出不会归给当前编码调用；无法匹配时计数明确增加。参数更新和后端切换形成新测量 generation，旧样本不混入新一代分位数。

Windows Desktop Duplication 保留源帧 ID、源 generation、`LastPresentTime` 和 `AccumulatedFrames`。重复发送缓存桌面时仍推进输出 PTS，源帧 ID 保持不变并标记重复。仅鼠标更新不算新桌面。桌面重建会退休旧源信息。未提供源身份的平台报告未知，不能从输出 FPS 推断真实源 FPS。`LastPresentTime` 属于源主机的 QPC 时钟；源年龄只由同机 QPC 计算。[微软字段语义](https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_2/ns-dxgi1_2-dxgi_outdupl_frame_info)

其余事件采用该主机本地 monotonic 时钟，从本 epoch 起点计算微秒偏移。禁止将这些偏移直接减去客户端或另一主机的时间。

## 事件与报告

每个 `frames[].events_us` 数组按 schema v1 的以下固定顺序排列；未知事件为 `null`：

1. `capture_start`：调用采集前。
2. `capture_ready`：采集返回原始帧。
3. `encode_call_start`：调用编码器前。
4. `encode_call_return`：该次编码调用返回。
5. `au_ready`：与该输入 PTS 对应的编码输出可用。
6. `enqueue_return`：有界输出队列的提交返回。
7. `send_dequeue`：发送循环取出该输出。
8. `first_rtp_write_return`：首个 RTP 写入 API 返回。
9. `last_rtp_write_return`：尾个 RTP 写入 API 返回。

生产者提交返回可能晚于消费者出队，两者不强加不存在的先后关系。`encode_call` 包含当前同步编码接口的工作线程、转换与驱动等待，不能解释成纯视频引擎时间；RTP API 返回也不代表数据已经离开网卡。客户端收到遥测的时间不用于计算单程网络延迟。

`ready_to_dequeue` 对齐参考软件的主机处理口径；`ready_to_au` 与 `au_to_dequeue` 分别显示编码路径和发送等待的贡献。报告包含 p50/p95/p99/p99.9、最大值、样本数与本地时间窗口。分位数采用最近秩，有限窗口的 p99.9 可能等于最大值。

上下文中的 FPS、码率是配置目标，不能当成实际源帧率或驱动属性读回。已观察到的新源图像、重复和未知分别计数；驱动有效参数、GPU 完成和实际显示仍需后续测量。

客户端在报告写入槽的锁内复核会话代次，退休回调不能覆盖新会话；同一媒体会话绑定一个主机 epoch，并拒绝倒退的 generation、窗口和重复报告。无序到达不会把较新的统计改回旧值。

每会话最多保存 512 条原始观察和 128 个待匹配输入；溢出只丢观测，不丢视频，单独计数。分位数覆盖当前 generation 的环形窗口，生命周期计数累计整段会话。慢帧保留触发点前后最多 12 条观察；没有慢帧时报告最近观察。触发阈值为两个目标帧周期与 20 ms 中较大者，用于诊断，不触发媒体恢复。采集到已是静态桌面不视作处理卡顿。

热路径只更新固定上限内的类型化数据。最多每秒一次在工作线程复制与聚合报告，JSON 和写盘异步处理。客户端和主机通过独立 SDP 能力 `x-eclipse-frame-trace:1` 协商；仅双方支持时创建独立的 `eclipse-frame-trace-v1` 无序数据通道。报告最多 12 KiB，寿命 100 ms，已有数据未发出时跳过新报告。遥测不进入键鼠的可靠有序控制通道，原有严格解析的 `telemetry` 消息保持原格式。UU 官方兼容链路不宣称支持此扩展。

## 使用

两端更新后，原生连接会将报告记录为客户端会话诊断中的 `host_frame_trace` 事件。CLI 被控端结束时也输出同一 JSON 事件，`native probe --report` 包含最后报告及已有诊断。汇总一个或多个本地诊断文件：

```bash
python3 tools/diagnostics/frame_trace_report.py session.jsonl
python3 tools/diagnostics/frame_trace_report.py session.jsonl session.tail-1.jsonl --json
```

工具只选取主机测量报告，不输出诊断文件中的账号、地址或其它会话事件。导出前仍应检查实际输入文件与自定义后端名称。

生成源自环验证不接触真实桌面、账号、输入或显示设置：

```bash
cargo run -p uu-core --example frame_trace_probe --release -- generated.json 120 600
ECLIPSE_FRAME_TRACE=0 cargo run -p uu-core --example frame_trace_probe --release -- disabled.json 120 600
python3 tools/diagnostics/frame_trace_report.py generated.json
python3 tools/native/frame_trace_compare.py --exe target/release/examples/frame_trace_probe --output trace-comparison
```

对照工具先保存门槛再运行三轮 ABBA（12 个独立进程）：稳态吞吐中位数下降不超过 1%，解码帧发布间隔 p99 的中位数增加不超过 0.5 ms。间隔在 mailbox 发布回调中记录，避开观察者轮询节奏。此门槛仅评估生成源的软件自环遥测开销，不能替代真实 4K HDR、硬编硬解、网络、GPU 完成或显示器 A/B。Windows 的 `source_identity_probe --desktop` 可额外只读验证当前桌面的源身份，输出元数据而不导出像素。

## Capture overlap measurement extension

Frame trace v2 adds replayable source-host QPC source-present-to-last-RTP-API
statistics and capture-slot waiting while retaining the existing stage boundaries.
See [the default-off capture overlap experiment](capture-overlap-experiment.md)
for ownership, bounded reporting, compatibility and acceptance limits.

## DXGI capture-attempt diagnostics

For an explicitly instrumented run, set both `ECLIPSE_CONTINUOUS_TRACE=1`
and `ECLIPSE_CAPTURE_DIAGNOSTIC=1` before opening the capture source. The
default is disabled. A single capture owner retains up to 32768 metadata
records in memory and writes one `dxgi_capture_diagnostics` JSON line to stderr
when its capture object is dropped. No per-frame file IO, logging lock, GPU
wait or texture retention is added; recording does add QPC/thread reads and
bounded metadata work when explicitly enabled.

Each actual AcquireNextFrame attempt preserves its sequence, raw QPC bracket,
requested timeout, HRESULT, desktop/pointer update QPC, accumulated updates,
and actual GPU/overlap/separate-device mode. `frame_info_valid=false` means the
DXGI fields on an unsuccessful call must not be interpreted as returned data.
The result distinguishes `wait_timeout`, `pointer_only`, `pool_busy`, a
successfully returned `fresh` snapshot, retirement and other errors. An
acquired resource that never becomes an output remains `acquired_not_returned`.
Returning a cached image keeps the actual failure/no-update reason and records
`cache_replay=true`, the old source identity and the new capture-return PTS.
Retirement records distinguish a source-generation retirement from a cache-only
reset; neither relabels an old image as a new source.

`returned_capture_pts_us` is the RawFrame PTS at the capture API boundary, not
an encoded AU or RTP timestamp. In particular, the overlap consumer may later
retimestamp an authorized replay without another DXGI attempt. Join a full
HostTrace by explicit PTS/source/QPC evidence; do not assume one acquire per
encoded frame. `LastPresentTime` is the OS desktop update time, not an encode
start, physical pixel time or causal mouse response.

Overflow explicitly increments `evicted`; retained metadata is not a proof of
complete capture history. The terminal means only that Drop serialized this
recorder (`clean_session_proven=false`); session exit and cleanup require their
own evidence. A forced exit, abort, failed write, partial JSON line or missing
terminal remains unknown. These fields are local diagnostic data and do not
extend FrameInfo or the older strict frame-trace wire protocol.
