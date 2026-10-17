import json, sys, time
import frida
pid=int(sys.argv[1]); secs=float(sys.argv[2]) if len(sys.argv)>2 else 180
out=sys.argv[3] if len(sys.argv)>3 else r"C:\Users\yfy\sslcap_dyn.jsonl"
dev=frida.get_local_device(); session=dev.attach(pid)
script=session.create_script(open(r"C:\Users\yfy\sslcapture_dyn.js","r",encoding="utf-8").read())
f=open(out,"w",encoding="utf-8")
def on_msg(m,d):
    if m.get("type")=="error": print("ERR",m,flush=True); return
    p=m.get("payload"); f.write(json.dumps(p,ensure_ascii=False)+"\n"); f.flush()
    if p.get("ev") in ("resolved","hooked","error"): print(p,flush=True)
    elif p.get("ev")=="io": print(p["dir"],p["len"],p["head"][:64],flush=True)
script.on("message",on_msg); script.load()
print("attached",pid,"for",secs,flush=True); time.sleep(secs)
try: script.unload()
finally: f.close(); session.detach()
print("done",flush=True)
