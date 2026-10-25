import multiprocessing as mp
import struct,time,unittest
import windows_latency_observer as observer

def slow_observer(stop,ready,output):
    def sample():ready.set();time.sleep(.20);return {'passed':True}
    result=observer.collect(sample,stop.wait,time.perf_counter_ns,1_000_000_000,lambda *a:None,5,8)
    output.put(result)

class ObserverLifecycle(unittest.TestCase):
    def test_slow_process_does_not_block_source_and_stops_normally(self):
        context=mp.get_context('spawn');stop=context.Event();ready=context.Event();out=context.Queue();child=context.Process(target=slow_observer,args=(stop,ready,out));child.start()
        try:
            self.assertTrue(ready.wait(5));ticks=[];end=time.monotonic()+.35
            while time.monotonic()<end:ticks.append(time.perf_counter());time.sleep(.005)
            stop.set();child.join(5);self.assertEqual(child.exitcode,0);result=out.get(timeout=2);self.assertTrue(result['completed']);self.assertEqual([x['state']for x in result['transitions']],['starting','observing','stopping','completed']);self.assertGreater(len(ticks),40);self.assertLess(max(b-a for a,b in zip(ticks,ticks[1:])),.10)
        finally:
            if child.is_alive():child.terminate();child.join()
    def test_exception_identity_failure_and_record_bound_are_terminal(self):
        for sample,cap,reason in [(lambda:(_ for _ in ()).throw(RuntimeError('owner changed')),8,'owner changed'),(lambda:{'passed':False,'error':'identity unknown'},8,'identity unknown'),(lambda:{'passed':True},1,'record limit')]:
            now=[0]
            def wait(seconds):now[0]+=int(seconds*1000);return False
            result=observer.collect(sample,wait,lambda:now[0],1000,lambda *a:None,5,cap)
            self.assertEqual(result['state'],'failed');self.assertFalse(result['completed']);self.assertIn(reason,result['error']);self.assertLessEqual(len(result['records']),cap)
    def test_page_consistency_and_missing_owner_fail_closed(self):
        page=bytearray(observer.PAGE_BYTES);struct.pack_into('<Q',page,0,observer.MAGIC);observer.publish(page,23,12345);self.assertEqual(observer.read_page(page),{'updates':23,'paint_ticks':12345,'producer_state':1});struct.pack_into('<Q',page,8,49)
        with self.assertRaises(RuntimeError):observer.read_page(page)
        struct.pack_into('<Q',page,0,0)
        with self.assertRaises(RuntimeError):observer.read_page(page)
    def test_owner_creation_reuse_and_unknown_are_rejected(self):
        owner={'known':True,'pid':5,'creation_filetime':10};self.assertTrue(observer.same_owner(owner,dict(owner)))
        for changed in [dict(owner,pid=6),dict(owner,creation_filetime=11),dict(owner,known=False)]:self.assertFalse(observer.same_owner(owner,changed))

    def test_stop_and_storage_bounds(self):
        result=observer.collect(lambda:{'passed':True},lambda _:True,lambda:0,1000,lambda *a:None,5,8);self.assertTrue(result['completed']);self.assertEqual(result['records'],[])
        result=observer.collect(lambda:{'passed':True},lambda _:False,lambda:0,1000,lambda *a:None,5,8,1);self.assertFalse(result['completed']);self.assertIn('byte limit',result['error'])
if __name__=='__main__':unittest.main()
