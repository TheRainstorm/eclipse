'use strict';
const MAX_CALLS = 600, HEAD = 128, SMALL_FULL = 4096;
const mod = Process.findModuleByName('libssl-3-x64.dll');
if (!mod) { send({ev:'error', msg:'libssl-3-x64.dll not loaded'}); }
else {
  const resolved = {};
  function ex(name) {
    try { return mod.getExportByName(name); } catch (e) { return null; }
  }
  for (const n of ['SSL_write_ex','SSL_read_ex','SSL_write','SSL_read']) {
    const a = ex(n); if (a) resolved[n] = a.toString();
  }
  send({ev:'resolved', mod: mod.name, base: mod.base.toString(), funcs: resolved});

  const stats = {w:0,r:0};
  const t0 = Date.now();
  function emit(dir, ssl, buf, len) {
    if (len <= 0 || stats[dir] >= MAX_CALLS) return;
    stats[dir]++;
    const n = (len > HEAD && len > SMALL_FULL) ? HEAD : len;
    let s = '';
    try { s = Array.from(new Uint8Array(buf.readByteArray(n))).map(b=>b.toString(16).padStart(2,'0')).join(''); } catch(e) {}
    send({ev:'io', dir, t: Date.now()-t0, ssl: ssl.toString(), len, head: s});
  }

  function hookEx(name, dir) {
    const a = ex(name); if (!a) return;
    Interceptor.attach(a, {
      onEnter(args) { this.ssl=args[0]; this.buf=args[1]; this.len=args[2].toUInt32(); this.out=args[3]; },
      onLeave(ret) {
        if (ret.toInt32() !== 1) return;
        let n=0; try { n=this.out.readU64().toNumber(); } catch(e){}
        emit(dir, this.ssl, this.buf, n);
      }
    });
    send({ev:'hooked', name, dir});
  }
  function hookOld(name, dir) {
    const a = ex(name); if (!a) return;
    Interceptor.attach(a, {
      onEnter(args) { this.ssl=args[0]; this.buf=args[1]; this.len=args[2].toInt32(); },
      onLeave(ret) {
        const n = ret.toInt32(); if (n <= 0) return;
        emit(dir, this.ssl, this.buf, n);
      }
    });
    send({ev:'hooked', name, dir});
  }
  hookEx('SSL_write_ex','w'); hookEx('SSL_read_ex','r');
  hookOld('SSL_write','w'); hookOld('SSL_read','r');
}
