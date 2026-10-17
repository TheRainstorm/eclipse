// Capture plaintext at streamer.dll's statically-linked OpenSSL boundary.
// Resolves SSL_write_ex / SSL_read_ex through the adapter wrappers, so it
// survives minor version changes (no hardcoded RVA).
'use strict';

const MAX_CALLS = 400;       // per direction
const HEAD = 96;             // bytes of header to keep for every call
const SMALL_FULL = 4096;     // keep full payload when <= this

function findModule(name) {
  const m = Process.findModuleByName(name);
  if (!m) throw new Error('module not loaded: ' + name);
  return m;
}

function scanUnique(base, size, sig) {
  const hits = Memory.scanSync(base, size, sig);
  if (hits.length !== 1) throw new Error('scan ' + sig + ' -> ' + hits.length + ' hits');
  return hits[0].address;
}

function callTargetAt(p, from, to) {
  for (let i = from; i < to; i++) {
    if (p.add(i).readU8() === 0xE8) {
      const rel = p.add(i + 1).readS32();
      return p.add(i + 5).add(rel);
    }
  }
  throw new Error('no call found in range');
}

function main() {
  const mod = findModule('streamer.dll');
  // OpenSSLAdapter::Write -- this+0x175 flag, this+0x190 SSL*
  const wwrap = scanUnique(mod.base, mod.size,
    'C6 81 75 01 00 00 00 4C 89 C0 48 C1 E8 1F 0F 85');
  // OpenSSLAdapter::Read -- this+0x174 flag, this+0x190 SSL*
  const rwrap = scanUnique(mod.base, mod.size,
    'C6 86 74 01 00 00 00 48 89 D8 48 C1 E8 1F 0F 85');
  // wwrap/rwrap point at the signature *inside* the wrapper (+0x16), not the
  // function entry. The first call after the signature is the OpenSSL one.
  const writeEx = callTargetAt(wwrap, 0x10, 0x30);
  const readEx = callTargetAt(rwrap, 0x10, 0x30);

  function checkSslEx(fn, tag) {
    const b = new Uint8Array(fn.readByteArray(6));
    if (!(b[0] === 0xB8 && b[1] === 0x38 && b[2] === 0 && b[3] === 0 &&
          b[4] === 0 && b[5] === 0xE8)) {
      throw new Error(tag + ' prologue mismatch at ' + fn);
    }
  }
  checkSslEx(writeEx, 'writeEx');
  checkSslEx(readEx, 'readEx');

  send({ ev: 'resolved', base: mod.base.toString(), writeEx: writeEx.toString(), readEx: readEx.toString(),
         writeRva: writeEx.sub(mod.base).toString(), readRva: readEx.sub(mod.base).toString() });

  const stats = { w: 0, r: 0, wb: 0, rb: 0 };
  const t0 = Date.now();

  function emit(dir, ssl, buf, len) {
    if (stats[dir] >= MAX_CALLS) return;
    stats[dir]++;
    if (dir === 'w') stats.wb += len; else stats.rb += len;
    if (len <= 0) return;
    let n = len;
    if (n > HEAD && n > SMALL_FULL) n = HEAD;
    const hex = n > 0 ? buf.readByteArray(n) : null;
    if (hex) {
      send({ ev: 'io', dir, t: Date.now() - t0, ssl: ssl.toString(), len,
             head: Array.from(new Uint8Array(hex)).map(b => b.toString(16).padStart(2, '0')).join('') });
    } else {
      send({ ev: 'io', dir, t: Date.now() - t0, ssl: ssl.toString(), len, head: '' });
    }
  }

  Interceptor.attach(writeEx, {
    onEnter(args) {
      this.ssl = args[0]; this.buf = args[1]; this.len = args[2].toUInt32(); this.out = args[3];
    },
    onLeave(ret) {
      if (ret.toInt32() !== 1) return;
      let wrote = 0;
      try { wrote = this.out.readU64().toNumber(); } catch (e) {}
      if (wrote > 0) emit('w', this.ssl, this.buf, wrote);
    }
  });

  Interceptor.attach(readEx, {
    onEnter(args) {
      this.ssl = args[0]; this.buf = args[1]; this.len = args[2].toUInt32(); this.out = args[3];
    },
    onLeave(ret) {
      if (ret.toInt32() === 1) {
        let got = 0;
        try { got = this.out.readU64().toNumber(); } catch (e) {}
        if (got > 0) emit('r', this.ssl, this.buf, got);
      }
    }
  });

  send({ ev: 'hooked' });
}

setImmediate(main);
