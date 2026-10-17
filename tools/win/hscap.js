'use strict';
// Capture TLS handshake + raw TCP bytes, mapping sockets to destinations.
const ws = Process.findModuleByName('ws2_32.dll');
const dest = new Map();
function addr(ptr) {
  try {
    const fam = ptr.readU16();
    if (fam !== 2) return null;
    const port = (ptr.add(2).readU8() << 8) | ptr.add(3).readU8();
    const ip = [ptr.add(4).readU8(),ptr.add(5).readU8(),ptr.add(6).readU8(),ptr.add(7).readU8()].join('.');
    return ip+':'+port;
  } catch(e) { return null; }
}
const c = ws.getExportByName('connect');
Interceptor.attach(c, { onEnter(a){ const d=addr(a[1]); if(d) dest.set(a[0].toString(), d); } });
let n=0;
function hook(name, cb) {
  let f; try { f=ws.getExportByName(name); } catch(e){ return; }
  Interceptor.attach(f, cb);
}
function log(kind, s, buf, len) {
  if (len<=0 || n>200) return;
  let first=0; try{ first=buf.readU8(); }catch(e){}
  const d = dest.get(s.toString()) || '?';
  if (first!==0x16 && len<40) return; // keep handshakes and meaningful payloads
  n++;
  const k=Math.min(len, 4096);
  let hex=''; try{ hex=Array.from(new Uint8Array(buf.readByteArray(k))).map(b=>b.toString(16).padStart(2,'0')).join(''); }catch(e){}
  send({ev:'raw',kind,d,len,hex});
}
hook('send',{onEnter(a){this.s=a[0];this.b=a[1];},onLeave(r){log('send',this.s,this.b,r.toInt32());}});
hook('recv',{onEnter(a){this.s=a[0];this.b=a[1];},onLeave(r){log('recv',this.s,this.b,r.toInt32());}});
hook('WSASend',{onEnter(a){this.s=a[0];this.w=a[1];},onLeave(r){if(r.toInt32()!==0)return; const l=this.w.readU32(); log('wsasend',this.s,this.w.add(8).readPointer(),l);}});
hook('WSARecv',{onEnter(a){this.s=a[0];this.w=a[1];this.lp=ptr(a[3]);},onLeave(r){if(r.toInt32()!==0)return; const l=this.lp.readU32(); log('wsarecv',this.s,this.w.add(8).readPointer(),l);}});
send({ev:'hooked'});
