'use strict';
const ws = Process.findModuleByName('ws2_32.dll');
if (!ws) { send({ev:'error',msg:'no ws2_32'}); } else {
  const fns = {};
  for (const n of ['send','recv','WSASend','WSARecv']) {
    try { fns[n] = ws.getExportByName(n); } catch(e) {}
  }
  send({ev:'funcs', names:Object.keys(fns).map(k=>[k,fns[k].toString()])});
  const seen = new Set();
  let total = 0;
  function bt(ctx) {
    try {
      return Thread.backtrace(ctx, Backtracer.ACCURATE).slice(0,9).map(a => {
        try { const s = DebugSymbol.fromAddress(a); const m = Process.findModuleByAddress(a);
          return (m ? m.name : (s.moduleName||'?')) + '+' + (m ? a.sub(m.base).toString(16) : '?') + (s.name ? ' ' + s.name : '');
        } catch(e) { return '?'; }
      });
    } catch(e) { return ['btfail:'+e.message]; }
  }
  function emit(kind, fd, buf, len, extra) {
    if (len <= 0 || total > 300) return;
    total++;
    const n = Math.min(len, 24);
    let hex=''; try { hex = Array.from(new Uint8Array(buf.readByteArray(n))).map(b=>b.toString(16).padStart(2,'0')).join(''); } catch(e){}
    const key = kind+':'+hex.slice(0,8);
    const first = !seen.has(key); seen.add(key);
    send({ev:'io', kind, fd: fd.toString(), len, hex, first, stack: first ? bt(this.context) : undefined, extra});
  }
  if (fns.send) Interceptor.attach(fns.send, {
    onEnter(a){ this.fd=a[0]; this.buf=a[1]; this.len=a[2].toInt32(); },
    onLeave(r){ const n=r.toInt32(); if(n>0) emit.call(this,'send',this.fd,this.buf,this.len); }
  });
  if (fns.recv) Interceptor.attach(fns.recv, {
    onEnter(a){ this.fd=a[0]; this.buf=a[1]; this.len=a[2].toInt32(); },
    onLeave(r){ const n=r.toInt32(); if(n>0) emit.call(this,'recv',this.fd,this.buf,this.len); }
  });
  if (fns.WSASend) Interceptor.attach(fns.WSASend, {
    onEnter(a){ this.fd=a[0]; this.wsabuf=a[1]; this.cnt=a[2].toInt32(); },
    onLeave(r){ if(r.toInt32()!==0) return; try { const l=this.wsabuf.readU32(); const b=this.wsabuf.add(8).readPointer(); emit.call(this,'wsasend',this.fd,b,l); } catch(e){} }
  });
  if (fns.WSARecv) Interceptor.attach(fns.WSARecv, {
    onEnter(a){ this.fd=a[0]; this.wsabuf=a[1]; this.lp=ptr(a[3]); },
    onLeave(r){ if(r.toInt32()!==0) return; try { const l=this.lp.readU32(); const b=this.wsabuf.add(8).readPointer(); if(l>0) emit.call(this,'wsarecv',this.fd,b,l); } catch(e){} }
  });
  send({ev:'hooked'});
}
