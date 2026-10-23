# Windows 后台服务与锁屏串流

在主窗口的服务设置中选择“安装并启动后台服务”，确认 Windows 管理员提示。安装使用当前已保存的服务设置、UU 登录状态、设备身份、配对密码和授权记录；已启用的自建入口凭据会复制到服务的受保护目录。客户端收藏、预览和串流偏好继续保留在用户配置中。

后台服务随 Windows 启动，等待安装时选定的控制台用户登录。管理员提示中使用另一位管理员的凭据时，共享用户仍是当前控制台用户。在同一用户按 Win+L 锁屏后保持运行；注销或切换到其他 Windows 用户时停止该用户的媒体进程，该用户重新登录后恢复。需要有效的物理或虚拟显示输出，系统休眠、关机、断电或未登录状态不能开始串流。当前实现针对控制台会话，不接管任意 RDP 会话。

普通窗口共享仍受当前用户的桌面权限限制。Windows 的安全桌面采集要求 LocalSystem 身份；后台服务在目标控制台会话中启动无界面的 System 媒体进程。采集和输入工作线程跟随当前输入桌面，桌面切换或 DXGI 失效时清除缓存并重建采集，恢复后发送关键帧。服务不会自动解锁 Windows；用户仍需在远端输入自己的 Windows 登录凭据。通过 SendInput 不能生成安全注意序列 Ctrl+Alt+Delete。

“管理后台服务”打开独立的管理员窗口，可以批准新设备配对、设置密码和授权公钥、调整设备权限、查看与断开连接，以及编辑共享设置。配置可在串流中保存，选择下次启动生效，或保存后重启再连接。关闭管理窗口后，后台服务继续运行。拥有“管理服务和连接权限”的远端设备也可通过原生串流窗口管理服务，沿用相同的保存与权限规则。

安装或更新后台程序成功后，前台共享的自动启动选项关闭。前台用户配置和后台服务配置分别保存，后续后台设置应在后台管理窗口或具有管理权限的远端客户端中修改。主窗口重新登录或登出 UU 后，可以使用“同步主窗口的 UU 账号状态”更新后台账号；此操作重启正在运行的后台服务。安装新版普通程序后，使用“更新后台程序”更新受保护的后台程序，保留后台配置和配对身份。安装器先准备完整的新版本，再切换服务；切换或启动失败时恢复原安装记录和服务状态，并报告恢复结果。

也可以在安装包目录使用管理员 PowerShell：

```powershell
.\uu.exe service install --open-manager
.\uu.exe service status
.\uu.exe service manage
.\uu.exe service stop
.\uu.exe service start
.\uu.exe service restart
.\uu.exe service import-account
.\uu.exe service remove
```

服务名为 `EclipseRemoteHost`。程序安装在 Windows Program Files 的同名目录，配置和服务日志保存在 ProgramData 的同名目录；实际位置通过 Windows Known Folder API 获取。日志位于服务配置的 `state/uu-remote/log/service.log`，达到约 4 MiB 后在新进程启动时轮换。移除服务注册会保留受保护配置，便于重新安装；彻底移除时，在服务停止后由管理员删除对应目录。

安装器验证包内程序哈希，拒绝重解析点及普通用户可修改的服务目录。媒体进程由服务私有 Job 对象管理，停止时先请求正常退出，超过期限仅终止该服务拥有的进程。管理通道仅向本地管理员与 System 开放，拒绝远程命名管道连接；客户端还会核验服务器的 System 身份及受保护程序位置。新设备仍需完成配对，后台服务不自动批准请求。

SCM 在服务异常退出后自动重启，连续失败时等待时间依次为 1、5、15 秒；正常停止服务不会触发这项恢复。媒体子进程由服务单独监督，退出后重新等待所属控制台会话并启动。

实现参考了 Sunshine 的服务与桌面生命周期设计，代码独立实现。Windows 平台约束见 [DuplicateOutput](https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_2/nf-dxgi1_2-idxgioutput1-duplicateoutput)、[OpenInputDesktop](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-openinputdesktop) 和 [SetThreadDesktop](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setthreaddesktop)。
