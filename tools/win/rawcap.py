import json,sys,time,frida
pid=int(sys.argv[1]); secs=float(sys.argv[2]) if len(sys.argv)>2 else 15
s=frida.get_local_device().attach(pid)
sc=s.create_script(open(r"C:\Users\yfy\rawcap.js").read())
def m(x,d):
    if x.get('type')=='error': print('ERR',x,flush=True); return
    p=x.get('payload')
    if p.get('ev')=='io': print(p['kind'],p['len'],p['hex'],('STACK '+str(p.get('stack')) if p.get('first') else ''),flush=True)
    else: print(p,flush=True)
sc.on('message',m); sc.load(); print('attached',pid,flush=True); time.sleep(secs)
try: sc.unload()
finally: s.detach()
print('done',flush=True)
