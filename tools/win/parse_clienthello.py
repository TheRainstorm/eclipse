#!/usr/bin/env python3
"""Parse a TLS ClientHello from a hex string / tlscap JSONL."""
import json, struct, sys

def parse(hexstr):
    b = bytes.fromhex(hexstr)
    if len(b) < 5 or b[0] != 0x16:
        return {'error': 'not a TLS handshake record', 'first': b[:4].hex()}
    rec_len = struct.unpack('>H', b[3:5])[0]
    hs = b[5:5+rec_len]
    if len(hs) < 4 or hs[0] != 1:
        return {'error': 'not a ClientHello', 'type': hs[0] if hs else None}
    p = 4
    ver = hs[p:p+2].hex(); p += 2
    p += 32  # random
    sid_len = hs[p]; p += 1 + sid_len
    cs_len = struct.unpack('>H', hs[p:p+2])[0]; p += 2
    cs = [hs[p+i:p+i+2].hex() for i in range(0, cs_len, 2)]; p += cs_len
    comp_len = hs[p]; p += 1 + comp_len
    out = {'version': ver, 'ciphers': cs, 'sni': None, 'alpn': None,
           'supported_versions': [], 'extensions': []}
    if p + 2 > len(hs):
        return out
    ext_total = struct.unpack('>H', hs[p:p+2])[0]; p += 2
    end = min(len(hs), p + ext_total)
    while p + 4 <= end:
        et, el = struct.unpack('>HH', hs[p:p+4]); p += 4
        data = hs[p:p+el]; p += el
        out['extensions'].append(hex(et))
        if et == 0x0000 and len(data) >= 5:
            n = struct.unpack('>H', data[3:5])[0]
            out['sni'] = data[5:5+n].decode('ascii', 'replace')
        elif et == 0x0010 and len(data) >= 2:
            alpn = []
            q = 2
            while q < len(data):
                n = data[q]; q += 1
                alpn.append(data[q:q+n].decode('ascii', 'replace')); q += n
            out['alpn'] = alpn
        elif et == 0x002b:
            q = 1
            while q + 1 < len(data):
                out['supported_versions'].append(hex(struct.unpack('>H', data[q:q+2])[0])); q += 2
    return out

if __name__ == '__main__':
    if len(sys.argv) > 1 and sys.argv[1].endswith('.jsonl'):
        for line in open(sys.argv[1]):
            r = json.loads(line)
            if r.get('ev') == 'raw' and r.get('hex', '').startswith('16'):
                print(r.get('d'), json.dumps(parse(r['hex']), indent=1)); break
    else:
        print(json.dumps(parse(sys.argv[1]), indent=1))
