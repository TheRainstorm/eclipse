网易UU远程 — Rust 原生客户端（Windows x64）

统一安装：
  - 正式目录固定为 %USERPROFILE%\uu-gui-demo，启动 uu-gui.exe。
  - 在新包目录执行 powershell -ExecutionPolicy Bypass -File .\install.ps1。
  - 安装器校验 version.json 的哈希，旧 rtx-auto / hud-recovery 与备份移到
    %USERPROFILE%\uu-gui-archive，桌面统一使用 UU Remote Rust 快捷方式。
  - 主页底部显示版本提交号，悬停可查看实际 exe 路径。

运行：
  1. 双击 uu-gui.exe；首次运行如有防火墙提示，允许 UDP/TCP。
  2. 首次启动显示二维码，用手机扫码登录。
  3. 左侧选择设备，点击“进入桌面”；在独立窗口打开串流，原设备主窗口保留，可继续连接其他设备。
  4. 界面按钮仅最大化窗口；Ctrl+Alt+F / X 切换全屏串流，Ctrl+Alt+S 切换 HUD；控制中心可调画质、分辨率、帧率和观看模式。
  5. 全屏悬浮入口可拖动；Ctrl+Alt+M 隐藏/恢复入口，Ctrl+Alt+Q 或菜单中“退出远控”断开。

解码：
  - 默认优先 D3D11VA（Media Foundation），失败自动回退 OpenH264。
  - Ctrl+Alt+S 显示顶部紧凑 HUD；悬停查看实际后端、回退原因及统计定义。
  - HUD 显示实际编码、Rx/De/Rd、RTT±波动、丢包、带宽、Render/Decode；保留丢包、重传、乱序、坏帧和恢复计数。
  - HUD 顶部居中，背景跟随文字范围，不显示远端未提供的 Encode 延迟。
  - Intel Arc B390 / AMD RX 7900 XTX 已实测；1080p 输出按可见尺寸裁剪。
  - 硬解目前仍有 GPU→CPU 回读，实测速度不一定优于软解。
  - 设置 UU_DECODER=sw 可强制软解；UU_DECODER=auto 恢复默认选择。
  - 当前实际帧率也取决于被控端；官方静态桌面会主动降帧。
  - 画质菜单提供 30/60/90/120/144 和自定义 5–144 FPS，默认请求60；设置确认不等于实际达到该帧率。
  - FPS 已合并到画质菜单；黑点标记当前选项，自定义码率/帧率/分辨率从侧面展开，确认后回主菜单。
  - 不提供超级屏启用/退出操作，只使用现有远端显示屏；HUD 仍可只读显示屏幕类型和刷新率。
  - 高帧率仍取决于远端采集、编码、网络及本地呈现；帧率通过 CONTROL 就绪后的设置请求发送。
  - UU_FPS=120 可指定连接初始目标；旧 UU_FPS=1/2/3/4 分别兼容30/60/90/144。

离线自测（PowerShell，在本目录运行）：
  .\uu.exe probe --h264 C:\path\sample.h264
  .\uu.exe probe --h264 C:\path\sample.h264 --frame first-frame.ppm
  $env:UU_DECODER='sw'; .\uu.exe probe --h264 C:\path\sample.h264
  Remove-Item Env:UU_DECODER
  --frame 将首帧写成 PPM；探测同时打印尺寸、行跨距、帧数和最终后端。
  无输出帧或有解码错误时返回非零退出码。

