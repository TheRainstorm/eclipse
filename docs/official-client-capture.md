# 官方客户端（GameViewer.exe）明文抓包方法

最后更新：2026-10-18

> 目的：抓取官方主控端与官方被控端之间的**应用层明文**（PCK 帧），
> 用于逆向协议（例如码率/画质控制报文）。本文是操作手册，照做即可，避免重复踩坑。

## 1. 一句话

给 Windows 测试客户端 上的 `GameViewer.exe` 挂 Frida，钩 **`streamer.dll` 内联 OpenSSL 的写/读函数**
（RVA `0xdd7f30` / `0xdd7690`），即可拿到**明文 PCK 帧**；脚本用 `Win32_Process.Create`
脱离 SSH 会话启动，否则进程会被 SSH 断开杀掉。

## 2. 关键认知（踩过的坑）

| 现象 / 做法 | 结论 |
|---|---|
| 钩 `libssl-3-x64.dll` 的导出 `SSL_write` | ❌ **抓不到**。官方数据走 streamer.dll **静态链接**的 OpenSSL，不经过 libssl 导出。 |
| 钩 `plaincap.js`（pattern 命中 vtable+0x28/+0x38 的函数） | ❌ 拿到的是 **TLS 密文**（能看到 `17 03 03 00 41` 记录头）。这是 socket 层，不是明文。 |
| 钩 `streamer.dll` RVA `0xdd7f30`/`0xdd7690`（`wirecap.js`/`wire2.js`） | ✅ **明文 PCK 帧**。这两个是 streamer 内部的 SSL 写/读包装，返回值是字节数，buf 是 arg1。 |
| `b.find(b'PCK')` 全缓冲扫描 | ❌ 二进制里到处是假 `PCK`（50 43 4b），产生海量噪声。**只认缓冲区开头就是 `PCK`**。 |
| `Start-Process` 启动抓包 | ❌ 子进程随 SSH 会话结束被杀。用 `Win32_Process.Create`（见 §5）。 |
| Frida 以 `SYSTEM` 计划任务附加 | ❌ 附加会话进程失败。用**交互式用户令牌**（`New-ScheduledTaskPrincipal -LogonType Interactive`）或 `Win32_Process.Create` 即可。 |

## 3. 前置

- 目标机 **Windows 测试客户端**（`ssh -p 22 USER@192.0.2.10`）。
- 官方客户端进程名 `GameViewer.exe`，路径 `D:\Programs\GameViewer\bin\GameViewer.exe`。
- Python：`C:\Users\USER\AppData\Local\Programs\Python\Python311\python.exe`（已装 `frida` 17.24）。
- 需要的文件（已在 Windows 测试客户端 `C:\Users\USER\`）：
  - `wire2.js` / `wire2.py`（当前推荐；`wirecap.js/.py` 是旧版，`wirecap.js` 里 hex 有 `keep` 变量 bug）
  - `run_wire2b.bat`（把 python 包一层，便于脱离启动）

## 4. 脚本内容

### `wire2.js`

```js
'use strict';
const m = Process.findModuleByName('streamer.dll');
const W = m.base.add(0xdd7f30), R = m.base.add(0xdd7690);
const MAX=6000, HEAD=220, FULL=4096;
let nw=0,nr=0; const t0=Date.now();
function hex(p,n){ try{return Array.from(new Uint8Array(p.readByteArray(n))).map(b=>b.toString(16).padStart(2,'0')).join('');}catch(e){return 'ERR';} }
function emit(d,ssl,buf,len){
  if(d==='w'? (nw++>=MAX):(nr++>=MAX)) return;
  const n = (len>HEAD && len>FULL)?HEAD:len;
  send({ev:'io',d,t:Date.now()-t0,ssl:ssl.toString(),n:len,hex:hex(buf,n)});
}
Interceptor.attach(W,{onEnter(a){this.s=a[0];this.b=a[1];this.l=a[2].toInt32();},onLeave(r){const n=r.toInt32(); if(n>0) emit('w',this.s,this.b,n);}});
Interceptor.attach(R,{onEnter(a){this.s=a[0];this.b=a[1];this.l=a[2].toInt32();},onLeave(r){const n=r.toInt32(); if(n>0) emit('r',this.s,this.b,n);}});
send({ev:'hooked',W:W.toString(),R:R.toString()});
```

### `wire2.py`

```python
import frida,sys,time,json
pid=int(sys.argv[1]); secs=float(sys.argv[2]) if len(sys.argv)>2 else 20
out=sys.argv[3] if len(sys.argv)>3 else r'C:\Users\USER\wire2.jsonl'
s=frida.get_local_device().attach(pid)
sc=s.create_script(open(r'C:\Users\USER\wire2.js').read())
f=open(out,'w',encoding='utf-8')
def m(x,d):
    if x.get('type')=='error': print('ERR',x,flush=True); return
    p=x.get('payload'); f.write(json.dumps(p)+'\n'); f.flush()
    if p.get('ev')!='io': print(p,flush=True)
