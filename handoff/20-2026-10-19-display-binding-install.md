# 20 · 未绑定显示器切换与D盘统一安装

后续实机反馈为持续切屏等待并超时。交接21更正本轮关于“等待屏幕清单重新绑定”的假设：官方旧切屏路径通知为Message8 CaptureChange，必须消费该通知，不以另一份ScreenSources为必要条件；静态证据和修复见[交接21](21-2026-10-19-display-change-tabs.md)。

用户反馈：选择屏幕报“此显示器超出当前支持的5路视频轨道范围”，顶部+离标签太远，并要求之后统一同步到 `D:\SMB\uu-gui-demo`。继续由用户做真实GUI实验，不自动启动、连接、注入或终止旧程序。

## 原因与修改

- `ee6723e`：顶部标签的横向滚动区原来关闭宽度自适应，只有少量标签也占满预留宽度，把+推到右边；改为按实际内容收缩，多标签仍支持受限宽度滚动。
- `badf5fb`：官方 `Screen.12 video_track_index` 是int32，未绑定视频轨道的源可使用负值。旧解析转成u32再检查小于5，错误拒绝了正常屏幕。现保留有符号值，以 `assigned_track()` 显式区分未绑定源，移除把容量当作身份范围的判断。
- 已绑定目标沿用 `RpcRequest.15 SendVideoTrackRequest`。未绑定目标先发官方 `Message.9 CaptureConfig`，仅包含client=3（WINDOWS）、screen_id和config_flag=8（仅切换采集源）；不附带分辨率/FPS/画质，不创建/禁用显示器，不请求超级屏。收到清单中的实际轨道绑定后再应用保存的目标设置。
- `Message.19 CaptureConfigResponse` 的字段1请求序号、2标志8、3错误码0对应本次激活。切换仍等待同序号成功响应、目标设置确认及目标新帧；未绑定、拒绝及超时期间不绘制旧源为新屏，不恢复键鼠。清理缓存、释放旧屏按键和鼠标的顺序保持。
- 收轨道优先采用SDP的 `video_N` 名称，数字mid作兼容回退，不靠到包顺序。保留五路接收m-line容量，这不等于只能选择编号小于5的显示器，也不声称已支持任意数量同时收流。

## 官方实现核对

本地 `GameViewerServer.exe` SHA256为 `a88e3afad478a75faf13b8ddbe118d95af7580abaa555efacb19dc84c3949303`。CaptureConfig处理器 `0x140473e10` 的flag8分支在 `0x14047411e..137` 调用 `0x14045e990`，校验现有源ID并重新绑定采集轨道0，更新屏幕信息。与直接调用现代采集设置查不到未绑定源轨道的路径不同。

x358h已有answer SDP只读核对了五个video m-line与 `video_0..4` 名称；未输出或保存到交接中的ICE凭据。协议测试与独立编码实现不导入官方二进制或GPL应用代码。

## 安装约定

自本轮起，正式目录是x358h `192.168.34.3:2202` 的 **`D:\SMB\uu-gui-demo`**。`tools/windows/install.ps1` 默认值、打包清单和使用说明统一更新。安装器将指向旧 `%USERPROFILE%\uu-gui-demo\uu-gui.exe` 的桌面/开始菜单快捷方式及桌面UU Remote Rust迁到新入口；旧C盘文件和正在运行的窗口保留，不再向C盘同步新版本。登录态仍在用户资料目录，无需迁移。

开发端仍执行 `python3 tools/windows/package.py`，从分发包运行安装器；安装后必须核对六项文件SHA256、version.json.source_commit、桌面目标和工作目录。不要复用仍写死C盘的旧临时部署脚本。用户需关闭旧主页和串流后重开桌面入口才能使用新版本。

## 验证与待验收

整仓131项通过（core45、GUI26、media36及其它24）；最后补充绑定失败/超时隐藏旧源的状态测试后GUI26项再次通过。原有迟到ICE的真实WebRTC回环新增激活请求、Message19响应及CONTROL接收分发断言；仍包含轨道切换、屏幕清单、键鼠、心跳、设置与双向文本剪贴板。

这些是本地合成对端测试和官方静态字段/分支核对，不是KVM-WIN11真实切屏验收。用户重开后请直接尝试顶部两个屏幕互切，确认画面、键鼠边界与加号位置；默认保持布局策略不变。

## 本轮交付

Windows release构建成功，已安装实际构建源 `072e36480df086e120530b518e8e8f8653d0ac1f`，GUI SHA256 `6f88d1aff81f87a613cf5d0e516313902c45a878676f9d59c20106e859ffc535`。六项文件哈希及源提交全部一致，桌面UU Remote Rust目标为 `D:\SMB\uu-gui-demo\uu-gui.exe`，工作目录为 `D:\SMB\uu-gui-demo`。旧窗口保留，重开后主页版本应为 `072e364`；发布记录提交不代表另一版二进制。

私有证据位于 `artifacts/ui-dpi-validation/display-binding-{workspace-test.log,gui-final-test.log,package.log,install.json,installed-verified.json}`，安装器在Windows PowerShell解析器检查通过。临时部署脚本 `/tmp/uu_deploy_demo_D.py` 已改为验证D盘；后续仍以仓库安装器和清单为准，不依赖临时脚本存在。