注意：
  - libstdc++-6.dll 和 libgcc_s_seh-1.dll 必须与两个 exe 放在同一目录。
  - 登录态保存在 %USERPROFILE%\.local\share\uu-remote\state.json。
  - 键鼠走 CONTROL_DATA_CHANNEL 裸 JSON；Windows 路径中键值 16，支持按住拖动。
  - 画面与输入分别建立，顶部“输入连接中”期间不可操作；45秒超时报出各传输层状态。
  - 已修补DTLS握手结束后的接收竞态，避免接收任务等待过期确认、阻断SCTP握手而视频仍更新。
  - HUD 悬停可查看输入通道状态、本地发送数与失败数；发送成功不代表远端已执行。
  - 默认官方自动画质、跟随远端分辨率；也可选 1080p/1440p/4K 或手动码率。
  - 观看模式不转发键鼠输入，悬浮菜单和设置操作不会穿透到远端。
  - 视频接收端有短时重排/NACK/RTX、坏帧丢弃和 IDR 参考链恢复；视频 FEC 尚未接入。
  - Windows 硬解保留源 PTS，解码器重建不再把时间戳归零。
  - 串流窗口默认在目标显示器可用区域的 80% 范围内等比适配、居中，可手动调整大小。
  - 串流窗口跨屏或 DPI 变化后重新按目标工作区的 80% 适配、居中；同屏手动大小保留。
  - 主页在启动光标所在显示器打开；串流窗口跟随设备主窗口所在显示器。
  - Win+Shift+方向键在本地切换当前窗口所在显示器，支持窗口、最大化和全屏模式。
  - 每台设备独立串流窗口；再次点击同一设备会激活已有窗口。关闭串流只释放该设备。
  - 关闭设备主窗口时，已有串流窗口继续运行；每个串流独立退出清理。
  - 主页跨 DPI 和跨屏快捷键移动时同时约束窗口大小、位置，避免超出屏幕工作区。
  - 非全屏时支持拖动四边/四角缩放窗口；确认画质后回到主菜单。
  - 退出会等待后台释放会话；取消连接和异常也执行清理。
  - 网络丢失不重建健康解码器；重传窗口随 RTT 调整，完整帧不额外等待。
  - 压缩帧和显示队列有界，新帧到达立即唤醒显示。
  - 垂直同步默认开启；UU_VSYNC=0 可关。

声音、输入和设置：
  - 接收 Opus 双声道音频，声音菜单可调音量与静音；默认跟随 Windows 默认播放设备。
  - 音频采用短时乱序等待、有界播放缓冲和 Opus 内带恢复/丢包隐藏；不保证远端每包都有 FEC。
  - 全屏且本串流窗口处于前台时，可捕获 Alt+Tab、Windows 键等系统快捷键发往远端。
  - Ctrl+Alt+Del 由本地 Windows 安全桌面保留；本地退出、全屏、HUD、悬浮入口快捷键不转发。
  - 鼠标菜单支持桌面绝对定位 / 游戏原始相对位移，以及本地光标 / 远端画面捕获光标。
  - 游戏模式单击画面捕获鼠标；Ctrl+Alt+Z 释放或重新捕获，之后可操作本地菜单。
  - 打开设置、菜单或窗口失焦、退出时释放远端键鼠。
  - 本地光标支持远端 PNG 形状、热点与缩放；未知图片格式回退系统箭头。
  - 主页右上角与串流控制中心都可打开软件设置。
  - 设置独立保存在 %USERPROFILE%\.local\share\uu-remote\gui-settings.json，不覆盖登录态。
  - 保存并应用影响当前串流和以后新建窗口；垂直同步、解码方式下次启动生效。

第三方组件许可（webrtc-dtls 0.10.0，采用其 MIT 许可）：
MIT License

Copyright (c) 2021 WebRTC.rs

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.


第三方音频组件：libopus 1.3 / BSD-3-Clause
Copyright 2001-2011 Xiph.Org, Skype Limited, Octasic,
                    Jean-Marc Valin, Timothy B. Terriberry,
                    CSIRO, Gregory Maxwell, Mark Borgerding,
                    Erik de Castro Lopo

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions
are met:

- Redistributions of source code must retain the above copyright
notice, this list of conditions and the following disclaimer.

- Redistributions in binary form must reproduce the above copyright
notice, this list of conditions and the following disclaimer in the
documentation and/or other materials provided with the distribution.

- Neither the name of Internet Society, IETF or IETF Trust, nor the
names of specific contributors, may be used to endorse or promote
products derived from this software without specific prior written
permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS
``AS IS'' AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT
LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR
A PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT OWNER
OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL,
EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO,
PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE, DATA, OR
PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF
LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING
NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF THIS
SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

Opus is subject to the royalty-free patent licenses which are
specified at:

Xiph.Org Foundation:
https://datatracker.ietf.org/ipr/1524/

Microsoft Corporation:
https://datatracker.ietf.org/ipr/1914/

Broadcom Corporation:
https://datatracker.ietf.org/ipr/1526/


第三方音频组件：audiopus_sys 0.2.2 / ISC
ISC License

Copyright (c) 2019, Lakelezz

Permission to use, copy, modify, and/or distribute this software for any
purpose with or without fee is hereby granted, provided that the above
copyright notice and this permission notice appear in all copies.

THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES
WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF
MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR
ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN
ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF
OR IN CONNECTION WITH THE USE OR PERFORMANCE OF THIS SOFTWARE.


