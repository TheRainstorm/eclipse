import sys,time,frida
pid=int(sys.argv[1]); secs=float(sys.argv[2]) if len(sys.argv)>2 else 12
s=frida.get_local_device().attach(pid)
sc=s.create_script(open(r"C:\Users\yfy\pckmap_len.js").read())
def m(x,d):
    if x.get('type')=='error': print('ERR',x,flush=True); return
    print(x.get('payload'),flush=True)
sc.on('message',m); sc.load(); print('attached',pid,flush=True); time.sleep(secs)
try: sc.unload()
finally: s.detach()
