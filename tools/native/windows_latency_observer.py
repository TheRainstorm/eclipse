"""Finite independent observer. The paint producer never waits for this process.

Ownership: producer writes a 64-byte seqlock page and initial/final scene report;
observer alone writes its progress and final inventories; parent joins both,
then merges reports. No shared lock or JSON work in the paint callback.
"""
import ctypes as C
from ctypes import wintypes as W
import json,math,mmap,os,pathlib,struct,time,uuid
MAGIC=0x45434C4950534536
PAGE_BYTES=64

def publish(page,count,ticks,state=1):
    struct.pack_into('<Q',page,8,count*2+1)
    struct.pack_into('<QQQ',page,16,count,ticks,state)
    struct.pack_into('<Q',page,8,count*2+2)

def read_page(page):
    for _ in range(4):
        magic,seq=struct.unpack_from('<QQ',page,0);count,ticks,state=struct.unpack_from('<QQQ',page,16);end=struct.unpack_from('<Q',page,8)[0]
        if magic==MAGIC and seq==end and not seq%2:return {'updates':count,'paint_ticks':ticks,'producer_state':state}
    raise RuntimeError('paint page inconsistent or missing owner')

class PaintPage:
    def __init__(self):
        self.name='Local\\EclipseLatencyPaint-'+uuid.uuid4().hex
        self.page=mmap.mmap(-1,PAGE_BYTES,tagname=self.name)
        struct.pack_into('<Q',self.page,0,MAGIC);publish(self.page,0,0)
    def record(self,count,ticks):publish(self.page,count,ticks)
    def close(self):self.page.close()

