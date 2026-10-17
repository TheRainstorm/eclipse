'use strict';
const ws=Process.findModuleByName('ws2_32.dll');
const mod=Process.findModuleByName('streamer.dll');
if(!ws||!mod){send({ev:'error'});}else{
 const gp=new NativeFunction(ws.getExportByName('getpeername'),'int',['int','pointer','pointer']);
 function dest(s){const sa=Memory.alloc(128),sl=Memory.alloc(4);sl.writeInt(128);if(gp(s.toInt32(),sa,sl)!==0)return '?';if(sa.readU16()!==2)return '?';const p=(sa.add(2).readU8()<<8)|sa.add(3).readU8();return [4,5,6,7].map(i=>sa.add(i).readU8()).join('.')+':'+p;}
 const W=mod.base.add(0xdd7f30);
 Interceptor.attach(W,{onEnter(a){this.b=a[1];this.n=a[2].toInt32();},onLeave(r){const n=r.toInt32();if(n<=0)return;try{const b=new Uint8Array(this.b.readByteArray(3));if(b[0]===0x50&&b[1]===0x43&&b[2]===0x4b)send({ev:'pckw',t:Date.now(),len:n});}catch(e){}}});
 function log(s,buf,len){if(len<5)return;let b0=0,b1=0,b2=0;try{b0=buf.readU8();b1=buf.add(1).readU8();b2=buf.add(2).readU8();}catch(e){}if(b0!==0x17||b1!==3||b2!==3)return;send({ev:'wire',t:Date.now(),sock:s.toString(),d:dest(s),len});}
 try{const f=ws.getExportByName('send');Interceptor.attach(f,{onEnter(a){this.s=a[0];this.b=a[1];},onLeave(r){const n=r.toInt32();if(n>0)log(this.s,this.b,n);}});}catch(e){}
 try{const f=ws.getExportByName('WSASend');Interceptor.attach(f,{onEnter(a){this.s=a[0];this.w=a[1];},onLeave(r){if(r.toInt32()===0)log(this.s,this.w.add(8).readPointer(),this.w.readU32());}});}catch(e){}
 send({ev:'hooked'});
}
