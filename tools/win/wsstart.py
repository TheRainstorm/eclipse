import frida,sys,time,json
# usage: wsstart.py <existing_pid_to_ignore> <seconds>
ignore=int(sys.argv[1]) if len(sys.argv)>1 else 0
secs=float(sys.argv[2]) if len(sys.argv)>2 else 90
out=sys.argv[3] if len(sys.argv)>3 else r"C:\Users\yfy\wsstart.jsonl"
dev=frida.get_local_device()
deadline=time.time()+120
pid=None
while time.time()<deadline:
    for p in dev.enumerate_processes():
        if p.name.lower()=='gameviewer.exe' and p.pid!=ignore:
            pid=p.pid; break
    if pid: break
    time.sleep(0.1)
if not pid:
    print('no new GameViewer.exe',flush=True); sys.exit(1)
print('new pid',pid,flush=True)
s=dev.attach(pid)
sc=s.create_script(open(r"C:\Users\yfy\wsstart.js").read())
f=open(out,'w',encoding='utf-8')
def m(x,d):
    if x.get('type')=='error': print('ERR',x,flush=True); return
    p=x.get('payload'); f.write(json.dumps(p)+"\n"); f.flush()
    if p.get('ev')!='raw' and p.get('ev')!='ssl': print(p,flush=True)
sc.on('message',m); sc.load()
print('attached',pid,flush=True); time.sleep(secs)
try: sc.unload()
finally: f.close(); s.detach()
print('done',flush=True)
