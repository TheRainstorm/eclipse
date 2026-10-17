#!/usr/bin/env python3
"""Summarize PCK TLS captures (tlscap*.jsonl + pcksniff*.jsonl)."""
import json, sys, struct
sys.path.insert(0, __file__.rsplit('/', 1)[0])
from parse_clienthello import parse

def load(p):
    out = []
    try:
        for line in open(p):
            out.append(json.loads(line))
    except FileNotFoundError:
        pass
    return out

for path in sys.argv[1:]:
    rows = load(path)
    print('===', path, len(rows), 'events')
    maps = [r for r in rows if r.get('ev') == 'map']
    if maps:
        print(' SSL -> TCP mappings:')
        for m in maps:
            print('  ', m['ssl'], '->', m['d'], 'plain', m.get('plain'), 'wire', m.get('wire'), 'pck', m.get('pck'))
    for r in rows:
        if r.get('ev') != 'raw':
            continue
        h = r.get('hex', '')
        if not h.startswith('16'):
            continue
        info = parse(h)
        print(' TLS handshake from', r.get('d'), 'len', r.get('len'))
        print('  version', info.get('version'), 'sni', info.get('sni'), 'alpn', info.get('alpn'))
        print('  ciphers', info.get('ciphers', [])[:12])
        print('  ext', info.get('extensions'))
