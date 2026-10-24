#!/usr/bin/env python3
"""Alternate generated-loopback telemetry A/B; never operate a real desktop."""
import argparse
import json
import os
from pathlib import Path
import statistics
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--exe', type=Path, required=True, help='built frame_trace_probe executable')
    parser.add_argument('--output', type=Path, required=True, help='new result directory')
    parser.add_argument('--fps', type=int, default=120)
    parser.add_argument('--frames', type=int, default=600)
    parser.add_argument('--cycles', type=int, default=3)
    args = parser.parse_args()
    if not (30 <= args.fps <= 240 and max(120, args.fps * 2) <= args.frames <= 2400 and 1 <= args.cycles <= 6):
        parser.error('requires 30..240 FPS, at least two seconds / 120..2400 frames, and 1..6 cycles')
    binary = args.exe.resolve(strict=True)
    args.output.mkdir(mode=0o700, parents=True, exist_ok=False)
    criteria = {'steady_fps_max_relative_drop': 0.01, 'median_delivery_gap_p99_max_added_ms': 0.5}
    (args.output / 'criteria.json').write_text(json.dumps({'generated_only': True, **criteria}, indent=2) + '\n')
    rows = []
    for index, enabled in enumerate([True, False, False, True] * args.cycles):
        output = args.output / f'{index:02d}-{"on" if enabled else "off"}.json'
        environment = os.environ.copy()
        environment['ECLIPSE_FRAME_TRACE'] = '1' if enabled else '0'
        result = subprocess.run([str(binary), str(output), str(args.fps), str(args.frames)],
                                env=environment, capture_output=True, text=True, timeout=120)
        output.with_suffix('.log').write_text(result.stdout + result.stderr)
        if result.returncode:
            raise RuntimeError(f'generated run {index} failed; inspect its log')
        row = json.loads(output.read_text())
        assert row['generated_only'] and row['trace_enabled'] == enabled and row['decoded'] >= args.frames
        assert 'openh264' in row['decoder_backend'].lower(), 'comparison requires the explicit software decoder'
        rows.append(row)
        print(json.dumps({'run': index, 'enabled': enabled, 'steady_fps': row['steady_fps'],
                          'delivery_gap_p99_ms': row['decoded_delivery_gap_ms_p50_p95_p99_max'][2]}), flush=True)
    groups = {}
    for enabled in (False, True):
        group = [row for row in rows if row['trace_enabled'] == enabled]
        groups['on' if enabled else 'off'] = {
            'runs': len(group), 'decoded': sum(row['decoded'] for row in group),
            'steady_fps_median': statistics.median(row['steady_fps'] for row in group),
            'delivery_gap_p99_ms_median': statistics.median(row['decoded_delivery_gap_ms_p50_p95_p99_max'][2] for row in group),
            'mean_encode_us_median': statistics.median(row['mean_encode_us'] for row in group),
            'display_skips': sum(row['decoded_display_skips'] for row in group),
            'decoder_backends': sorted({row['decoder_backend'] for row in group}),
        }
    relative_drop = 1 - groups['on']['steady_fps_median'] / groups['off']['steady_fps_median']
    tail_added = groups['on']['delivery_gap_p99_ms_median'] - groups['off']['delivery_gap_p99_ms_median']
    passed = relative_drop <= criteria['steady_fps_max_relative_drop'] and tail_added <= criteria['median_delivery_gap_p99_max_added_ms']
    summary = {'generated_only': True, 'fps': args.fps, 'frames_per_run': args.frames,
               'criteria': criteria, 'groups': groups, 'relative_fps_drop': relative_drop,
               'added_p99_ms': tail_added, 'passed': passed,
               'scope': 'generated 320x180 software-codec loopback; mailbox publication, not GPU completion or actual display'}
    (args.output / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    print(json.dumps(summary), flush=True)
    if not passed:
        raise SystemExit(1)


if __name__ == '__main__':
    main()
