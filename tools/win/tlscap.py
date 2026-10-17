import json,sys,time,frida
pid=int(sys.argv[1]); secs=float(sys.argv[2]) if len(sys.argv)>2 else 1800
out=sys.argv[3] if len(sys.argv)>3 else r"C:\Users\yfy\tlscap.jsonl"
s=frida.get_local_device().attach(pid)
sc=s.create_script(open(r"C:\Users\yfy\tlscap.js").read())
f=open(out,"w",encoding="utf-8")
def m(x,d):
    if x.get('type')=='error': print('ERR',x,flush=True); return
    p=x.get('payload'); f.write(json.dumps(p)+"\n"); f.flush()
    if p.get('ev')!='raw': print(p,flush=True)
sc.on('message',m); sc.load(); print('attached',pid,flush=True); time.sleep(secs)
try: sc.unload()
finally: f.close(); s.detach()
print('done',flush=True)
