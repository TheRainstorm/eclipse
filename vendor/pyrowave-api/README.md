# PyroWave C API

固定的 PyroWave C API 1.0.0 头文件及其 Vulkan 类型依赖；未修改上游头文件。
源码版本、Granite 与其最小子模块版本见 `tools/pyrowave/sources.json`。
许可文本见本目录 `LICENSES.txt`，随包通知由 `licenses/third-party/pyrowave.txt` 生成。

运行库独立构建并随完整安装包分发：

```sh
python3 tools/pyrowave/build.py --jobs 6
```

只编译预生成 shader 和 C API 所需的最小 Granite 组件，不下载 shader 编译器、
播放器或其它可选依赖。运行时只加载安装目录内的运行库并核对精确 C API 版本。
Rust 不接触 C++ 对象布局。
