"""Deterministic full-source fixture and read-only interactive window ownership.

No machine addresses, user windows or display settings are changed here.
"""
import ctypes as C
from ctypes import wintypes as W
import hashlib
import json
import math
import os
import random
import struct
import sys
import time


def manifest(config):
    scene=config.get('stimulus_scene')
    if scene is None:return None
    if not isinstance(scene,dict) or scene.get('mode')!='full_source_v1':raise ValueError('unknown stimulus scene mode')
    bounds=scene.get('bounds');seed=scene.get('seed',734219)
    if not isinstance(bounds,list) or len(bounds)!=4 or any(type(v)is not int for v in bounds) or not 2400<=bounds[2]<=8192 or not 1850<=bounds[3]<=8192 or any(abs(v)>65536 for v in bounds[:2]):raise ValueError('full source physical bounds invalid')
    if type(seed)is not int or not 0<=seed<2**32:raise ValueError('scene seed invalid')
    return {'mode':'full_source_v1','bounds':bounds,'seed':seed,'background':[24,28,36],'border_width':8,'corner_RGB':[[220,120,40],[40,180,220],[180,40,220],[80,220,100]],'border_RGB':[[240,40,40],[40,240,40],[40,40,240],[240,240,40]],'stripe_roi':[32,32,960,540],'stripe_period':220,'texture_rect':[1280,768,1024,1024],'texture_cell':4,'texture_period':16,'texture_formula':'MT19937.Random(seed+phase).randbytes(256*256*3), row-major RGB, each cell4x4 physical pixels','barcode_origin':[1120,80],'barcode_cell':[32,32],'barcode_stride':48,'barcode_low':32,'barcode_high':224,'texture_cache_bytes':16*1024*1024*4,'texture_cache_limit_bytes':128*1024*1024,'paint_requested_fps':config.get('stimulus_fps',config['fps'])}


def texture_grid(scene,phase):return random.Random(scene['seed']+phase%16).randbytes(256*256*3)


def texture_cache(scene):
    frames=[]
    for phase in range(16):
        grid=texture_grid(scene,phase);rows=[]
        for y in range(256):
            row=grid[y*768:(y+1)*768]
            bgra=b''.join(bytes((row[x+2],row[x+1],row[x],0))*4 for x in range(0,768,3))
            rows.append(bgra*4)
        frames.append(b''.join(rows))
    if sum(map(len,frames))>scene['texture_cache_limit_bytes']:raise RuntimeError('scene cache overflow')
    return frames


def identity(pid):
    k=C.WinDLL('kernel32',use_last_error=True);k.OpenProcess.argtypes=[W.DWORD,W.BOOL,W.DWORD];k.OpenProcess.restype=W.HANDLE;k.CloseHandle.argtypes=[W.HANDLE]
    k.QueryFullProcessImageNameW.argtypes=[W.HANDLE,W.DWORD,W.LPWSTR,C.POINTER(W.DWORD)];k.GetProcessTimes.argtypes=[W.HANDLE,C.c_void_p,C.c_void_p,C.c_void_p,C.c_void_p]
    h=k.OpenProcess(0x1000,False,pid)
    if not h:return {'pid':pid,'known':False,'error':C.get_last_error()}
    try:
        name=C.create_unicode_buffer(32768);size=W.DWORD(len(name));created=C.c_uint64();ex=C.c_uint64();kt=C.c_uint64();ut=C.c_uint64()
        a=k.QueryFullProcessImageNameW(h,0,name,C.byref(size));b=k.GetProcessTimes(h,C.byref(created),C.byref(ex),C.byref(kt),C.byref(ut))
        return {'pid':pid,'known':bool(a and b),'image':name.value if a else None,'creation_filetime':created.value if b else None,'error':None if a and b else C.get_last_error()}
    finally:k.CloseHandle(h)


