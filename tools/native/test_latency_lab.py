"""Regression gates must fail closed on incomplete or incomparable hardware evidence."""
import hashlib
import json
from pathlib import Path
import tempfile
import unittest
import latency_lab as lab

CRITERIA = dict(min_paint_fps_ratio=.9, min_new_source_ratio=.7, max_paint_gap_ms=100,
                max_mailbox_p99_ms=30, max_first_frame_ms=3000, max_memory_growth_mb=128,
                fps_max_relative_drop=.02, tail_max_added_ms=1, improvement_min_ms=.25,
                objective='host_ready_to_last_rtp_p99_window_median_ms')
CASE = dict(name='desktop', seconds=6, warmup_seconds=1, source='desktop', hdr=False,
            fps=60, bitrate_kbps=20000, size=[1920,1080], codec='h264', vsync=False,
            encoder_contains='amf', decoder_contains='mf', render_path='d3d11-wgl-gpu')


class Gates(unittest.TestCase):
    def fixture(self, path):
        context = dict(generation=1,width=1920,height=1080,fps=60,bitrate_kbps=20000,codec='h264',hdr=False,backend='amf')
        def trace(end, ms):
            return dict(epoch=4,window_end_us=end,context=context,
                        counts=dict(captured=360,new_source=350,unknown_source=0,unmatched_outputs=0,pending_evictions=0),
                        frames=[dict(gpu_input=True,source={'frame_id':1})],
                        stages={name:dict(p99_us=ms*1000) for name in ('encode_call','ready_to_last_rtp','au_to_dequeue')})
        viewer = dict(version=1,completed=True,errors=0,decoder_backend='mf (d3d11)',render_paths=['d3d11-wgl-gpu'],
                      coverage_ms=5900,configuration={key:CASE[key] for key in ('fps','bitrate_kbps','codec','seconds','warmup_seconds','vsync')},
                      paint_fps=59.9,first_frame_ms=500,
                      paint_gap_ms=dict(p99=18,max=25,over_50ms=0,overflow=False),
                      mailbox_to_draw_ms=dict(p99=4,overflow=False),
                      samples=[dict(at_ms=at,host_frame_trace=trace(at*1000,ms)) for at,ms in [(2000,1),(3000,4),(4000,7)]])
        viewer['configuration']['bounds'] = CASE['size']
        worker = dict(completed=True,cleanup_ok=True,revision='a'*40,artifact_sha256=['b'*64],
                      desktop_capture=True,stimulus_updates=400,memory_growth_mb=5,
                      environment={name:'fixed' for name in ('host_gpu','client_gpu','host_display','client_display','network','stimulus')})
        (path/'viewer.json').write_text(json.dumps(viewer)); (path/'worker.json').write_text(json.dumps(worker))
        (path/'host.log').write_text(json.dumps(dict(event='host_frame_trace',report=trace(5_000_000,7)))+'\n')
        return dict(variant=dict(revision='a'*40,artifacts=[dict(sha256='b'*64)]),case=CASE), viewer, worker

    def test_window_percentiles_are_not_pooled_and_valid_evidence_passes(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory); request,_,_=self.fixture(root)
            row=lab.summarize(root,request)
            self.assertEqual(row['metrics']['host_encode_p99_window_median_ms'],4)
            self.assertEqual(lab.absolute_failures(row,CASE,CRITERIA),[])
            self.assertFalse(row['scanout_measured'])

    def test_cleanup_fallback_missing_trace_and_overflow_fail_closed(self):
        mutations=[('worker','cleanup_ok',False),('viewer','decoder_backend','software'),
                   ('viewer','render_paths',['cpu-planar-upload']),('viewer','completed',False),
                   ('viewer','mailbox_to_draw_ms',dict(p99=1,overflow=True))]
        for target,key,value in mutations:
            with self.subTest(target=target,key=key),tempfile.TemporaryDirectory() as directory:
                root=Path(directory); request,viewer,worker=self.fixture(root)
                document=worker if target=='worker' else viewer; document[key]=value
                (root/(target+'.json')).write_text(json.dumps(document))
                self.assertTrue(lab.summarize(root,request)['failures'])
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory); request,_,_=self.fixture(root); (root/'host.log').write_text('connection failed\n')
            self.assertTrue(lab.summarize(root,request)['failures'])

    def rows(self):
        metrics=dict(paint_fps=60,paint_gap_p99_ms=17,mailbox_to_draw_p99_ms=4,host_ready_to_last_rtp_p99_window_median_ms=8)
        return [dict(run=str(i),case='desktop',variant=v,cycle=i//4,metrics={**metrics},failures=[],environment={'fixed':1})
                for i,v in enumerate(('baseline','candidate','candidate','baseline')*2)]

    def test_only_measured_improvement_passes_and_tail_tradeoff_is_rejected(self):
        rows=self.rows()
        self.assertFalse(lab.compare(rows,[CASE],CRITERIA,True)['desktop']['improved'])
        for row in rows:
            if row['variant']=='candidate':row['metrics'][CRITERIA['objective']]-=1
        self.assertTrue(lab.compare(rows,[CASE],CRITERIA,True)['desktop']['improved'])
        for row in rows:
            if row['variant']=='candidate':row['metrics']['mailbox_to_draw_p99_ms']=10
        self.assertFalse(lab.compare(rows,[CASE],CRITERIA,True)['desktop']['passed'])

    def test_environment_change_and_failed_trials_cannot_be_accepted(self):
        rows=self.rows();rows[-1]['environment']={'fixed':2}
        self.assertFalse(lab.compare(rows,[CASE],CRITERIA,True)['desktop']['passed'])
        rows=self.rows();rows[1]['failures']=['decoder error']
        self.assertFalse(lab.compare(rows,[CASE],CRITERIA,True)['desktop']['passed'])

    def test_mutated_artifact_is_rejected_before_running_adapter(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory);(root/'payload').write_bytes(b'fixed')
            config=dict(version=1,adapter=['unused'],variants={'baseline':dict(revision='a'*40,
                        artifacts=[dict(path='payload',sha256=hashlib.sha256(b'fixed').hexdigest())])},cases=[CASE],criteria=CRITERIA)
            path=root/'config.json';path.write_text(json.dumps(config));lab.load_config(path)
            (root/'payload').write_bytes(b'changed')
            with self.assertRaisesRegex(ValueError,'SHA-256 mismatch'):lab.load_config(path)


if __name__=='__main__':unittest.main()
