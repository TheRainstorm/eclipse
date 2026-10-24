import json
import tempfile
import unittest
from pathlib import Path
import frame_trace_report as tool


class ReportTests(unittest.TestCase):
    def test_json_probe_and_rotated_jsonl_select_latest_generation_without_other_metadata(self):
        first = {'version': 1, 'epoch': 1, 'context': {'generation': 1}, 'stages': {}, 'window_end_us': 10}
        latest = dict(first, window_end_us=20)
        next_generation = dict(first, context={'generation': 2}, window_end_us=30)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            probe = root / 'probe.json'
            probe.write_text(json.dumps({'unrelated': 'exclude', 'final': {'host_frame_trace': latest},
                                         'diagnostics': [{'event': 'host_frame_trace', 'report': first}]}))
            tail = root / 'tail.jsonl'
            tail.write_text('non-JSON CLI output\n' + json.dumps({'event': 'host_frame_trace', 'report': next_generation}) + '\n{"incomplete"')
            self.assertEqual(tool.summarize([probe, tail]), [latest, next_generation])

    def test_old_unknown_versions_and_unrelated_reports_remain_unknown(self):
        self.assertEqual(list(tool.reports({'event': 'native_host_preparation', 'timing': {'encode': 1}})), [])
        self.assertEqual(list(tool.reports({'version': 2, 'context': {}, 'stages': {}})), [])


if __name__ == '__main__':
    unittest.main()
