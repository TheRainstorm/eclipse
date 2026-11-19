#!/usr/bin/env python3
"""Attribute first RTP read to first draw on the client's monotonic clock.

Run with --group NAME TRACE [TRACE ...] for each comparable condition. Only
pool runs acquired under the same settings. Window selection uses first RTP
read in [start, end); later boundaries may fall outside that interval.
The first retained RTP read anchors each file independently.
"""
import argparse
from collections import Counter
import json
import math
from pathlib import Path


# Decode return belongs to the compressed input of that call. A decoder can
# return another input's image, so follow output PTS and the frozen mailbox ID.
BOUNDARIES = (
    'first_rtp_read', 'last_rtp_read', 'complete_au', 'decode_start',
    'output_available_by_pts', 'mailbox_published', 'mailbox_take', 'draw_callback',
)
STAGES = tuple(f'{a}..{b}' for a, b in zip(BOUNDARIES, BOUNDARIES[1:]))


def unsigned(value):
    return type(value) is int and value >= 0


def identities(row):
    """The trace attaches packet events by RTP, outputs by PTS and draws by row."""
    return (
        (row['id'],),
        (row['epoch'], row['track'], row['ssrc'], row['rtp']),
        (row['epoch'], row['track'], row['pts_us']),
    )


def read_trace(trace, skip_us, duration_us):
    if (trace['version'] != 1 or trace['event'] != 'video_pipeline_trace_end'
            or trace['clock'] != 'client_monotonic' or trace['complete'] is not True):
        raise ValueError('expected a complete v1 client_monotonic terminal pipeline trace')
    rows, names = trace['frames'], trace['stages']
    retained, evicted = trace['retained_frames'], trace['evicted_frames']
    if retained != len(rows) or trace['total_frames'] != retained + evicted:
        raise ValueError('inconsistent trace retention counters')
    if len(names) != len(set(names)):
        raise ValueError('ambiguous stage table')
    indices = [names.index(key) for key in BOUNDARIES]
    seen = [Counter() for _ in range(3)]
    packets = []
    for row in rows:
        if len(row['at_us']) != len(names):
            raise ValueError('timestamp vector length does not match the stage table')
        times = [row['at_us'][i] for i in indices]
        if any(t is not None and not unsigned(t) for t in times):
            raise ValueError('invalid timestamp')
        if times[0] is not None:
            packets.append(times[0])
        for counter, key in zip(seen, identities(row)):
            if all(unsigned(v) for v in key):
                counter[key] += 1
    if not packets:
        raise ValueError('trace contains no first RTP read')
    origin = min(packets)
    start = origin + skip_us
    end = start + duration_us if duration_us is not None else None
    accepted, rejected, missing, reversed_pairs = [], Counter(), Counter(), Counter()
    sources = Counter()
    window_rows = 0
    for row in rows:
        times = [row['at_us'][i] for i in indices]
        if times[0] is None or times[0] < start or (end is not None and times[0] >= end):
            continue
        window_rows += 1
        sources[(row['epoch'], row['track'], row['ssrc'])] += 1
        absent = [name for name, value in zip(BOUNDARIES, times) if value is None]
        missing.update(absent)
        inversions = [name for name, a, b in zip(STAGES, times, times[1:])
                      if a is not None and b is not None and b < a]
        reversed_pairs.update(inversions)
        keys = identities(row)
        if any(counter[key] > 1 for counter, key in zip(seen, keys)):
            rejected['ambiguous_identity'] += 1
        elif absent:
            rejected['missing_boundary'] += 1
        elif not unsigned(row['generation']) or any(not all(map(unsigned, key)) for key in keys):
            rejected['missing_identity'] += 1
        elif inversions:
            rejected['reversed_boundary'] += 1
        else:
            accepted.append(tuple(b - a for a, b in zip(times, times[1:])))
    coverage = {
        'trace_id': trace['trace_id'], 'total_frames': trace['total_frames'],
        'retained_frames': retained, 'evicted_frames': evicted,
        'unmatched_events_whole_trace': trace['unmatched_events'],
        'rows_without_window_anchor': retained - len(packets),
        'first_retained_rtp_us': origin, 'last_retained_rtp_us': max(packets),
        'window_start_us': start, 'window_end_exclusive_us': end,
        'window_rows': window_rows, 'accepted_frames': len(accepted),
        'window_sources': [{'epoch': e, 'track': t, 'ssrc': s, 'rows': n}
                           for (e, t, s), n in sources.items()],
        'rejected_rows': dict(rejected), 'rows_missing_each_boundary': dict(missing),
        'rows_reversed_each_boundary': dict(reversed_pairs),
    }
    return coverage, accepted


def quantile(ordered, fraction):
    """Linearly interpolate the ordered sample at (N-1)*fraction."""
    index = (len(ordered) - 1) * fraction
    lower = math.floor(index)
    return ordered[lower] + (ordered[math.ceil(index)] - ordered[lower]) * (index - lower)


