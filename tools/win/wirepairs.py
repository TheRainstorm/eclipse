import sys,time,frida,json
pid=int(sys.argv[1]); secs=float(sys.argv[2]) if len(sys.argv)>2 else 10
s=frida.get_local_device().attach(pid)
sc=s.create_script(open(r"C:\Users\yfy\wirepairs.js").read())
rows=[]
def m(x,d):
    if x.get('type')=='error': print('ERR',x,flush=True); return
    p=x.get('payload')
    if p.get('ev') in ('pckw','wire'): rows.append(p)
    else: print(p,flush=True)
sc.on('message',m); sc.load(); time.sleep(secs)
try: sc.unload()
finally: s.detach()
# correlate each pckw with wire records within 30ms whose len is pck len + 5..40
for p in [r for r in rows if r['ev']=='pckw']:
    cands=[w for w in rows if w['ev']=='wire' and 0 <= w['t']-p['t'] <= 30 and 5 <= w['len']-p['len'] <= 40]
    if cands:
        w=cands[0]
        print(f"pck {p['len']:4d} -> {w['d']:24s} wire {w['len']:4d} diff {w['len']-p['len']:2d}")
