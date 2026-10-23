# 原生键鼠控制

v0.2.0 发布包仍为原生纯观看。v0.3.0 已接入主机后端与图形端键鼠控制；兼容模式的键鼠功能保持独立。

## 主机许可

真实显示器主机只有显式启用 `--allow-input`，才允许持有该次私有邀请的控制端请求键鼠：

```bash
uu native-host --invitation-file ./desktop.eclipse-invite.json --allow-input
```

省略此开关时始终仅观看。合成画面、单窗口源不允许桌面输入；不要把邀请交给希望授权范围以外的人。Windows 使用当前交互桌面的 SendInput，Linux X11 使用同一授权会话的 XTest，Wayland 使用与屏幕采集相同且已获键鼠许可的 RemoteDesktop 门户会话。不自动提权或修改输入设备权限，未获得门户输入授权时继续仅观看。Windows 目标窗口的权限限制仍由操作系统执行。

TLS 邀请与视频 Offer/Answer 保持 v0.2.0 格式。新控制端在已认证的 `eclipse-control-v1` SCTP 通道请求控制，主机确认后才启用输入；旧主机忽略该扩展时继续观看，旧控制端不请求控制时仍然只读。

## 图形端控制

主页“原生直连”提供“请求键鼠控制”选项；主机未许可时继续仅观看，不影响视频。连接后单击画面获得输入焦点，可在悬浮菜单“安全”中切换仅观看，或重新请求已释放的控制权。命令行 `uu-gui --native-invitation ./desktop.eclipse-invite.json --view-only` 显式只读。

桌面绝对定位与游戏相对移动复用现有鼠标菜单；目前显示本地光标。Ctrl+Alt+S 切换 HUD、F/X 切换全屏、Q 退出串流、Z 释放或捕获键鼠，均由本地处理。

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

Windows 交互桌面可额外检查 SendInput 的实际 OS 消息投递：

```powershell
cargo run --release -p uu-input --example windows_owned_probe -- --owned-window
```

该检查短暂打开自己的前台窗口，验证标点与扩展键扫描码、中键与侧键、双轴滚轮和按键释放。每次发送前检查前台归属，鼠标按键和滚轮还核对指针命中该窗口；焦点移走后停止检查。结束时恢复先前指针与前台窗口，焦点已被用户切换时保持用户的新选择。CI 编译该示例，实际交互检查需在可用的桌面会话显式运行。