def window_snapshot(relation_hwnd=None):
    k=C.WinDLL('kernel32',use_last_error=True);sid=W.DWORD();k.ProcessIdToSessionId(os.getpid(),C.byref(sid))
    if sid.value==0:raise RuntimeError('window inventory requires interactive session')
    u=C.WinDLL('user32',use_last_error=True);d=C.WinDLL('dwmapi');u.GetWindowThreadProcessId.argtypes=[W.HWND,C.POINTER(W.DWORD)];u.GetWindowRect.argtypes=[W.HWND,C.POINTER(W.RECT)];u.GetWindowTextW.argtypes=[W.HWND,W.LPWSTR,C.c_int];u.IsWindowVisible.argtypes=[W.HWND];u.IsIconic.argtypes=[W.HWND];u.GetForegroundWindow.restype=W.HWND;u.GetWindowLongPtrW.argtypes=[W.HWND,C.c_int];u.GetWindowLongPtrW.restype=C.c_ssize_t;d.DwmGetWindowAttribute.argtypes=[W.HWND,W.DWORD,C.c_void_p,W.DWORD]
    rows=[];cache={};callback=C.WINFUNCTYPE(W.BOOL,W.HWND,W.LPARAM)
    def each(hwnd,_):
        pid=W.DWORD();u.GetWindowThreadProcessId(hwnd,C.byref(pid));rect=W.RECT();ok=u.GetWindowRect(hwnd,C.byref(rect));title=C.create_unicode_buffer(512);u.GetWindowTextW(hwnd,title,512);cloaked=W.DWORD();status=d.DwmGetWindowAttribute(hwnd,14,C.byref(cloaked),C.sizeof(cloaked))
        if pid.value not in cache:cache[pid.value]=identity(pid.value)
        rows.append({'hwnd':int(hwnd),'pid':pid.value,'identity':cache[pid.value],'enumeration_index':len(rows),'extended_style':int(u.GetWindowLongPtrW(hwnd,-20)),'title':title.value,'rect':[rect.left,rect.top,rect.right,rect.bottom] if ok else None,'visible':bool(u.IsWindowVisible(hwnd)),'minimized':bool(u.IsIconic(hwnd)),'cloaked':cloaked.value if status==0 else None})
        return True
    cb=callback(each);u.EnumWindows.argtypes=[callback,W.LPARAM]
    if not u.EnumWindows(cb,0):raise C.WinError(C.get_last_error())
    q=C.c_int64();f=C.c_int64();k.QueryPerformanceCounter(C.byref(q));k.QueryPerformanceFrequency(C.byref(f))
    relations={}
    u.GetWindow.argtypes=[W.HWND,W.UINT];u.GetWindow.restype=W.HWND;u.IsWindow.argtypes=[W.HWND]
    for row in rows:
        if row['pid']!=os.getpid() and row['hwnd']!=relation_hwnd:continue
        chain=[];seen={row['hwnd']};cur=row['hwnd'];unknown=None
        for _ in range(512):
            if not u.IsWindow(cur):unknown='window disappeared';break
            nxt=int(u.GetWindow(cur,3)or 0)
            if not nxt:break
            if nxt in seen:unknown='repeated HWND';break
            if not u.IsWindow(nxt) or not any(r['hwnd']==nxt for r in rows):unknown='inventory/relationship race';break
            seen.add(nxt);chain.append(nxt);cur=nxt
        else:unknown='relationship traversal limit'
        if not u.IsWindow(row['hwnd']) or int(u.GetWindow(row['hwnd'],3)or 0)!=(chain[0] if chain else 0):unknown='relationship changed during snapshot'
        relations[str(row['hwnd'])]={'above':chain,'unknown':unknown,'api':'GetWindow GW_HWNDPREV','limit':512}
    return {'relations':relations,'session':sid.value,'observer':identity(os.getpid()),'parent_pid':os.getppid(),'qpc':{'ticks':q.value,'frequency':f.value},'foreground':int(u.GetForegroundWindow()or 0),'windows':rows}


def intersects(a,b):return a[0]<b[2] and a[2]>b[0] and a[1]<b[3] and a[3]>b[1]


