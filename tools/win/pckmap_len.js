'use strict';
const ws = Process.findModuleByName('ws2_32.dll');
const mod = Process.findModuleByName('streamer.dll');
if (!ws || !mod) { send({ev:'error',msg:'module missing'}); }
else {
  const gp = new NativeFunction(ws.getExportByName('getpeername'), 'int', ['int','pointer','pointer']);
  const last = new Map();
  const seen = new Set();
  function dest(s) {
    const sa=Memory.alloc(128), sl=Memory.alloc(4); sl.writeInt(128);
    if (gp(s.toInt32(),sa,sl)!==0) return '?';
    const fam=sa.readU16();
    if (fam!==2) return '?';
    const port=(sa.add(2).readU8()<<8)|sa.add(3).readU8();
    return [4,5,6,7].map(i=>sa.add(i).readU8()).join('.')+':'+port;
  }
  function log(s, wire) {
    const tid=Process.getCurrentThreadId();
    const l=last.get(tid); if(!l) return;
    const dt=Date.now()-l.ts; if(dt>50) return;
    const diff=wire-l.len;
    if(diff<5 || diff>40) return;
    const d=dest(s);
    const key=l.ssl.toString()+'|'+s.toString();
    if(seen.has(key)) return; seen.add(key);
    send({ev:'map',ssl:l.ssl.toString(),sock:s.toString(),d,plain:l.len,wire,diff,dt});
  }
  const W=mod.base.add(0xdd7f30);
  Interceptor.attach(W,{onEnter(a){last.set(Process.getCurrentThreadId(),{ssl:a[0],len:a[2].toInt32(),ts:Date.now()});}});
  for (const name of ['send','recv']) {
    try{const f=ws.getExportByName(name); Interceptor.attach(f,{onEnter(a){this.s=a[0];},onLeave(r){const n=r.toInt32(); if(n>0)log(this.s,n);}});}catch(e){}
  }
  try{const f=ws.getExportByName('WSASend'); Interceptor.attach(f,{onEnter(a){this.s=a[0];this.w=a[1];},onLeave(r){if(r.toInt32()===0)log(this.s,this.w.readU32());}});}catch(e){}
  try{const f=ws.getExportByName('WSARecv'); Interceptor.attach(f,{onEnter(a){this.s=a[0];this.w=a[1];this.lp=ptr(a[3]);},onLeave(r){if(r.toInt32()===0)log(this.s,this.lp.readU32());}});}catch(e){}
  send({ev:'hooked'});
}
