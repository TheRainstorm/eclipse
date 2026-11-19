# CPU 颜色转换优化与验证

2026-10-19：NV12/I420 到 GUI RGBA 的原逐像素路径在开发机上每张1080p帧需要约20ms，已足以挤占60FPS的16.7ms预算。硬件解码之后仍执行这一段，因此只看HUD的 Decode ms 无法看到它。

现在x86_64运行时检测SSE4.1，一次转换8个像素；未支持的CPU及其它架构保留原标量实现。保留原BT.601的16.16系数、右移舍入和限幅，尾部不足8像素仍标量处理，不读取行外补齐字节。NV12沿用有pitch的UV平面，I420沿用解码适配器的pitch亮度+紧排U/V约定。奇数可视尺寸或截断420缓冲不进入内核。

该实现复用本仓库原颜色算式，未复制参考项目的实现。参考Moonlight的SDL路径可以直接上传NV12/I420或用libswscale回退；当前egui/glow仍上传RGBA，因此先缩短这条现有CPU路径，后续GPU呈现仍需单独改造。

## 可重跑的离线测量

```sh
cargo run --release -p uu-media --example convert_bench -- 60
cargo test -p uu-media conversion_equivalence_tests
```

开发机AMD Ryzen 5 5600G，Linux x86_64，release构建；旧版和新版均固定在同一个逻辑CPU，先暖一次，再连续转换60帧，统计含输出分配的均值。1080p pitch2048，4K pitch4096。

| 输入 | 旧版 ms/帧 | 新版 ms/帧 | 输出指纹 |
|---|---:|---:|---|
| NV12 1920×1080 | 19.989 | 1.361 | ce57705df1deac42 |
| I420 1920×1080 | 19.007 | 1.346 | d9c9c1b7ec2db796 |
| NV12 3840×2160 | 79.629 | 5.752 | 3eea3a8812f645c5 |
| I420 3840×2160 | 76.000 | 5.565 | d90ebca4a2b03242 |

四组输出指纹均与旧版一致；更严格的测试逐字节比对全部65,536种U/V组合及亮度边界，另覆盖可视宽度2–66、多种高度/奇偶pitch、向量尾部、完整奇数尺寸和截断平面。Windows交叉检查通过。

这里只量CPU转换，不包括解码、驱动回读、GPU纹理上传、呈现或网络，也不是Windows或4K60串流验收。GPU回读和上传仍存在；本优化不会直接增加官方被控端的采集帧率。私有测量保存在 `artifacts/live-display-validation/convert-{before,after}-pinned.log`。
