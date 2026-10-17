'use strict';
// Capture PCK plaintext and map each SSL* to its TCP destination.
const ws = Process.findModuleByName('ws2_32.dll');
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

const last = new Map(); // tid -> {ssl, len, ts, isPck}
function note(ssl, buf, len) {
  let isPck = false;
  try {
    const b = new Uint8Array(buf.readByteArray(Math.min(len, 4)));
    isPck = b[0]===0x50 && b[1]===0x43 && b[2]===0x4b; // "PCK"
  } catch(e) {}
  last.set(Process.getCurrentThreadId(), {ssl, len, ts: Date.now(), isPck});
}
function check(sock, wireLen) {
  const tid = Process.getCurrentThreadId();
  const l = last.get(tid);
  if (!l || Date.now() - l.ts > 50) return;
  const d = dest.get(sock.toString());
  if (!d) return;
  const key = l.ssl.toString() + '|' + d;
  if (!check.seen) check.seen = new Set();
  if (check.seen.has(key)) return;
  check.seen.add(key);
  send({ev:'map', ssl:l.ssl.toString(), d, plain:l.len, wire:wireLen, pck:l.isPck});
}

const mod = Process.findModuleByName('streamer.dll');
if (!mod) { send({ev:'error',msg:'streamer.dll not loaded'}); }
else {
  const W = mod.base.add(0xdd7f30);
  const R = mod.base.add(0xdd7690);
  Interceptor.attach(W, {
    onEnter(a){ this.s=a[0]; this.b=a[1]; this.n=a[2].toInt32(); },
    onLeave(r){ const n=r.toInt32(); if(n>0){ note(this.s,this.b,this.n); try{ const b=new Uint8Array(this.b.readByteArray(Math.min(n,4))); if(b[0]===0x50&&b[1]===0x43&&b[2]===0x4b){ send({ev:'pck',d:'w',ssl:this.s.toString(),len:n,hex:Array.from(new Uint8Array(this.b.readByteArray(Math.min(n,160)))).map(x=>x.toString(16).padStart(2,'0')).join('')}); } }catch(e){} } }
  });
  Interceptor.attach(R, {
    onEnter(a){ this.s=a[0]; this.b=a[1]; },
    onLeave(r){ const n=r.toInt32(); if(n>0){ note(this.s,this.b,n); try{ const b=new Uint8Array(this.b.readByteArray(Math.min(n,4))); if(b[0]===0x50&&b[1]===0x43&&b[2]===0x4b){ send({ev:'pck',d:'r',ssl:this.s.toString(),len:n,hex:Array.from(new Uint8Array(this.b.readByteArray(Math.min(n,160)))).map(x=>x.toString(16).padStart(2,'0')).join('')}); } }catch(e){} } }
  });
  for (const [name, kind] of [['send','send'],['recv','recv']]) {
    try { const f=ws.getExportByName(name); Interceptor.attach(f,{onEnter(a){this.s=a[0];},onLeave(r){const n=r.toInt32(); if(n>0) check(this.s,n);}}); } catch(e) {}
  }
  try { const f=ws.getExportByName('WSASend'); Interceptor.attach(f,{onEnter(a){this.s=a[0];},onLeave(r){if(r.toInt32()===0) check(this.s,0);}}); } catch(e) {}
  try { const f=ws.getExportByName('WSARecv'); Interceptor.attach(f,{onEnter(a){this.s=a[0];},onLeave(r){if(r.toInt32()===0) check(this.s,0);}}); } catch(e) {}
  send({ev:'hooked'});
}