第三方音频组件：opus-rs 0.3.1 / MIT
Copyright (c) 2016 Tad Hardesty

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in
all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT.  IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
THE SOFTWARE.


第三方音频组件：cpal 0.15.3 / Apache-2.0
                                 Apache License
                           Version 2.0, January 2004
                        http://www.apache.org/licenses/

   TERMS AND CONDITIONS FOR USE, REPRODUCTION, AND DISTRIBUTION

   1. Definitions.

      "License" shall mean the terms and conditions for use, reproduction,
      and distribution as defined by Sections 1 through 9 of this document.

      "Licensor" shall mean the copyright owner or entity authorized by
      the copyright owner that is granting the License.

      "Legal Entity" shall mean the union of the acting entity and all
      other entities that control, are controlled by, or are under common
      control with that entity. For the purposes of this definition,
      "control" means (i) the power, direct or indirect, to cause the
      direction or management of such entity, whether by contract or
      otherwise, or (ii) ownership of fifty percent (50%) or more of the
      outstanding shares, or (iii) beneficial ownership of such entity.

      "You" (or "Your") shall mean an individual or Legal Entity
      exercising permissions granted by this License.

      "Source" form shall mean the preferred form for making modifications,
      including but not limited to software source code, documentation
      source, and configuration files.

      "Object" form shall mean any form resulting from mechanical
      transformation or translation of a Source form, including but
      not limited to compiled object code, generated documentation,
      and conversions to other media types.

      "Work" shall mean the work of authorship, whether in Source or
      Object form, made available under the License, as indicated by a
      copyright notice that is included in or attached to the work
      (an example is provided in the Appendix below).

      "Derivative Works" shall mean any work, whether in Source or Object
      form, that is based on (or derived from) the Work and for which the
      editorial revisions, annotations, elaborations, or other modifications
      represent, as a whole, an original work of authorship. For the purposes
      of this License, Derivative Works shall not include works that remain
      separable from, or merely link (or bind by name) to the interfaces of,
      the Work and Derivative Works thereof.

      "Contribution" shall mean any work of authorship, including
      the original version of the Work and any modifications or additions
      to that Work or Derivative Works thereof, that is intentionally
      submitted to Licensor for inclusion in the Work by the copyright owner
      or by an individual or Legal Entity authorized to submit on behalf of
      the copyright owner. For the purposes of this definition, "submitted"
      means any form of electronic, verbal, or written communication sent
      to the Licensor or its representatives, including but not limited to
      communication on electronic mailing lists, source code control systems,
      and issue tracking systems that are managed by, or on behalf of, the
      Licensor for the purpose of discussing and improving the Work, but
      excluding communication that is conspicuously marked or otherwise
      designated in writing by the copyright owner as "Not a Contribution."

      "Contributor" shall mean Licensor and any individual or Legal Entity
      on behalf of whom a Contribution has been received by Licensor and
      subsequently incorporated within the Work.

   2. Grant of Copyright License. Subject to the terms and conditions of
      this License, each Contributor hereby grants to You a perpetual,
      worldwide, non-exclusive, no-charge, royalty-free, irrevocable
      copyright license to reproduce, prepare Derivative Works of,
      publicly display, publicly perform, sublicense, and distribute the
      Work and such Derivative Works in Source or Object form.

   3. Grant of Patent License. Subject to the terms and conditions of
      this License, each Contributor hereby grants to You a perpetual,
      worldwide, non-exclusive, no-charge, royalty-free, irrevocable
      (except as stated in this section) patent license to make, have made,
      use, offer to sell, sell, import, and otherwise transfer the Work,
      where such license applies only to those patent claims licensable
      by such Contributor that are necessarily infringed by their
      Contribution(s) alone or by combination of their Contribution(s)
      with the Work to which such Contribution(s) was submitted. If You
      institute patent litigation against any entity (including a
      cross-claim or counterclaim in a lawsuit) alleging that the Work
      or a Contribution incorporated within the Work constitutes direct
      or contributory patent infringement, then any patent licenses
      granted to You under this License for that Work shall terminate
      as of the date such litigation is filed.

   4. Redistribution. You may reproduce and distribute copies of the
      Work or Derivative Works thereof in any medium, with or without
      modifications, and in Source or Object form, provided that You
      meet the following conditions:

      (a) You must give any other recipients of the Work or
          Derivative Works a copy of this License; and

      (b) You must cause any modified files to carry prominent notices
          stating that You changed the files; and

      (c) You must retain, in the Source form of any Derivative Works
          that You distribute, all copyright, patent, trademark, and
          attribution notices from the Source form of the Work,
          excluding those notices that do not pertain to any part of
          the Derivative Works; and

      (d) If the Work includes a "NOTICE" text file as part of its
          distribution, then any Derivative Works that You distribute must
          include a readable copy of the attribution notices contained
          within such NOTICE file, excluding those notices that do not
          pertain to any part of the Derivative Works, in at least one
          of the following places: within a NOTICE text file distributed
          as part of the Derivative Works; within the Source form or
          documentation, if provided along with the Derivative Works; or,
          within a display generated by the Derivative Works, if and
          wherever such third-party notices normally appear. The contents
          of the NOTICE file are for informational purposes only and
          do not modify the License. You may add Your own attribution
          notices within Derivative Works that You distribute, alongside
          or as an addendum to the NOTICE text from the Work, provided
          that such additional attribution notices cannot be construed
          as modifying the License.

      You may add Your own copyright statement to Your modifications and
      may provide additional or different license terms and conditions
      for use, reproduction, or distribution of Your modifications, or
      for any such Derivative Works as a whole, provided Your use,
      reproduction, and distribution of the Work otherwise complies with
      the conditions stated in this License.

   5. Submission of Contributions. Unless You explicitly state otherwise,
      any Contribution intentionally submitted for inclusion in the Work
      by You to the Licensor shall be under the terms and conditions of
      this License, without any additional terms or conditions.
      Notwithstanding the above, nothing herein shall supersede or modify
      the terms of any separate license agreement you may have executed
      with Licensor regarding such Contributions.

   6. Trademarks. This License does not grant permission to use the trade
      names, trademarks, service marks, or product names of the Licensor,
      except as required for reasonable and customary use in describing the
      origin of the Work and reproducing the content of the NOTICE file.

   7. Disclaimer of Warranty. Unless required by applicable law or
      agreed to in writing, Licensor provides the Work (and each
      Contributor provides its Contributions) on an "AS IS" BASIS,
      WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or
      implied, including, without limitation, any warranties or conditions
      of TITLE, NON-INFRINGEMENT, MERCHANTABILITY, or FITNESS FOR A
      PARTICULAR PURPOSE. You are solely responsible for determining the
      appropriateness of using or redistributing the Work and assume any
      risks associated with Your exercise of permissions under this License.

   8. Limitation of Liability. In no event and under no legal theory,
      whether in tort (including negligence), contract, or otherwise,
      unless required by applicable law (such as deliberate and grossly
      negligent acts) or agreed to in writing, shall any Contributor be
      liable to You for damages, including any direct, indirect, special,
      incidental, or consequential damages of any character arising as a
      result of this License or out of the use or inability to use the
      Work (including but not limited to damages for loss of goodwill,
      work stoppage, computer failure or malfunction, or any and all
      other commercial damages or losses), even if such Contributor
      has been advised of the possibility of such damages.

   9. Accepting Warranty or Additional Liability. While redistributing
      the Work or Derivative Works thereof, You may choose to offer,
      and charge a fee for, acceptance of support, warranty, indemnity,
      or other liability obligations and/or rights consistent with this
      License. However, in accepting such obligations, You may act only
      on Your own behalf and on Your sole responsibility, not on behalf
      of any other Contributor, and only if You agree to indemnify,
      defend, and hold each Contributor harmless for any liability
      incurred by, or claims asserted against, such Contributor by reason
      of your accepting any such warranty or additional liability.

   END OF TERMS AND CONDITIONS

   APPENDIX: How to apply the Apache License to your work.

      To apply the Apache License to your work, attach the following
      boilerplate notice, with the fields enclosed by brackets "[]"
      replaced with your own identifying information. (Don't include
      the brackets!)  The text should be enclosed in the appropriate
      comment syntax for the file format. We also recommend that a
      file or class name and description of purpose be included on the
      same "printed page" as the copyright notice for easier
      identification within third-party archives.

   Copyright [yyyy] [name of copyright owner]

   Licensed under the Apache License, Version 2.0 (the "License");
   you may not use this file except in compliance with the License.
   You may obtain a copy of the License at

       http://www.apache.org/licenses/LICENSE-2.0

   Unless required by applicable law or agreed to in writing, software
   distributed under the License is distributed on an "AS IS" BASIS,
   WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
   See the License for the specific language governing permissions and
   limitations under the License.
