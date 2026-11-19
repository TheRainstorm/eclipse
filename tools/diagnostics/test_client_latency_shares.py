import json
from pathlib import Path
import tempfile
import unittest

import client_latency_shares as tool


def row(number, times, **changes):
    # An unattached input-return / native-select stage may be anywhere in the table.
    return dict(id=number, epoch=1, track=0, ssrc=5, rtp=number,
                generation=0, pts_us=number * 10, at_us=[None, *times]) | changes


def trace(rows):
    return {'event': 'video_pipeline_trace_end', 'version': 1, 'trace_id': 'sample',
            'clock': 'client_monotonic', 'complete': True, 'stages': ['decode_return', *tool.BOUNDARIES],
            'frames': rows, 'retained_frames': len(rows), 'evicted_frames': 3,
            'total_frames': len(rows) + 3, 'unmatched_events': 17}


class LatencySharesTests(unittest.TestCase):
    def test_interval_observes_starting_packet_even_if_draw_after_end(self):
        data = trace([row(3, range(28, 36)), row(1, range(10, 18)), row(2, range(20, 28))])
        coverage, frames = tool.read_trace(data, 10, 5)
        self.assertEqual((coverage['window_start_us'], coverage['window_end_exclusive_us']), (20, 25))
        self.assertEqual((coverage['evicted_frames'], coverage['unmatched_events_whole_trace']), (3, 17))
        self.assertEqual(frames, [(1,) * 7])

    def test_rejects_incomplete_reversed_or_ambiguous_images_without_splicing(self):
        rows = [row(1, range(8)), row(2, [30, 31, 29, 33, 34, 35, 36, 37]),
                row(3, [40, 41, 42, 43, None, 45, 46, 47]),
                row(4, range(50, 58), pts_us=70), row(7, range(60, 68)),
                row(8, range(70, 78), generation=None),
                row(9, [None, 81, 82, 83, 84, 85, 86, 87])]
        coverage, frames = tool.read_trace(trace(rows), 0, None)
        self.assertEqual(frames, [(1,) * 7])
        self.assertEqual(coverage['rejected_rows'], dict(reversed_boundary=1, missing_boundary=1,
                                                       ambiguous_identity=2, missing_identity=1))
        self.assertEqual(coverage['rows_without_window_anchor'], 1)
        self.assertEqual(coverage['rows_missing_each_boundary'], {'output_available_by_pts': 1})
        self.assertEqual(coverage['rows_reversed_each_boundary'], {tool.STAGES[1]: 1})

    def test_time_weighted_shares_and_tied_tail_use_same_cohort(self):
        frames = [(10, 0)] * 18 + [(0, 90)] * 2
        frames = [(*f, 0, 0, 0, 0, 0) for f in frames]
        values = tool.summary(frames)
        all_rows, tail = values['all'], values['at_or_above_total_p95']
        self.assertEqual(all_rows['stages'][tool.STAGES[0]]['share_pct'], 50)
        self.assertEqual(all_rows['total_mean_ms'], .018)
        self.assertEqual((all_rows['total_p50_ms'], all_rows['total_p95_ms']), (.010, .090))
        self.assertEqual(tail['frames'], 2)
        self.assertEqual(tail['stages'][tool.STAGES[1]]['share_pct'], 100)
        for value in values.values():
            self.assertEqual(sum(p['sum_us'] for p in value['stages'].values()), value['total_sum_us'])
            self.assertEqual(sum(p['share_pct'] for p in value['stages'].values()), 100)

    def test_unavailable_observations_and_zero_denominator(self):
        self.assertIsNone(tool.summary([])['all']['total_p95_ms'])
        zero = tool.summary([(0,) * 7])
        self.assertEqual(zero['at_or_above_total_p95']['frames'], 1)
        self.assertIsNone(zero['all']['stages'][tool.STAGES[0]]['share_pct'])
        for changed in ({'clock': 'host_monotonic'}, {'complete': False}, {'retained_frames': 1}):
            with self.assertRaises(ValueError):
                tool.read_trace(trace([]) | changed, 0, None)

    def test_group_comparison_is_in_percentage_points_and_never_reuses_trace(self):
        with tempfile.TemporaryDirectory() as tmp:
            first, second = [Path(tmp) / f'{i}.json' for i in (1, 2)]
            first.write_text(json.dumps(trace([row(1, [0, 12, 12, 12, 12, 12, 12, 12])])))
            second.write_text(json.dumps(trace([row(1, [800, 803, 812, 812, 812, 812, 812, 812])])
                                         | {'trace_id': 'other'}))
            groups = [['a', first], ['b', second]]
            result = tool.analyze(groups, 0, None)
            self.assertEqual(result['share_delta_pp_from_baseline']['b']['all'][tool.STAGES[0]], -75)
            with self.assertRaisesRegex(ValueError, 'counted more than once'):
                tool.analyze([*groups, ['c', first]], 0, None)


if __name__ == '__main__':
    unittest.main()
