'use strict';
// Capture the startup WebSocket handshake and Socket.IO auth packet.
const out = [];
function sendEv(o){ try{ send(o); }catch(e){} }
function hookWs(){
  const ws = Process.findModuleByName('ws2_32.dll'); if(!ws) return;
  let n=0;
  function log(kind, s, buf, len){
    if(len<=0 || n>300) return;
    let b0=0; try{b0=buf.readU8();}catch(e){}
    if(!(b0===0x16 || b0===0x81 || b0===0x82)) {
      // keep ASCII HTTP requests too
      let pre=''; try{pre=buf.readUtf8String(Math.min(len,8))||'';}catch(e){}
      if(!/^(GET|POST|HEAD|CONNECT)/.test(pre)) return;
    }
    n++;
    const k=Math.min(len, 4096);
    let hex=''; try{hex=Array.from(new Uint8Array(buf.readByteArray(k))).map(x=>x.toString(16).padStart(2,'0')).join('');}catch(e){}
    sendEv({ev:'raw',kind,s:s.toString(),len,hex});
  }
  try{
    const s=ws.getExportByName('send');
    Interceptor.attach(s,{onEnter(a){this.s=a[0];this.b=a[1];},onLeave(r){log('send',this.s,this.b,r.toInt32());}});
    const r=ws.getExportByName('recv');
    Interceptor.attach(r,{onEnter(a){this.s=a[0];this.b=a[1];},onLeave(x){log('recv',this.s,this.b,x.toInt32());}});
    const wsnd=ws.getExportByName('WSASend');
    Interceptor.attach(wsnd,{onEnter(a){this.s=a[0];this.w=a[1];},onLeave(x){if(x.toInt32()!==0)return;const l=this.w.readU32();log('wsasend',this.s,this.w.add(8).readPointer(),l);}});
    const wrcv=ws.getExportByName('WSARecv');
    Interceptor.attach(wrcv,{onEnter(a){this.s=a[0];this.w=a[1];this.lp=ptr(a[3]);},onLeave(x){if(x.toInt32()!==0)return;const l=this.lp.readU32();log('wsarecv',this.s,this.w.add(8).readPointer(),l);}});
    sendEv({ev:'ws_hooked'});
  }catch(e){ sendEv({ev:'ws_err',msg:e.message}); }
}
function hookSsl(){
  const m=Process.findModuleByName('streamer.dll'); if(!m) return false;
  try{
    const W=m.base.add(0xdd7f30), R=m.base.add(0xdd7690);
    Interceptor.attach(W,{onEnter(a){this.s=a[0];this.b=a[1];},onLeave(r){const n=r.toInt32();if(n<=0)return;let hex='';try{hex=Array.from(new Uint8Array(this.b.readByteArray(Math.min(n,4096)))).map(x=>x.toString(16).padStart(2,'0')).join('');}catch(e){}sendEv({ev:'ssl',d:'w',s:this.s.toString(),len:n,hex});}});
    Interceptor.attach(R,{onEnter(a){this.s=a[0];this.b=a[1];},onLeave(r){const n=r.toInt32();if(n<=0)return;let hex='';try{hex=Array.from(new Uint8Array(this.b.readByteArray(Math.min(n,4096)))).map(x=>x.toString(16).padStart(2,'0')).join('');}catch(e){}sendEv({ev:'ssl',d:'r',s:this.s.toString(),len:n,hex});}});
    sendEv({ev:'ssl_hooked'});
    return true;
  }catch(e){ sendEv({ev:'ssl_err',msg:e.message}); return false; }
}
hookWs();
if(!hookSsl()){
  const t=setInterval(()=>{ if(hookSsl()) clearInterval(t); }, 100);
}
