"""Exercise the same report owner and close/finally order used on Windows."""
import ast
import json
from pathlib import Path
from types import SimpleNamespace
import tempfile
import unittest

import windows_latency_worker as worker


class FinalReports(unittest.TestCase):
    def test_close_then_finally_never_downgrades_the_final_file(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)/'stimulus.json'
            report = worker.StimulusReport(path, lambda: {'pid':7,'hwnd':11})
            for at in (100, 1_000_000, 1_010_000, 1_020_000):
                report.record(at)
            report.progress()
            light = json.loads(path.read_text())
            self.assertFalse(light['final_raw_timestamps'])
            self.assertEqual(light['completion_offsets_retained'], 4)
            self.assertEqual(light['completion_offsets_us'], [])
            # WM_DESTROY seals; the enclosing finally owns the only full write.
            try:
                report.close()
                self.assertIsNone(report.progress())
                with self.assertRaises(RuntimeError):report.record(1_030_000)
            finally:
                report.finalize()
            raw = path.read_bytes()
            for _ in range(3):
                report.close()
                report.progress()
                report.finalize('late cleanup cannot rewrite a terminal report')
            self.assertEqual(path.read_bytes(), raw)
            final = json.loads(raw)
            self.assertTrue(final['completed'])
            self.assertTrue(final['final_raw_timestamps'])
            self.assertEqual(final['completion_offsets_us'], [100,1_000_000,1_010_000,1_020_000])
            self.assertEqual(final['updates'], 4)
            self.assertEqual(final['paint_completion_gap_ms']['samples'], 2)
            self.assertEqual(final['paint_completion_gap_ms']['p99'], 10)
            self.assertEqual(final['steady_completed_hz'], 100)

    def test_exception_finally_retains_partial_and_zero_samples(self):
        for values in ([], [1_000_000,1_020_000]):
            with self.subTest(values=values), tempfile.TemporaryDirectory() as directory:
                path=Path(directory)/'stimulus.json'
                report=worker.StimulusReport(path, lambda:{})
                try:
                    for value in values:report.record(value)
                    raise RuntimeError('isolated failure')
                except RuntimeError as error:
                    report.close(error)
                finally:
                    report.finalize()
                final=json.loads(path.read_text())
                self.assertFalse(final['completed'])
                self.assertEqual(final['state'],'failed')
                self.assertEqual(final['error'],'isolated failure')
                self.assertEqual(final['completion_offsets_us'],values)
                self.assertTrue(final['final_raw_timestamps'])

    def test_write_failure_is_visible_and_sealed_data_can_be_retried(self):
        writes=[]
        def fail(path,value):
            writes.append(value)
            raise OSError('isolated write failure')
        report=worker.StimulusReport('unused.json',lambda:{},writer=fail)
        report.record(9)
        with self.assertRaisesRegex(OSError,'write failure'):report.finalize()
        self.assertIsNone(report.final)
        self.assertTrue(report.closed)
        with self.assertRaises(RuntimeError):report.record(10)
        report.writer=lambda path,value:writes.append(value)
        self.assertEqual(report.finalize()['completion_offsets_us'],[9])

    def test_overflow_keeps_obtained_data_and_fails_the_final_report(self):
        writes=[]
        report=worker.StimulusReport('unused.json',lambda:{},writer=lambda p,v:writes.append(v),capacity=1)
        report.record(8)
        try:report.record(9)
        except RuntimeError as error:report.close(error)
        final=report.finalize()
        self.assertEqual(final['completion_offsets_us'],[8])
        self.assertFalse(final['completed'])
        self.assertIn('overflow',final['error'])

    def test_actual_stimulus_finally_has_the_only_terminal_write(self):
        tree=ast.parse(Path(worker.__file__).read_text())
        stimulus=next(n for n in tree.body if isinstance(n,ast.FunctionDef) and n.name=='stimulus')
        finalizers=[n for n in ast.walk(stimulus) if isinstance(n,ast.Call) and isinstance(n.func,ast.Attribute) and n.func.attr=='finalize']
        self.assertEqual(len(finalizers),1)
        outer=next(n for n in stimulus.body if isinstance(n,ast.Try))
        self.assertTrue(any(n is finalizers[0] for statement in outer.finalbody for n in ast.walk(statement)))

    def test_product_stimulus_and_startup_failure_identity(self):
        product=SimpleNamespace(pid=101);stimulus=SimpleNamespace(pid=202)
        for child in reversed([stimulus,product]):
            pass
        self.assertEqual(worker.process_identity(product,stimulus,product),
                         dict(product_pid=101,stimulus_pid=202,monitored_pid=101))
        self.assertEqual(worker.process_identity(None,stimulus,stimulus),
                         dict(product_pid=None,stimulus_pid=202,monitored_pid=202))
        self.assertEqual(worker.process_identity(None,None,None),
                         dict(product_pid=None,stimulus_pid=None,monitored_pid=None))


if __name__=='__main__':unittest.main()
