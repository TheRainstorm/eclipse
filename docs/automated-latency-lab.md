# 自动化实机延迟实验

`tools/native/latency_lab.py` 把固定产物、真实桌面串流、重连、指标检查和候选对照串起来。每轮使用独立进程、配置目录和临时邀请；不会覆盖正在使用的版本、重启服务、修改显示设置、注入键鼠、自动合并或发布。机器地址和部署适配器保存在本地开发资料中。

这一版覆盖 SDR 桌面、GPU 采集输入、硬件编码/解码和复用产品 `VideoTexture` 的测试窗口。客户端不是完整主页与串流菜单，不能替代完整 UI、输入、音频、HDR、驱动故障或锁屏验收。绘制回调代表图形 API 调用边界，不能视为扫描输出或真实端到端延迟。

## 执行流程

1. 从已提交的完整 SHA 构建并校验产物。现有完整包构建脚本支持同 SHA 缓存；测试探针另行构建。构建完成后固定哈希，再开始实验，测试中不追随移动的 HEAD。
2. 适配器在两端暂存独立目录，核对完整包清单与探针哈希，以交互会话的普通用户启动有时限的测试进程。
3. 被控端显示测试专用、不激活的动画窗口，走 DWM 和实际桌面采集。客户端打开只接收视频的测试窗口，使用产品视频纹理渲染器。每轮结束关闭连接并检查自有进程退出。
4. 基线模式重复连接；对照模式每个条件按 A→B→B→A 顺序运行。默认两组，不同时运行两个竞争 GPU 的实验。
5. 保存报告、主机轨迹、进程资源采样、实际后端、环境指纹、失败日志和退出证据。基础设施或清理失败立即停止后续硬件工作，性能不达标则保留整组对照证据。
6. 修改一个经测量定位的瓶颈，独立提交、重新构建候选，再执行相同实验。只在硬门槛全部通过、每组 ABBA 的吞吐和尾部均不退化、至少两组都达到预登记收益时，将候选标记为可接受。不会自动改写源码或把偶然较快的一次结果合并。

```sh
cargo build --release -p uu-gui --example latency_viewer_probe --locked
python3 tools/native/latency_lab.py \
  --config /path/to/private-experiment.json \
  --output /path/to/new-result-directory --cycles 2
```

Windows 编译增加 `--target x86_64-pc-windows-gnu`。探针旁放置经过完整包清单校验的运行时 DLL。`latency_viewer_probe` 接收私有配置文件，其字段为 `invitation`、新建的 `report`、`seconds`、`warmup_seconds`、`fps`、`bounds`、`bitrate_kbps`、`codec`、`vsync`。Windows 由 `windows_latency_worker.py` 在交互会话运行；它通过内核 Job 管理自有后代进程，调度器终止 worker 后仍会清理后代。

`--cycles 1` 可用于排障，不会接受性能候选。`--stop-file PATH` 在当前轮清理后停止；不完整实验不能通过。进程超时、探针失败、缺少报告均返回非零状态并保存失败，不会当作“零延迟”。

## 私有实验配置和适配器

配置 `version` 为 1；`adapter` 是 argv 数组，不经过 shell。`variants.baseline` 和可选的 `variants.candidate` 分别提供 `revision`（完整提交）、`artifacts`（包含 `path`、`sha256` 的数组）和适配器需要的额外字段。所有文件在实验前和每轮前核对 SHA-256。`adapter_options` 仅交给本地适配器，放入机器配置等私有内容。

`cases` 为不同条件的数组，每项明确：

| 字段 | 含义 |
| --- | --- |
| `name` | 唯一名称，限字母、数字、下划线、连字符 |
| `seconds` / `warmup_seconds` | 每轮稳态秒数（5–3600）/首帧后热身（1–30） |
| `source` / `hdr` | 当前分别要求 `desktop` / `false`，其它组合明确拒绝 |
| `size` / `fps` / `bitrate_kbps` / `codec` | 期望实际编码尺寸、目标帧率、目标码率、编码 |
| `vsync` | 测试窗口垂直同步设置，两变体一致 |
| `encoder_contains` / `decoder_contains` / `render_path` | 必须命中的实际后端与渲染路径，回退即失败 |

`criteria` 必须在实验前完整给定，工具没有看到结果后放宽阈值的逻辑：

| 字段 | 判断 |
| --- | --- |
| `min_paint_fps_ratio` | 绘制新图帧率至少为目标帧率的此比例 |
| `min_new_source_ratio` | 主机采集样本中至少有此比例是真实新源帧 |
| `max_paint_gap_ms` | 热身后任意新图绘制间隔的上限 |
| `max_mailbox_p99_ms` / `max_first_frame_ms` | 邮箱到绘制回调 p99 / 首帧时间上限 |
| `max_memory_growth_mb` | 适配器报告的进程私有内存增长上限 |
| `fps_max_relative_drop` | 候选对基线吞吐的最大相对退化 |
| `tail_max_added_ms` | 绘制间隔、邮箱与主机发送尾部的允许增加量 |
| `objective` | `host_ready_to_last_rtp_p99_window_median_ms` 或 `mailbox_to_draw_p99_ms` |
| `improvement_min_ms` | 每组对照的目标收益下限；至少两组分别满足，吞吐和各项尾部也逐组检查 |

适配器调用为 `adapter ... --request FILE --output DIRECTORY`。请求含 `run_id`、固定变体、条件和适配器选项。适配器必须留下：

- `viewer.json`：上述 Rust 探针的原始报告。
- `host.log`：完整主机输出，包含唯一的最终 `host_frame_trace` 事件。
- `worker.json`：`completed`、`cleanup_ok`、`revision`、`artifact_sha256` 数组、`desktop_capture`、`stimulus_updates`、`memory_growth_mb`；以及 `environment` 中的 `host_gpu`、`client_gpu`、`host_display`、`client_display`、`network`、`stimulus` 指纹。
- 原始子进程日志、退出码和资源采样。失败也要执行清理；远端任务必须有独立于 SSH 连接的截止时间，不能仅依赖控制端 finally。

必须比较显示模式、驱动、网络路径、源负载、编码质量预设等条件；环境指纹变化会使对照失败。适配器应验证包内源码提交、逐文件哈希、探针来源；不能只把请求的 SHA 抄到结果里。不要启动新测试去竞争用户当前正在使用的串流。

## 指标与迭代边界

探针对热身后的新图绘制间隔、**同一张邮箱图像**的发布到绘制回调时间记录 p50/p95/p99/p99.9/max，及超过 2T/50/100/250ms 的数量。样本有上限，溢出即失败。原始报告保留每秒解码均值、接收/解码队列节奏与主机有限轨迹。

主机轨迹是重叠的 512 帧窗口。汇总使用“每窗口 p99 的中位数”，不会把重叠窗口重复计数后冒充整场 p99。全场主机 p99、GPU 完成、跨机时钟对应、实际扫描输出、图像质量一致性和网络故障恢复仍需补充测量。报告中的 FPS、码率是配置目标；实际输出帧率另记，不能视为驱动参数回读。

短基线用于检查流程和定位瓶颈，不能声称已经完成长稳验收。长测可为关键条件设置 `seconds: 3600` 并运行三轮以上，同时检查资源变化和退出。新候选应先复现短对照，再扩大长测、完整 GUI 与受控故障场景。每个优化保留基线、候选、门槛、证据与独立 Git 提交，失败候选不晋级。