def attribution(frames):
    totals = sorted(map(sum, frames))
    elapsed = sum(totals)
    parts = {}
    for index, name in enumerate(STAGES):
        time = sum(row[index] for row in frames)
        parts[name] = {
            'sum_us': time, 'mean_ms': time / (1000 * len(frames)) if frames else None,
            'share_pct': 100 * time / elapsed if elapsed else None,
        }
    return {
        'frames': len(frames), 'total_sum_us': elapsed,
        'total_mean_ms': elapsed / (1000 * len(frames)) if frames else None,
        'total_p50_ms': quantile(totals, .50) / 1000 if frames else None,
        'total_p95_ms': quantile(totals, .95) / 1000 if frames else None, 'stages': parts,
    }


def summary(frames):
    all_frames = attribution(frames)
    # Compare in the same microsecond units without a milliseconds round trip.
    threshold = quantile(sorted(map(sum, frames)), .95) if frames else None
    tail = [row for row in frames if sum(row) >= threshold]
    return {'all': all_frames, 'at_or_above_total_p95': attribution(tail)}


def analyze(groups, skip_us, duration_us):
    result = {
        'scope': 'Client monotonic API observations, first RTP read through first draw callback. '
                 'Wire transit, host work, GPU completion and panel display are outside this interval.',
        'method': 'Same-row complete monotone paths. Shares use sums of microseconds; '
                  'group summaries pool accepted frames. P50/P95 use linear interpolation. '
                  'Tail includes all ties at or above the group total P95. Unmatched events are whole-trace counts.',
        'skip_us': skip_us, 'duration_us': duration_us, 'groups': {},
    }
    trace_ids = set()
    for name, *paths in groups:
        if not paths or name in result['groups']:
            raise ValueError('each group requires a unique name and at least one trace')
        runs, pooled = [], []
        for path in map(Path, paths):
            try:
                coverage, frames = read_trace(json.loads(path.read_bytes()), skip_us, duration_us)
            except (ValueError, TypeError, KeyError, OSError) as error:
                raise ValueError(f'{path}: {error}') from error
            if coverage['trace_id'] in trace_ids:
                raise ValueError(f'{path}: trace counted more than once')
            trace_ids.add(coverage['trace_id'])
            runs.append({'file': str(path), 'coverage': coverage, 'summary': summary(frames)})
            pooled.extend(frames)
        result['groups'][name] = {'runs': runs, 'pooled': summary(pooled)}
    baseline, *others = result['groups']
    result['baseline_group'] = baseline
    deltas = {}
    for name in others:
        deltas[name] = {}
        for cohort, summary_base in result['groups'][baseline]['pooled'].items():
            comparison = result['groups'][name]['pooled'][cohort]
            changes = {}
            for stage in STAGES:
                before = summary_base['stages'][stage]['share_pct']
                after = comparison['stages'][stage]['share_pct']
                changes[stage] = None if before is None or after is None else after - before
            deltas[name][cohort] = changes
    result['share_delta_pp_from_baseline'] = deltas
    return result


def fmt(value):
    return '—' if value is None else f'{value:.3f}'


def print_table(result):
    print(result['scope'], result['method'], sep='\n')
    for name, group in result['groups'].items():
        print(f'\n{name}')
        for run in group['runs']:
            c = run['coverage']
            print(f"  {run['file']}: {c['accepted_frames']}/{c['window_rows']} selected, "
                  f"{c['rejected_rows']} excluded, {c['evicted_frames']} evicted, "
                  f"{c['unmatched_events_whole_trace']} unmatched events in whole trace")
        for cohort, value in group['pooled'].items():
            print(f"{cohort} (N={value['frames']}) mean/P50/P95 ms: " +
                  ' / '.join(fmt(value[key]) for key in ('total_mean_ms', 'total_p50_ms', 'total_p95_ms')))
        print(f"{'Stage':45} {'All ms':>9} {'All %':>9} {'Tail ms':>9} {'Tail %':>9}")
        for stage in STAGES:
            parts = [group['pooled'][key]['stages'][stage] for key in ('all', 'at_or_above_total_p95')]
            print(f'{stage:45}' + ''.join(f"{fmt(part[field]):>10}" for part in parts
                                        for field in ('mean_ms', 'share_pct')))
    for name, changes in result['share_delta_pp_from_baseline'].items():
        print(f"\nShare change from {result['baseline_group']} to {name} (percentage points)")
        print(f"{'Stage':45} {'All':>9} {'Tail':>9}")
        for stage in STAGES:
            print(f"{stage:45} {fmt(changes['all'][stage]):>9} {fmt(changes['at_or_above_total_p95'][stage]):>9}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--group', nargs='+', action='append', required=True, metavar='NAME_OR_TRACE')
    parser.add_argument('--skip', type=float, default=0, help='seconds from earliest retained first RTP read')
    parser.add_argument('--duration', type=float, help='window length in seconds (default: through final read)')
    parser.add_argument('--json', action='store_true', help='include per-run summaries and exact coverage')
    args = parser.parse_args()
    if not math.isfinite(args.skip) or args.skip < 0:
        parser.error('--skip must be finite and nonnegative')
    if args.duration is not None and (not math.isfinite(args.duration) or args.duration <= 0):
        parser.error('--duration must be finite and positive')
    try:
        result = analyze(args.group, round(args.skip * 1e6),
                         round(args.duration * 1e6) if args.duration is not None else None)
    except ValueError as error:
        parser.exit(2, f'{error}\n')
    if args.json:
        print(json.dumps(result, indent=2, allow_nan=False))
    else:
        print_table(result)


if __name__ == '__main__':
    main()
