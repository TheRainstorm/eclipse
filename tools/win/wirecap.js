'use strict';
// Plaintext boundary: streamer.dll SSL_write/SSL_read implementations.
// Both return the byte count in rax; buffer is arg1.
const m = Process.findModuleByName('streamer.dll');
if (!m) { send({ev:'error',msg:'no streamer.dll'}); }
else {
  const W = m.base.add(0xdd7f30), R = m.base.add(0xdd7690);
  const MAX = 6000;
  let nw=0, nr=0; const t0=Date.now();
  function hex(p,n){ try{return Array.from(new Uint8Array(p.readByteArray(n))).map(b=>b.toString(16).padStart(2,'0')).join('');}catch(e){return 'ERR';} }
  function emit(d,ssl,buf,n){
    if(n<=0) return;
    if(d==='w'? nw++>=MAX : nr++>=MAX) return;
    const keep = n<=2048 ? n : 128;
    send({ev:'io',d,t:Date.now()-t0,ssl:ssl.toString(),n,hex:hex(buf,keep)});
  }
  Interceptor.attach(W,{onEnter(a){this.s=a[0];this.b=a[1];},onLeave(r){const n=r.toInt32(); if(n>0) emit('w',this.s,this.b,n);}});
  Interceptor.attach(R,{onEnter(a){this.s=a[0];this.b=a[1];},onLeave(r){const n=r.toInt32(); if(n>0) emit('r',this.s,this.b,n);}});
  send({ev:'hooked',W:W.toString(),R:R.toString()});
}
