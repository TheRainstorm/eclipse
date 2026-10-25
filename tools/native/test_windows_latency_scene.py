import unittest
from unittest.mock import Mock, patch
import windows_latency_scene as scene

class FullScene(unittest.TestCase):
    def setUp(self):self.spec=scene.manifest({'fps':60,'stimulus_fps':120,'stimulus_scene':{'mode':'full_source_v1','bounds':[-20,30,3840,2160],'seed':734219}})
    def test_explicit_mode_validation_and_bounded_reproducible_texture(self):
        self.assertIsNone(scene.manifest({'fps':60}))
        for spec in [{'mode':'other','bounds':[0,0,3840,2160]},{'mode':'full_source_v1','bounds':[0,0,1920,1080]},{'mode':'full_source_v1','bounds':[0,0,3840,2160],'seed':True}]:
            with self.assertRaises(ValueError):scene.manifest({'fps':60,'stimulus_scene':spec})
        a=scene.texture_grid(self.spec,3);self.assertEqual(a,scene.texture_grid(self.spec,19));self.assertNotEqual(a,scene.texture_grid(self.spec,4));cache=scene.texture_cache(self.spec);self.assertEqual(sum(map(len,cache)),64*1024*1024);self.assertEqual(cache[3][0:4],bytes([a[2],a[1],a[0],0]));self.assertEqual(cache[3][:4096],cache[3][4096:8192])
    def test_window_identity_geometry_occlusion_and_foreign_changes(self):
        own={'hwnd':10,'pid':20,'identity':{'known':True,'creation_filetime':1},'z':1,'extended_style':8,'rect':[-20,30,3820,2190],'visible':True,'minimized':False,'cloaked':0}
        snap={'windows':[own],'relations':{'10':{'above':[],'unknown':None}}};self.assertTrue(scene.coverage(snap,10,20,self.spec['bounds'])['passed'])
        own['extended_style']=0;self.assertFalse(scene.coverage(snap,10,20,self.spec['bounds'])['passed']);own['extended_style']=8
        self.assertFalse(scene.coverage(snap,10,21,self.spec['bounds'])['passed']);own['identity']['known']=False;self.assertFalse(scene.coverage(snap,10,20,self.spec['bounds'])['passed']);own['identity']['known']=True
        occluder={**own,'hwnd':11,'pid':22,'z':0,'rect':[0,0,30,80]};self.assertFalse(scene.coverage({'windows':[occluder,own],'relations':{'10':{'above':[11],'unknown':None}}},10,20,self.spec['bounds'])['passed']);occluder['minimized']=True;self.assertTrue(scene.coverage({'windows':[occluder,own],'relations':{'10':{'above':[11],'unknown':None}}},10,20,self.spec['bounds'])['passed'])
        old={'windows':[occluder]};new={'windows':[{**occluder,'rect':[100,100,130,180]}]};self.assertTrue(scene.foreign_changes(old,new,20));self.assertEqual(occluder['rect'],[0,0,30,80]);self.assertEqual(scene.foreign_changes(old,old,20),[])
    def test_pixel_validation_rejects_occlusion_and_wrong_phase_marker(self):
        spec=self.spec;cache=scene.texture_cache(spec);phase=7;counter=23;w,h=spec['bounds'][2:]
        def pixel(x,y):
            if y==3:return spec['border_RGB'][0]
            if x==w-4:return spec['border_RGB'][1]
            if y==h-4:return spec['border_RGB'][2]
            if x==3:return spec['border_RGB'][3]
            if (x,y) in [(16,16),(w-17,16),(16,h-17),(w-17,h-17)]:return spec['corner_RGB'][[(16,16),(w-17,16),(16,h-17),(w-17,h-17)].index((x,y))]
            if y==96:
                bit=(x-1136)//48;v=224 if ((phase>>(bit%4))&1)^(bit//4) else 32;return [v]*3
            if y==302:
                band=(x-112)//160;return [(counter*3+band*37)%220+20,(band*41+counter)%220+20,(counter*2+band*31)%220+20]
            at=((y-768)*1024+x-1280)*4;return list(cache[phase][at:at+3][::-1])
        self.assertTrue(scene.validate_pixels(spec,pixel)['passed']);self.assertFalse(scene.validate_pixels(spec,lambda x,y:[0,0,0])['passed'])
    def test_readback_api_contract_and_allocation_failure_cleanup(self):
        class Fn:
            def __init__(self,fn):self.fn=fn
            def __call__(self,*a):return self.fn(*a)
        for failure in [None,'dc','mem','bitmap','select','copy','restore','scanlines']:
            with self.subTest(failure=failure):
                user=Mock();gdi=Mock();selected={20:1};events=[]
                user.GetDC=Fn(lambda *_:0 if failure=='dc' else 10)
                user.ReleaseDC=Fn(lambda *_:events.append('release') or 1)
                gdi.CreateCompatibleDC=Fn(lambda *_:0 if failure=='mem' else 20)
                gdi.CreateCompatibleBitmap=Fn(lambda *_:0 if failure=='bitmap' else 30)
                def select(dc,obj):
                    events.append(('select',obj))
                    if failure=='select' or (failure=='restore' and obj==1):return 0
                    old=selected[dc];selected[dc]=obj;return old
                def bits(dc,bm,*args):
                    self.assertNotIn(bm,selected.values());events.append('read');return 0 if failure=='scanlines' else 2160
                gdi.SelectObject=Fn(select);gdi.BitBlt=Fn(lambda *_:0 if failure=='copy' else 1);gdi.GetDIBits=Fn(bits)
                gdi.DeleteObject=Fn(lambda *_:events.append('bitmap') or 1);gdi.DeleteDC=Fn(lambda *_:events.append('mem') or 1)
                with patch.object(scene.C,'WinDLL',lambda n,*a,**k:user if n=='user32' else gdi,create=True),patch.object(scene,'validate_pixels',return_value={'passed':True}):
                    if failure:
                        with self.assertRaises(RuntimeError):scene.screen_probe(self.spec)
                    else:self.assertTrue(scene.screen_probe(self.spec)['passed'])
                if failure!='dc':self.assertEqual(events[-1],'release')
                if failure not in ['dc','mem','bitmap','restore']:self.assertLess(events.index('bitmap'),events.index('mem'))
                if failure=='restore':self.assertLess(events.index('mem'),events.index('bitmap'))

    def test_resource_cleanup_restores_selection_before_deletion_even_on_error_path(self):
        obj=object.__new__(scene.Renderer);obj.mem=1;obj.bitmap=2;obj.old=3;obj.gdi=Mock();calls=[];obj.gdi.SelectObject.side_effect=lambda *a:calls.append('restore');obj.gdi.DeleteObject.side_effect=lambda *a:calls.append('bitmap');obj.gdi.DeleteDC.side_effect=lambda *a:calls.append('dc');obj.close();obj.close();self.assertEqual(calls,['restore','bitmap','dc'])
if __name__=='__main__':unittest.main()
