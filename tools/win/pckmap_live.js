'use strict';
const ws = Process.findModuleByName('ws2_32.dll');
const mod = Process.findModuleByName('streamer.dll');
if (!ws || !mod) { send({ev:'error',msg:'module missing'}); }
else {
  const getpeername = new NativeFunction(ws.getExportByName('getpeername'), 'int', ['int','pointer','pointer']);
  const last = new Map();
  const seen = new Set();
  function log(s) {
    const tid = Process.getCurrentThreadId();
    const ssl = last.get(tid); if (!ssl) return;
    const key = ssl.toString() + '|' + s.toString();
    if (seen.has(key)) return; seen.add(key);
    const sa = Memory.alloc(128); const sl = Memory.alloc(4); sl.writeInt(128);
    let d = '?';
    if (getpeername(s.toInt32(), sa, sl) === 0) {
      const fam = sa.readU16();
      if (fam === 2) {
        const port = (sa.add(2).readU8()<<8)|sa.add(3).readU8();
        d = [4,5,6,7].map(i=>sa.add(i).readU8()).join('.')+':'+port;
      }
    }
    send({ev:'map', ssl:ssl.toString(), sock:s.toString(), d});
  }
  const W = mod.base.add(0xdd7f30);
  Interceptor.attach(W,{onEnter(a){last.set(Process.getCurrentThreadId(),a[0]);}});
  for (const name of ['send','recv']) {
    try { const f=ws.getExportByName(name); Interceptor.attach(f,{onEnter(a){this.s=a[0];},onLeave(r){if(r.toInt32()>0)log(this.s);}}); } catch(e){}
  }
  try { const f=ws.getExportByName('WSASend'); Interceptor.attach(f,{onEnter(a){this.s=a[0];},onLeave(r){if(r.toInt32()===0)log(this.s);}}); } catch(e){}
  try { const f=ws.getExportByName('WSARecv'); Interceptor.attach(f,{onEnter(a){this.s=a[0];},onLeave(r){if(r.toInt32()===0)log(this.s);}}); } catch(e){}
  send({ev:'hooked'});
}
