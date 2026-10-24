"""Regression gates must fail closed on incomplete or incomparable hardware evidence."""
import hashlib
import copy
import json
from pathlib import Path
import tempfile
import unittest
import latency_lab as lab
from windows_latency_worker import stimulus_fps

CRITERIA = dict(min_paint_fps_ratio=.9, min_new_source_ratio=.7, max_paint_gap_ms=100,
                max_mailbox_p99_ms=30, max_first_frame_ms=3000, max_memory_growth_mb=128,
                fps_max_relative_drop=.02, tail_max_added_ms=1, improvement_min_ms=.25,
                objective='host_ready_to_last_rtp_p99_window_median_ms',min_host_coverage_seconds=2)
CASE = dict(name='desktop', seconds=10, warmup_seconds=1, source='desktop', hdr=False,
            fps=60, bitrate_kbps=20000, size=[1920,1080], codec='h264', vsync=False,
            encoder_contains='amf', decoder_contains='mf', render_path='d3d11-wgl-gpu')


class Gates(unittest.TestCase):
    def test_stimulus_rate_is_independent_and_validated(self):
        self.assertEqual(stimulus_fps(dict(fps=60)), 60)
        self.assertEqual(stimulus_fps(dict(fps=60, stimulus_fps=120)), 120)
        for invalid in (True, 29, 241, 120.5):
            with self.assertRaises(ValueError):
                stimulus_fps(dict(fps=60, stimulus_fps=invalid))

    def test_fast_stimulus_cannot_pass_with_stream_rate_drawings(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory); request,_,worker=self.fixture(root)
            request['case']={**CASE, 'stimulus_fps':120}
            self.assertIn('desktop stimulus did not complete drawings at target cadence',lab.summarize(root,request)['failures'])
            worker['stimulus_completed_hz']=120;worker['stimulus_updates']=1400
            (root/'worker.json').write_text(json.dumps(worker))
            self.assertEqual(lab.summarize(root,request)['failures'],[])

    def fixture(self, path):
        context = dict(generation=1,width=1920,height=1080,fps=60,bitrate_kbps=20000,codec='h264',hdr=False,backend='amf')
        def trace(end, ms, start=None):
            return dict(epoch=4,clock_domain='host_monotonic',window_start_us=max(0,end-4_000_000) if start is None else start,window_end_us=end,context=context,
                        counts=dict(captured=360,new_source=350,unknown_source=0,unmatched_outputs=0,pending_evictions=0),
                        frames=[dict(gpu_input=True,source={'frame_id':1})],
                        stages={name:dict(p99_us=ms*1000) for name in ('encode_call','ready_to_last_rtp','au_to_dequeue')})
        viewer = dict(version=1,completed=True,errors=0,decoder_backend='mf (d3d11)',render_paths=['d3d11-wgl-gpu'],
                      coverage_ms=9900,decoded_formats=[dict(visible_size=[1920,1080],hdr=False,gpu=True,color='Limited709',pixel_format='Nv12')],configuration={key:CASE[key] for key in ('fps','bitrate_kbps','codec','seconds','warmup_seconds','vsync')},
                      paint_fps=59.9,first_frame_ms=500,
                      paint_gap_ms=dict(p99=18,max=25,over_50ms=0,overflow=False),
                      mailbox_to_draw_ms=dict(p99=4,overflow=False),
                      samples=[dict(at_ms=at,width=1920,height=1080,host_frame_trace=trace(at*1000,ms)) for at,ms in [(2000,99),(8000,1),(9000,4),(10000,7)]])
        viewer['configuration']['bounds'] = CASE['size']
        worker = dict(completed=True,cleanup_ok=True,revision='a'*40,artifact_sha256=['b'*64],
                      desktop_capture=True,stimulus_updates=700,stimulus_completed_hz=60,memory_growth_mb=5,
                      environment={name:'fixed' for name in ('host_gpu','client_gpu','host_display','client_display','network','stimulus')})
        worker['environment']['host_source']=dict(name='owned-output',physical_bounds=[0,0,3840,2160],hdr_enabled=True,refresh_hz=60)
        (path/'viewer.json').write_text(json.dumps(viewer)); (path/'worker.json').write_text(json.dumps(worker))
        (path/'host.log').write_text(json.dumps(dict(event='host_frame_trace',report=trace(11_000_000,7)))+'\n')
        return dict(variant=dict(revision='a'*40,artifacts=[dict(sha256='b'*64)]),case=CASE,criteria=CRITERIA), viewer, worker

    def test_window_percentiles_are_not_pooled_and_valid_evidence_passes(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory); request,_,_=self.fixture(root)
            row=lab.summarize(root,request)
            self.assertEqual(row['metrics']['host_encode_p99_window_median_ms'],4)
            self.assertEqual(lab.absolute_failures(row,CASE,CRITERIA),[])
            self.assertFalse(row['scanout_measured'])

    def test_warmup_tail_in_late_snapshot_is_excluded_until_entire_window_crosses_fence(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory);request,viewer,_=self.fixture(root)
            context=viewer['samples'][0]['host_frame_trace']['context']
            late=copy.deepcopy(viewer['samples'][0]);late['at_ms']=7000
            late['host_frame_trace']['window_end_us']=7_000_000
            # Late receive, but window still contains the large startup sample.
            viewer['samples'].insert(1,late)
            traces,coverage=lab.steady_host_windows(viewer,context,4)
            self.assertEqual(coverage['fence_us'],3_000_000)
            self.assertEqual(len(traces),3)
            self.assertTrue(all(t['window_start_us']>coverage['fence_us'] for t in traces))
            self.assertTrue(all(t['stages']['encode_call']['p99_us']<99_000 for t in traces))
            late['host_frame_trace']['window_start_us']=coverage['fence_us']
            traces,boundary=lab.steady_host_windows(viewer,context,4)
            self.assertEqual(len(traces),3, 'a window starting exactly at the fence is excluded')

    def test_host_clock_epoch_generation_and_empty_coverage_fail_closed(self):
        for key,value in [('epoch',5),('clock_domain','client_monotonic')]:
            with self.subTest(key=key),tempfile.TemporaryDirectory() as directory:
                root=Path(directory);_,viewer,_=self.fixture(root)
                context=viewer['samples'][0]['host_frame_trace']['context']
                viewer['samples'][-1]['host_frame_trace'][key]=value
                with self.assertRaises(ValueError):lab.steady_host_windows(viewer,context,4)
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory);_,viewer,_=self.fixture(root)
            context=copy.deepcopy(viewer['samples'][0]['host_frame_trace']['context'])
            viewer['samples'][-1]['host_frame_trace']['context']['generation']=2
            with self.assertRaises(ValueError):lab.steady_host_windows(viewer,context,4)
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory);request,viewer,_=self.fixture(root)
            for sample in viewer['samples']:sample['host_frame_trace']['window_start_us']=0
            (root/'viewer.json').write_text(json.dumps(viewer))
            row=lab.summarize(root,request)
            self.assertEqual(row['host_windows'],0)
            self.assertIsNone(row['metrics']['host_encode_p99_window_median_ms'])
            self.assertTrue(row['failures'])

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

    def test_opposing_tail_regressions_cannot_cancel_between_abba_blocks(self):
        rows=self.rows()
        for row in rows:
            if row['variant']=='candidate':
                row['metrics'][CRITERIA['objective']]=7
                row['metrics']['paint_gap_p99_ms']=20 if row['cycle']==0 else 14
        # Pooled medians look unchanged and both objective gains are positive;
        # block zero still incurs a 3ms tail regression and must reject.
        result=lab.compare(rows,[CASE],CRITERIA,True)['desktop']
        self.assertEqual(result['medians']['candidate']['paint_gap_p99_ms'],17)
        self.assertEqual(result['paired_block_improvements_ms'],[1,1])
        self.assertFalse(result['passed'])
        self.assertTrue(any('block 0: candidate tail regression' in f for f in result['failures']))

    def test_opposing_throughput_changes_cannot_cancel_between_abba_blocks(self):
        rows=self.rows()
        for row in rows:
            if row['variant']=='candidate':
                row['metrics'][CRITERIA['objective']]=7
                row['metrics']['paint_fps']=56 if row['cycle']==0 else 64
        result=lab.compare(rows,[CASE],CRITERIA,True)['desktop']
        self.assertEqual(result['medians']['candidate']['paint_fps'],60)
        self.assertFalse(result['improved'])
        self.assertIn('block 0: candidate throughput regression',result['failures'])

    def test_incomplete_abba_and_missing_tail_fail_instead_of_crashing(self):
        rows=self.rows();del rows[-1]
        result=lab.compare(rows,[CASE],CRITERIA,True)['desktop']
        self.assertFalse(result['passed'])
        self.assertTrue(any('incomplete' in f for f in result['failures']))
        criteria={**CRITERIA,'objective':'mailbox_to_draw_p99_ms'}
        rows=self.rows()
        for row in rows:del row['metrics']['host_ready_to_last_rtp_p99_window_median_ms']
        self.assertFalse(lab.compare(rows,[CASE],criteria,True)['desktop']['passed'])

    def test_environment_change_and_failed_trials_cannot_be_accepted(self):
        rows=self.rows();rows[-1]['environment']={'fixed':2}
        self.assertFalse(lab.compare(rows,[CASE],CRITERIA,True)['desktop']['passed'])
        rows=self.rows();rows[1]['failures']=['decoder error']
        self.assertFalse(lab.compare(rows,[CASE],CRITERIA,True)['desktop']['passed'])

    def test_mutated_artifact_is_rejected_before_running_adapter(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory);(root/'payload').write_bytes(b'fixed')
            config=dict(version=1,adapter=['python3',str(Path(lab.__file__).resolve())],variants={'baseline':dict(revision='a'*40,
                        artifacts=[dict(path='payload',sha256=hashlib.sha256(b'fixed').hexdigest())])},cases=[CASE],criteria=CRITERIA,
                        implementation_files=[dict(role=role,path=str(Path(lab.__file__).resolve()),sha256=lab.digest(lab.__file__),source='fixed test file') for role in ('runner','adapter')])
            path=root/'config.json';path.write_text(json.dumps(config));lab.load_config(path)
            (root/'payload').write_bytes(b'changed')
            with self.assertRaisesRegex(ValueError,'SHA-256 mismatch'):lab.load_config(path)


if __name__=='__main__':unittest.main()