def coverage(snapshot,hwnd,pid,bounds):
    expected=[bounds[0],bounds[1],bounds[0]+bounds[2],bounds[1]+bounds[3]];own=next((r for r in snapshot['windows'] if r['hwnd']==hwnd),None)
    above=[]
    if own:
        above=[r for r in snapshot['windows'] if r['hwnd'] in snapshot.get('relations',{}).get(str(hwnd),{}).get('above',[]) and r['visible'] and not r['minimized'] and r['cloaked']!=1 and (r['rect'] is None or intersects(r['rect'],expected))]
    relation=snapshot.get('relations',{}).get(str(hwnd));checks={'documented_relationship_known':relation is not None and relation['unknown'] is None,'owned_identity':own is not None and own['pid']==pid and own['identity']['known'],'physical_full_bounds':own is not None and own['rect']==expected,'visible':own is not None and own['visible'] and not own['minimized'],'uncloaked_known':own is not None and own['cloaked']==0,'topmost_style_known':own is not None and bool(own.get('extended_style',0)&8),'no_visible_occluder':own is not None and not above}
    return {'passed':all(checks.values()),'checks':checks,'own':own,'above_intersecting':above}


def foreign_changes(before,after,owned_pid):
    end={r['hwnd']:r for r in after['windows'] if r['pid']!=owned_pid};changes=[]
    for old in before['windows']:
        if old['pid']==owned_pid:continue
        new=end.get(old['hwnd'])
        if new is None:
            if old['visible']:changes.append({'hwnd':old['hwnd'],'change':'visible window disappeared','before':old})
            continue
        keys=['pid','rect','visible','minimized']
        different=[k for k in keys if old[k]!=new[k]]
        if old['identity']['known'] and new['identity']['known'] and old['identity']['creation_filetime']!=new['identity']['creation_filetime']:different.append('creation identity')
        if different:changes.append({'hwnd':old['hwnd'],'fields':different,'before':old,'after':new})
    return changes