sc.on('message',m); sc.load(); print('attached',pid,flush=True); time.sleep(secs)
try: sc.unload()
finally: f.close(); s.detach()
print('done',flush=True)
```

### `run_wire2b.bat`（把 pid/秒数/输出写死）

```bat
@echo off
"C:\Users\USER\AppData\Local\Programs\Python\Python311\python.exe" C:\Users\USER\wire2.py <PID> 300 > C:\Users\USER\wire2b_out.txt 2>&1
```

## 5. 脱离 SSH 会话启动（关键）

```powershell
# 1) 取当前 GameViewer pid，写进 .cmd
$pid2 = (Get-Process GameViewer | Select-Object -First 1).Id
# （或直接改 run_wire2b.bat 里的 <PID>）

# 2) 用 WMI 创建进程 —— 不随 SSH 会话结束而死
Invoke-CimMethod -ClassName Win32_Process -MethodName Create `
  -Arguments @{CommandLine='cmd.exe /c C:\Users\USER\run_wire2b.bat'}
```

> 备选：`New-ScheduledTaskPrincipal -LogonType Interactive` 注册计划任务再 `Start-ScheduledTask`。
> **不要**用 `/RU SYSTEM`（Frida 附加失败）。

## 6. PCK 帧格式（明文）

```
b[0:3]   = "PCK"          魔数
b[8]     = 0x51           应用层帧类型（0x52 是另一种框架，偏移不同，先忽略）
b[9:13]  = seq            big-endian
b[17:19] = channel        little-endian
b[20:24] = kind           little-endian
b[28:]   = payload        JSON（输入）或 protobuf（控制类）
```

- 输入示例（channel 会变，kind 7/8/15/16 等）：
  - `{"action":"mouse_move_absolute","abs_x":0.52,"abs_y":0.06,"screen_id":1}`
  - `{"action":"mouse_move_relative","delta_x":..,"delta_y":..,"mousetype":2}`
  - `{"heartbeat":"kbd_heartbeat"}`
  - `{"action":"kbd_press","interrept":"i","key":72,"kseq":9}`
- payload 尾部常见 `03 00 01 00`。
- 非 PCK 的小帧（如 `81 01 32` / `81 81 99 00 f4 0b aa`）是**保活帧**，不是应用数据。

## 7. 分析片段

```python
import json
g={}
for line in open(r'C:\Users\USER\wire2.jsonl',errors='ignore'):
    x=json.loads(line)
    if x.get('ev')!='io': continue
    b=bytes.fromhex(x.get('hex',''))
    if len(b)<28 or b[:3]!=b'PCK' or b[8]!=0x51: continue
    ch=int.from_bytes(b[17:19],'little'); kind=int.from_bytes(b[20:24],'little')
    g.setdefault((ch,kind),[]).append(b[28:])
for k,v in sorted(g.items(), key=lambda kv:-len(kv[1])):
    s=v[0]; print(k, len(v), ''.join(chr(c) if 32<=c<127 else '.' for c in s[:110]))
```

## 8. 抓"动作相关"报文的注意点

- **必须让官方客户端正在串流**，并在抓包窗口内**实际操作**（动鼠标 / 切码率），
  否则只会抓到保活帧。判据：`wire2.jsonl` 里出现 `PCK` 开头的帧（`50434b`）。
- 已有 4 份历史抓包（`/tmp/wirecap*.jsonl`、`/tmp/pcksniff*.jsonl`、`/tmp/pckcap.jsonl`，
  均在 ares）**只含输入 JSON**，从未包含码率/画质控制报文。

## 9. 若 streamer.dll 换版本（RVA 失效）

用 pattern 扫描定位，不依赖硬编码 RVA：

```js
// OpenSSLAdapter::Write / Read 包装（sslcapture.js 用的签名）
const wwrap = Memory.scanSync(mod.base, mod.size, 'C6 81 75 01 00 00 00 4C 89 C0 48 C1 E8 1F 0F 85')[0].address;
const rwrap = Memory.scanSync(mod.base, mod.size, 'C6 86 74 01 00 00 00 48 89 D8 48 C1 E8 1F 0F 85')[0].address;
// 包装体内 +0x10..+0x30 的第一个 E8(call) 目标即 SSL_write/read_ex
function firstCall(p, from, to){ for(let i=from;i<to;i++){ if(p.add(i).readU8()===0xE8) return p.add(i+5).add(p.add(i+1).readS32()); } }
const writeEx = firstCall(wwrap, 0x10, 0x30);   // 序言应为 B8 38 00 00 00 E8
const readEx  = firstCall(rwrap, 0x10, 0x30);
```

当前版本（streamer.dll size `20500480`）解析结果：`writeRva=0xdd7f30`、`readRva=0xdd7690`，
序言均为 `b8 38 00 00 00 e8`（带栈探测 `mov eax,0x38; call __chkstk; sub rsp,rax`）。

## 10. 文件清单

| 位置 | 文件 |
|---|---|
| Windows 测试客户端 `C:\Users\USER\` | `wire2.js` `wire2.py` `run_wire2b.bat`（当前推荐） |
| Windows 测试客户端 `C:\Users\USER\` | `wirecap.js` `wirecap.py`（旧版，js 有 hex bug）、`plaincap.*`、`sslcapture*.*`、`startwire*.ps1` |
| ares `/tmp` | `wirecap.jsonl` `wirecap2.jsonl` `wirecap4.jsonl` `wirecap5.jsonl` `pcksniff.jsonl` `pcksniff2.jsonl` `pckcap.jsonl` `startwire*.ps1` |
| 参考 | codex 会话 `~/.codex/sessions/2026/10/17/rollout-2026-10-17T20-54-18-*.jsonl`（含当年定位过程） |
