# 原生输入扩展（开发版）

v0.2.0 发布包仍为原生纯观看。开发版已加入自有控制协议与主机后端，图形端接入将继续完善；兼容模式的键鼠功能保持独立。

## 主机许可

真实显示器主机只有显式启用 `--allow-input`，才允许持有该次私有邀请的控制端请求键鼠：

```bash
uu native-host --invitation-file ./desktop.eclipse-invite.json --allow-input
```

省略此开关时始终仅观看。合成画面、单窗口源不允许桌面输入；不要把邀请交给希望授权范围以外的人。Windows 使用当前交互桌面的 SendInput，Linux 使用同一授权 X11 会话的 XTest；不自动提权或修改输入设备权限，Wayland 输入仍需后续 portal。Windows 目标窗口的权限限制仍由操作系统执行。

TLS 邀请与视频 Offer/Answer 保持 v0.2.0 格式。新控制端在已认证的 `eclipse-control-v1` SCTP 通道请求控制，主机确认后才启用输入；旧主机忽略该扩展时继续观看，旧控制端不请求控制时仍然只读。

## 坐标与释放

绝对鼠标位置按采集源矩形转换为全局物理像素，包含多显示器偏移和 Windows 负原点；相对移动另行传递。键盘采用标准 HID 物理键码，以远端键盘布局解释；当前不包含完整消费设备页、文本注入或 IME 字符串同步。

每次会话只记录自己成功按下的键和按钮。仅观看切换、失焦、连接关闭会请求释放；有界队列拥塞时，独立的代次重置会丢弃过期输入并保留释放请求。主机以1秒心跳续租，控制流失联5秒后释放输入。输入降级不停止视频接收；恢复控制需再次明确请求。

## 隔离验证

```bash
cargo build --release -p uu-core --example native_control_probe --features vaapi --locked
python3 tools/native/input_smoke.py \
  --probe ./target/release/examples/native_control_probe \
  --output ./artifacts/native-control-test
```

脚本创建有随机 Xauthority 认证的独立 Xvfb，并核对服务器 PID；测试示例也要求该隔离证明，不接受工作桌面作为默认目标。它验证真实采集、TLS/SRTP 视频、控制授权、按住超过租期的心跳、只读切换与断线释放，同时确认本会话未按下的外部按键保持原状。加 `--probe-arg=--read-only` 检查主机拒绝控制时仍能观看且输入计数为零。
