'use strict';
// Hook streamer.dll's TLS plaintext boundary:
//   0xCA36E write(ctx, buf, len) -> vtable+0x28
//   0xCA3EE read (ctx, buf, len) -> vtable+0x38
// Signatures are anchored on the vtable offset so they don't depend on build RVA.
const MAX = 1200, HEAD = 160, SMALL_FULL = 4096;
const mod = Process.findModuleByName('streamer.dll');
if (!mod) { send({ev:'error', msg:'streamer.dll not loaded'}); }
else {
  const W = '48 85 D2 74 ?? 44 89 C5 48 89 D3 48 89 CE E8 ?? ?? ?? ?? 48 89 C7 48 89 F1 BA 0F 00 00 00 E8 ?? ?? ?? ?? 4C 63 C5 48 8B 07 48 8B 40 28';
  const R = '48 85 D2 74 ?? 44 89 C5 48 89 D3 48 89 CE E8 ?? ?? ?? ?? 48 89 C7 48 89 F1 BA 0F 00 00 00 E8 ?? ?? ?? ?? 4C 63 C5 48 8B 07 48 8B 40 38';
  function one(sig, tag) {
    const h = Memory.scanSync(mod.base, mod.size, sig);
    if (h.length !== 1) throw new Error(tag + ' scan -> ' + h.length);
    return h[0].address;
  }
  let w, r;
  try {
    // Signatures start 8 bytes into the function (after push rsi/rdi/rbp/rbx + sub rsp,0x28).
    w = one(W,'write').sub(8);
    r = one(R,'read').sub(8);
    const wp = w.readByteArray(8), rp = r.readByteArray(8);
    const want = [0x56,0x57,0x55,0x53,0x48,0x83,0xEC,0x28];
    const same = (b) => want.every((v,i) => new Uint8Array(b)[i] === v);
    if (!same(wp) || !same(rp)) throw new Error('prologue mismatch');
  }
  catch(e) { send({ev:'error', msg:e.message}); throw e; }
  send({ev:'resolved', base:mod.base.toString(), write:w.toString(), read:r.toString(),
        writeRva:w.sub(mod.base).toString(), readRva:r.sub(mod.base).toString()});

  let nw=0, nr=0; const t0=Date.now();
  function emit(dir, ctx, buf, len) {
    if (len <= 0) return;
    if (dir==='w' ? nw++ >= MAX : nr++ >= MAX) return;
    const n = (len > HEAD && len > SMALL_FULL) ? HEAD : len;
    let hex=''; try { hex = Array.from(new Uint8Array(buf.readByteArray(n))).map(b=>b.toString(16).padStart(2,'0')).join(''); } catch(e){}
    send({ev:'io', dir, t:Date.now()-t0, ctx:ctx.toString(), len, hex});
  }
  Interceptor.attach(w, {
    onEnter(a){ this.ctx=a[0]; this.buf=a[1]; this.len=a[2].toInt32(); },
    onLeave(ret){ const n=ret.toInt32(); if(n>0) emit('w',this.ctx,this.buf,n); }
  });
  Interceptor.attach(r, {
    onEnter(a){ this.ctx=a[0]; this.buf=a[1]; this.len=a[2].toInt32(); },
    onLeave(ret){ const n=ret.toInt32(); if(n>0) emit('r',this.ctx,this.buf,n); }
  });
  send({ev:'hooked'});
}