class Renderer:
    def __init__(self,scene,user,gdi):
        self.scene=scene;self.user=user;self.gdi=gdi;self.cache=texture_cache(scene);self.pointers=[C.c_char_p(v) for v in self.cache];self.mem=None;self.bitmap=None;self.old=None
        gdi.CreateCompatibleDC.argtypes=[W.HDC];gdi.CreateCompatibleDC.restype=W.HDC;gdi.CreateCompatibleBitmap.argtypes=[W.HDC,C.c_int,C.c_int];gdi.CreateCompatibleBitmap.restype=W.HBITMAP;gdi.SelectObject.argtypes=[W.HDC,W.HGDIOBJ];gdi.SelectObject.restype=W.HGDIOBJ;gdi.DeleteDC.argtypes=[W.HDC];gdi.BitBlt.argtypes=[W.HDC,C.c_int,C.c_int,C.c_int,C.c_int,W.HDC,C.c_int,C.c_int,W.DWORD]
        gdi.StretchDIBits.argtypes=[W.HDC,C.c_int,C.c_int,C.c_int,C.c_int,C.c_int,C.c_int,C.c_int,C.c_int,C.c_void_p,C.c_void_p,W.UINT,W.DWORD]
        self.info=C.create_string_buffer(struct.pack('<IiiHHIIiiII',40,1024,-1024,1,32,0,0,0,0,0,0))
    def fill(self,dc,rect,rgb):
        brush=self.gdi.CreateSolidBrush(rgb[0]|rgb[1]<<8|rgb[2]<<16)
        try:self.user.FillRect(dc,C.byref(W.RECT(*rect)),brush)
        finally:self.gdi.DeleteObject(brush)
    def paint(self,dc,count):
        s=self.scene;w,h=s['bounds'][2:]
        if self.mem is None:
            self.mem=self.gdi.CreateCompatibleDC(dc);self.bitmap=self.gdi.CreateCompatibleBitmap(dc,w,h)
            if not self.mem or not self.bitmap:raise C.WinError(C.get_last_error())
            self.old=self.gdi.SelectObject(self.mem,self.bitmap);self.fill(self.mem,[0,0,w,h],s['background'])
            for rect,color in zip([[0,0,w,8],[w-8,0,w,h],[0,h-8,w,h],[0,0,8,h]],s['border_RGB']):self.fill(self.mem,rect,color)
        for rect,color in zip([[8,8,24,24],[w-24,8,w-8,24],[8,h-24,24,h-8],[w-24,h-24,w-8,h-8]],s['corner_RGB']):self.fill(self.mem,rect,color)
        phase=count%16;x,y,tw,th=s['texture_rect']
        if self.gdi.StretchDIBits(self.mem,x,y,tw,th,0,0,tw,th,self.pointers[phase],self.info,0,0x00CC0020)==-1:raise RuntimeError('scene texture draw failed')
        for band in range(6):self.fill(self.mem,[32+band*160,32,32+(band+1)*160,572],[(count*3+band*37)%220+20,(band*41+count)%220+20,(count*2+band*31)%220+20])
        for bit in range(8):
            value=((phase>>(bit%4))&1)^(bit//4);v=s['barcode_high'] if value else s['barcode_low'];x=s['barcode_origin'][0]+bit*s['barcode_stride'];y=s['barcode_origin'][1];self.fill(self.mem,[x,y,x+32,y+32],[v,v,v])
        if not self.gdi.BitBlt(dc,0,0,w,h,self.mem,0,0,0x00CC0020):raise RuntimeError('scene final blit failed')
    def close(self):
        if self.mem:
            if self.old:self.gdi.SelectObject(self.mem,self.old)
            if self.bitmap:self.gdi.DeleteObject(self.bitmap)
            self.gdi.DeleteDC(self.mem);self.mem=None


def screen_probe(scene):
    """One bounded screen readback, never periodic full-screen screenshots."""
    u=C.WinDLL('user32');g=C.WinDLL('gdi32');u.GetDC.argtypes=[W.HWND];u.GetDC.restype=W.HDC;u.ReleaseDC.argtypes=[W.HWND,W.HDC];g.CreateCompatibleDC.argtypes=[W.HDC];g.CreateCompatibleDC.restype=W.HDC;g.CreateCompatibleBitmap.argtypes=[W.HDC,C.c_int,C.c_int];g.CreateCompatibleBitmap.restype=W.HBITMAP;g.SelectObject.argtypes=[W.HDC,W.HGDIOBJ];g.SelectObject.restype=W.HGDIOBJ;g.BitBlt.argtypes=[W.HDC,C.c_int,C.c_int,C.c_int,C.c_int,W.HDC,C.c_int,C.c_int,W.DWORD];g.GetDIBits.argtypes=[W.HDC,W.HBITMAP,W.UINT,W.UINT,C.c_void_p,C.c_void_p,W.UINT];g.DeleteObject.argtypes=[W.HGDIOBJ];g.DeleteDC.argtypes=[W.HDC]
    x,y,w,h=scene['bounds'];dc=None;mem=None;bm=None;old=None;selected=False;cleanup=[]
    invalid=(None,0,-1,C.c_void_p(-1).value)
    try:
        dc=u.GetDC(None)
        if not dc:raise RuntimeError('GetDC failed')
        mem=g.CreateCompatibleDC(dc)
        if not mem:raise RuntimeError('CreateCompatibleDC failed')
        bm=g.CreateCompatibleBitmap(dc,w,h)
        if not bm:raise RuntimeError('CreateCompatibleBitmap failed')
        old=g.SelectObject(mem,bm)
        if old in invalid:raise RuntimeError('SelectObject bitmap failed')
        selected=True
        if not g.BitBlt(mem,0,0,w,h,dc,x,y,0x00CC0020):raise RuntimeError('screen copy failed')
        if g.SelectObject(mem,old) in invalid:raise RuntimeError('SelectObject restore before read failed')
        selected=False
        info=C.create_string_buffer(struct.pack('<IiiHHIIiiII',40,w,-h,1,32,0,0,0,0,0,0));buffer=C.create_string_buffer(w*h*4)
        if g.GetDIBits(mem,bm,0,h,buffer,info,0)!=h:raise RuntimeError('screen readback incomplete')
        data=buffer.raw
        return validate_pixels(scene,lambda xx,yy:list(data[(yy*w+xx)*4:(yy*w+xx)*4+3][::-1]))
    finally:
        if selected and g.SelectObject(mem,old) in invalid:
            cleanup.append('restore selection')
            # A selected bitmap cannot be deleted: release its DC first.
            if g.DeleteDC(mem):mem=None
            else:cleanup.append('DeleteDC after failed restore')
        if bm and not g.DeleteObject(bm):cleanup.append('DeleteObject')
        if mem and not g.DeleteDC(mem):cleanup.append('DeleteDC')
        if dc and not u.ReleaseDC(None,dc):cleanup.append('ReleaseDC')
        if cleanup:raise RuntimeError('screen cleanup failed: '+', '.join(cleanup))



def validate_pixels(scene,pixel):
    """Finite markers/stripes/texture checks, with predeclared lossy tolerances."""
    w,h=scene['bounds'][2:];border_points=[(w//2,3),(w-4,h//2),(w//2,h-4),(3,h//2)]
    borders=[pixel(*p) for p in border_points];border_ok=all(max(abs(a-b)for a,b in zip(v,expected))<=20 for v,expected in zip(borders,scene['border_RGB']))
    corners=[pixel(16,16),pixel(w-17,16),pixel(16,h-17),pixel(w-17,h-17)];corner_ok=all(max(abs(a-b)for a,b in zip(v,c))<=20 for v,c in zip(corners,scene['corner_RGB']))
    bits=[pixel(1136+i*48,96)[0] for i in range(8)];binary=[v>128 for v in bits];phase=sum(int(binary[i])<<i for i in range(4));barcode_ok=all(absolute<40 for absolute in [abs(v-(224 if b else 32)) for v,b in zip(bits,binary)]) and all(binary[i]!=binary[i+4] for i in range(4))
    stripes=[pixel(112+b*160,302) for b in range(6)];scores=[]
    for n in range(220):scores.append((max(abs(a-b)for band,actual in enumerate(stripes)for a,b in zip(actual,[(n*3+band*37)%220+20,(band*41+n)%220+20,(n*2+band*31)%220+20])),n))
    stripe_error,stripe_phase=min(scores);grid=texture_grid(scene,phase);actual=[];expected=[];samples=[]
    for cy in range(8,256,16):
        for cx in range(8,256,16):
            xy=[1280+cx*4+2,768+cy*4+2];rgb=pixel(*xy);exp=list(grid[(cy*256+cx)*3:(cy*256+cx)*3+3]);actual+=rgb;expected+=exp;samples.append({'xy':xy,'RGB':rgb,'expected_RGB':exp,'phase':phase})
    mae=sum(abs(a-b)for a,b in zip(actual,expected))/len(actual);am=sum(actual)/len(actual);em=sum(expected)/len(expected);den=math.sqrt(sum((a-am)**2 for a in actual)*sum((b-em)**2 for b in expected));correlation=sum((a-am)*(b-em)for a,b in zip(actual,expected))/den if den else None
    checks={'all_borders':border_ok,'all_corners':corner_ok,'phase_marker':barcode_ok,'six_stripes':stripe_error<=20,'texture_matches_phase':mae<=55 and correlation is not None and correlation>=.45}
    return {'passed':all(checks.values()),'checks':checks,'raw_samples':{'borders':[{'xy':list(xy),'RGB':rgb,'expected_RGB':exp} for xy,rgb,exp in zip(border_points,borders,scene['border_RGB'])],'corners':[{'xy':list(xy),'RGB':rgb,'expected_RGB':exp} for xy,rgb,exp in zip([(16,16),(w-17,16),(16,h-17),(w-17,h-17)],corners,scene['corner_RGB'])],'barcode':[{'xy':[1136+i*48,96],'RGB':pixel(1136+i*48,96),'expected_RGB':[224 if binary[i] else 32]*3} for i in range(8)],'stripes':[{'xy':[112+b*160,302],'RGB':rgb,'expected_RGB':[(stripe_phase*3+b*37)%220+20,(b*41+stripe_phase)%220+20,(stripe_phase*2+b*31)%220+20]} for b,rgb in enumerate(stripes)],'texture':samples},'borders':borders,'corners':corners,'barcode_samples':bits,'texture_phase':phase,'stripe_phase':stripe_phase,'stripe_RGB_error':stripe_error,'texture_RGB_mean_absolute_error':mae,'texture_RGB_correlation':correlation,'tolerances':{'marker_and_stripes_max_error':20,'barcode_max_error_exclusive':40,'texture_MAE_max':55,'texture_correlation_min':.45},'scope':'Finite sample markers/256 texture points, not full-frame quality or all-frame pixel proof'}
