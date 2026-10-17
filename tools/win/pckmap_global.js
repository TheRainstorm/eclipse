'use strict';
const ws = Process.findModuleByName('ws2_32.dll');
const mod = Process.findModuleByName('streamer.dll');
if (!ws || !mod) { send({ev:'error',msg:'module missing'}); }
else {
  const gp = new NativeFunction(ws.getExportByName('getpeername'), 'int', ['int','pointer','pointer']);
  const writes=[];
  const seen=new Set();
  function dest(s){
    const sa=Memory.alloc(128), sl=Memory.alloc(4); sl.writeInt(128);
    if(gp(s.toInt32(),sa,sl)!==0) return '?';
    const fam=sa.readU16(); if(fam!==2) return '?';
    const port=(sa.add(2).readU8()<<8)|sa.add(3).readU8();
    return [4,5,6,7].map(i=>sa.add(i).readU8()).join('.')+':'+port;
  }
  function log(s, buf, wire){
    let b0=0; try{b0=buf.readU8();}catch(e){}
    if(b0!==0x17) return;
    const now=Date.now();
    for(let i=writes.length-1;i>=0;i--){
      const w=writes[i]; if(now-w.ts>50) break;
      const diff=wire-w.len; if(diff<5||diff>40) continue;
      const d=dest(s); const key=w.ssl.toString()+'|'+s.toString();
      if(seen.has(key)) continue; seen.add(key);
      send({ev:'map',ssl:w.ssl.toString(),sock:s.toString(),d,plain:w.len,wire,diff,dt:now-w.ts});
      break;
    }
  }
  const W=mod.base.add(0xdd7f30);
  Interceptor.attach(W,{onEnter(a){writes.push({ssl:a[0],len:a[2].toInt32(),ts:Date.now()}); if(writes.length>200) writes.splice(0,writes.length-200);}});
  try{const f=ws.getExportByName('send'); Interceptor.attach(f,{onEnter(a){this.s=a[0];this.b=a[1];},onLeave(r){const n=r.toInt32(); if(n>0)log(this.s,this.b,n);}});}catch(e){}
  try{const f=ws.getExportByName('WSASend'); Interceptor.attach(f,{onEnter(a){this.s=a[0];this.w=a[1];},onLeave(r){if(r.toInt32()===0){const b=this.w.add(8).readPointer(); log(this.s,b,this.w.readU32());}}});}catch(e){}
  send({ev:'hooked'});
}
