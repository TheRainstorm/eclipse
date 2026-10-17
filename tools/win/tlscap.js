'use strict';
// Capture the TLS handshake of the PCK control channel to the controlled host.
const ws = Process.findModuleByName('ws2_32.dll');
if (!ws) { send({ev:'error',msg:'no ws2_32'}); } else {
  const dest = new Map();
  function addr(p) {
    try {
      const fam = p.readU16();
      if (fam === 2) {
        const port = (p.add(2).readU8() << 8) | p.add(3).readU8();
        return [4,5,6,7].map(i=>p.add(i).readU8()).join('.') + ':' + port;
      }
      if (fam === 23) {
        const port = (p.add(2).readU8() << 8) | p.add(3).readU8();
        const b = new Uint8Array(p.add(8).readByteArray(16));
        return '[' + Array.from({length:8},(_,i)=>((b[i*2]<<8)|b[i*2+1]).toString(16)).join(':') + ']:' + port;
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
  const count = new Map();
  function log(kind, s, buf, len) {
    if (len<=0) return;
    const d = dest.get(s.toString()) || '?';
    if (!d.startsWith('192.168.35.6')) return;
    const n = count.get(s.toString()) || 0;
    if (n > 8) return;
    count.set(s.toString(), n+1);
    const k=Math.min(len,4096);
    let hex=''; try{hex=Array.from(new Uint8Array(buf.readByteArray(k))).map(x=>x.toString(16).padStart(2,'0')).join('');}catch(e){}
    send({ev:'raw',kind,s:s.toString(),d,len,hex});
  }
  for (const [name, kind] of [['send','send'],['recv','recv']]) {
    try { const f=ws.getExportByName(name); Interceptor.attach(f,{onEnter(a){this.s=a[0];this.b=a[1];},onLeave(r){log(kind,this.s,this.b,r.toInt32());}}); } catch(e) {}
  }
  try { const f=ws.getExportByName('WSASend'); Interceptor.attach(f,{onEnter(a){this.s=a[0];this.w=a[1];},onLeave(r){if(r.toInt32()===0) log('wsasend',this.s,this.w.add(8).readPointer(),this.w.readU32());}}); } catch(e) {}
  try { const f=ws.getExportByName('WSARecv'); Interceptor.attach(f,{onEnter(a){this.s=a[0];this.w=a[1];this.lp=ptr(a[3]);},onLeave(r){if(r.toInt32()===0) log('wsarecv',this.s,this.w.add(8).readPointer(),this.lp.readU32());}}); } catch(e) {}
  send({ev:'hooked'});
}
