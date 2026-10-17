'use strict';
// Map new TCP connections to destinations and dump the first bytes / TLS handshake.
const ws = Process.findModuleByName('ws2_32.dll');
if (!ws) { send({ev:'error',msg:'no ws2_32'}); } else {
  const dest = new Map();
  function addr(p) {
    try {
      const fam = p.readU16();
      if (fam === 2) {
        const port = (p.add(2).readU8() << 8) | p.add(3).readU8();
        const ip = [4,5,6,7].map(i=>p.add(i).readU8()).join('.');
        return ip+':'+port;
      }
      if (fam === 23) { // AF_INET6
        const port = (p.add(2).readU8() << 8) | p.add(3).readU8();
        const b = new Uint8Array(p.add(8).readByteArray(16));
        const ip = Array.from({length:8},(_,i)=>((b[i*2]<<8)|b[i*2+1]).toString(16)).join(':');
        return '['+ip+']:'+port;
      }
    } catch(e) {}
    return null;
  }
  for (const [name, idx] of [['connect',1],['WSAConnect',1],['ConnectEx',1]]) {
    try {
      const f = ws.getExportByName(name);
      Interceptor.attach(f, { onEnter(a){ const d=addr(a[idx]); if(d){ dest.set(a[0].toString(), d); send({ev:'connect',s:a[0].toString(),d}); } } });
    } catch(e) {}
  }
  const seen = new Map();
  function log(kind, s, buf, len) {
    if (len<=0) return;
    const d = dest.get(s.toString()) || '?';
    const n = (seen.get(d)||0);
    let b0=0; try{b0=buf.readU8();}catch(e){}
    if (n>60 && b0!==0x16) return;
    seen.set(d, n+1);
    const k=Math.min(len, 512);
    let hex=''; try{hex=Array.from(new Uint8Array(buf.readByteArray(k))).map(x=>x.toString(16).padStart(2,'0')).join('');}catch(e){}
    send({ev:'raw',kind,d,len,hex});
  }
  for (const [name, kind] of [['send','send'],['recv','recv']]) {
    try {
      const f = ws.getExportByName(name);
      Interceptor.attach(f,{onEnter(a){this.s=a[0];this.b=a[1];},onLeave(r){log(kind,this.s,this.b,r.toInt32());}});
    } catch(e) {}
  }
  try {
    const f = ws.getExportByName('WSASend');
    Interceptor.attach(f,{onEnter(a){this.s=a[0];this.w=a[1];},onLeave(r){if(r.toInt32()!==0)return;log('wsasend',this.s,this.w.add(8).readPointer(),this.w.readU32());}});
  } catch(e) {}
  try {
    const f = ws.getExportByName('WSARecv');
    Interceptor.attach(f,{onEnter(a){this.s=a[0];this.w=a[1];this.lp=ptr(a[3]);},onLeave(r){if(r.toInt32()!==0)return;log('wsarecv',this.s,this.w.add(8).readPointer(),this.lp.readU32());}});
  } catch(e) {}
  send({ev:'hooked'});
}