def collect(sample,wait,clock,frequency,progress,max_seconds,max_records,max_bytes=64*1024**2):
    """All expensive callbacks execute only in the observer process."""
    if not 1<=max_seconds<=600 or not 1<=max_records<=608:raise ValueError('observer bounds invalid')
    records=[];started=clock();due=started;state='starting';transitions=[{'state':state,'ticks':started}];error=None;missed=0;used=0
    try:
        state='observing';transitions.append({'state':state,'ticks':clock()})
        while True:
            if wait(max(0,(due-clock())/frequency)):
                state='stopping';transitions.append({'state':state,'ticks':clock()});break
            begin=clock()
            if begin-started>max_seconds*frequency:raise TimeoutError('observer lifetime exhausted')
            if len(records)>=max_records:raise RuntimeError('observer record limit')
            row={'sequence':len(records),'due_ticks':due,'begin_ticks':begin,'frequency':frequency,'missed_periods_before':max(0,(begin-due)//frequency)}
            missed+=row['missed_periods_before']
            try:row.update(sample())
            except BaseException as e:row.update(passed=False,error=type(e).__name__+': '+str(e))
            row['end_ticks']=clock();used+=len(json.dumps(row).encode('utf8'))
            if used>max_bytes:raise RuntimeError('observer retained-byte limit')
            records.append(row);progress(row,state)
            if not row['passed']:raise RuntimeError(row.get('error') or 'coverage/identity unknown or failed')
            due+=frequency
            if clock()>due:
                skipped=max(0,(clock()-due)//frequency);missed+=skipped;due+=skipped*frequency
        state='completed'
    except BaseException as e:error=type(e).__name__+': '+str(e);state='failed'
    transitions.append({'state':state,'ticks':clock()})
    return {'version':1,'transitions':transitions,'state':state,'completed':state=='completed','error':error,'records':records,'start_ticks':started,'stop_ticks':clock(),'frequency':frequency,'missed_periods':missed,'retained_bytes':used,'max_bytes':max_bytes,'max_records':max_records,'max_seconds':max_seconds,'nominal_interval_seconds':1,'max_unexplained_gap_seconds':2.5}

def same_owner(target,actual):
    return actual.get('known') is True and target.get('known') is True and actual.get('pid')==target.get('pid') and actual.get('creation_filetime')==target.get('creation_filetime')

def event(name,create=False):
    k=C.WinDLL('kernel32',use_last_error=True);k.CreateEventW.argtypes=[C.c_void_p,W.BOOL,W.BOOL,W.LPCWSTR];k.CreateEventW.restype=W.HANDLE;k.OpenEventW.argtypes=[W.DWORD,W.BOOL,W.LPCWSTR];k.OpenEventW.restype=W.HANDLE;k.SetEvent.argtypes=[W.HANDLE];k.WaitForSingleObject.argtypes=[W.HANDLE,W.DWORD];k.CloseHandle.argtypes=[W.HANDLE]
    h=k.CreateEventW(None,True,False,name) if create else k.OpenEventW(0x100000,False,name)
    if not h:raise C.WinError(C.get_last_error())
    return k,h

def run(config):
    import windows_latency_worker as worker
    import windows_latency_scene as scene
    dpi=None;cfg=config['observer'];root=pathlib.Path(config['root']);target=cfg['target'];expected=target['identity'];page=None;handle=None;result=None
    k=C.WinDLL('kernel32',use_last_error=True);freq=C.c_int64();k.QueryPerformanceFrequency(C.byref(freq))
    def ticks():
        t=C.c_int64();k.QueryPerformanceCounter(C.byref(t));return t.value
    observer=scene.identity(os.getpid());started=ticks()
    def target_matches():
        u=C.WinDLL('user32',use_last_error=True);u.IsWindow.argtypes=[W.HWND];u.GetWindowThreadProcessId.argtypes=[W.HWND,C.POINTER(W.DWORD)];pid=W.DWORD()
        if not u.IsWindow(target['hwnd']):raise RuntimeError('owned source window disappeared')
        u.GetWindowThreadProcessId(target['hwnd'],C.byref(pid));actual=scene.identity(pid.value)
        if pid.value!=target['pid'] or not same_owner(expected,actual):raise RuntimeError('owned source identity changed or unknown')
    def sample():
        target_matches();snapshot=scene.window_snapshot(relation_hwnd=target['hwnd']);coverage=scene.coverage(snapshot,target['hwnd'],target['pid'],cfg['bounds']);target_matches();state=read_page(page)
        return {'snapshot':snapshot,'coverage':coverage,'paint_progress':state,'passed':snapshot['session']==cfg['session'] and coverage['passed'] and state['producer_state']==1}
    def progress(row,state):worker.atomic(root/'stimulus-progress.json',{'state':state,'pid':target['pid'],'hwnd':target['hwnd'],'observer':observer,'sequence':row['sequence'],'updates':row.get('paint_progress',{}).get('updates'),'last_observation_end_ticks':row['end_ticks'],'passed':row['passed']})
    try:
        dpi=worker.establish_pm_v2()
        if not observer['known'] or not expected['known']:raise RuntimeError('observer/target identity unknown')
        k,handle=event(cfg['stop_event']);page=mmap.mmap(-1,PAGE_BYTES,tagname=cfg['paint_page']);read_page(page)
        def wait(seconds):
            status=k.WaitForSingleObject(handle,min(1000,max(0,math.ceil(seconds*1000))))
            if status not in [0,258]:raise RuntimeError('observer stop wait failed: '+str(status))
            return status==0
        result=collect(sample,wait,ticks,freq.value,progress,cfg['max_seconds'],cfg['max_records'])
    except BaseException as e:result={'state':'failed','completed':False,'error':type(e).__name__+': '+str(e),'records':[],'start_ticks':started,'stop_ticks':ticks(),'frequency':freq.value,'transitions':[{'state':'starting','ticks':started},{'state':'failed','ticks':ticks()}],'max_records':cfg['max_records'],'max_seconds':cfg['max_seconds'],'max_bytes':64*1024**2,'retained_bytes':0,'missed_periods':0}
    finally:
        if page is not None:page.close()
        if handle:k.CloseHandle(handle)
    result.update(pid=os.getpid(),parent_pid=os.getppid(),identity=observer,target=target,session_expected=cfg['session'],dpi=dpi,exit_code=0 if result['completed'] else 1)
    worker.atomic(root/'stimulus-observer.json',result);return result['exit_code']
