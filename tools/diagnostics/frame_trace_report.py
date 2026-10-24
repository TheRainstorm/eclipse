#!/usr/bin/env python3
"""Summarize bounded host-frame observations from session JSONL or probe JSON."""
import argparse
import json
from pathlib import Path


def reports(value):
    if isinstance(value, list):
        for item in value:
            yield from reports(item)
    elif isinstance(value, dict):
        if value.get('version') == 1 and isinstance(value.get('context'), dict) and isinstance(value.get('stages'), dict):
            yield value
        elif value.get('event') == 'host_frame_trace':
            yield from reports(value.get('report'))
        else:
            for key in ('trace', 'host_frame_trace', 'diagnostics', 'final', 'samples'):
                if key in value:
                    yield from reports(value[key])


def load_reports(path):
    # Session files rotate at 256 KiB; generated probe reports are also bounded.
    if path.stat().st_size > 8 * 1024 * 1024:
        raise ValueError('input exceeds the 8 MiB diagnostic-file limit')
    content = path.read_text(encoding='utf-8-sig')
    try:
        yield from reports(json.loads(content))
    except json.JSONDecodeError:
        for line in content.splitlines():
            try:
                yield from reports(json.loads(line))
            except json.JSONDecodeError:
                continue  # Ignore non-JSON CLI output and incomplete final lines.


def summarize(paths):
    latest = {}
    for path in paths:
        for report in load_reports(path):
            key = (report['epoch'], report['context']['generation'])
            if key not in latest or report['window_end_us'] > latest[key]['window_end_us']:
                latest[key] = report
    return list(latest.values())


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('files', type=Path, nargs='+')
    parser.add_argument('--json', action='store_true', help='emit selected reports without input paths or unrelated session metadata')
    args = parser.parse_args()
    try:
        selected = summarize(args.files)
    except (ValueError, OSError, KeyError, TypeError) as error:
        parser.exit(2, f'Cannot read frame trace: {error}\n')
    if not selected:
        parser.exit(1, 'No negotiated host-frame trace found; update both ends and record a native session.\n')
    if args.json:
        print(json.dumps({'scope': 'host CPU observations and RTP API returns; no one-way network or display timing',
                          'reports': selected}, ensure_ascii=False, indent=2))
        return
    print('Host clock only. RTP write return is not wire delivery; GPU/display completion is unmeasured.')
    for report in selected:
        context = report['context']
        print(f"\nEpoch {report['epoch']} / generation {context['generation']}: "
              f"{context['width']}x{context['height']}, target {context['fps']} FPS; "
              f"codec={json.dumps(context['codec'])}, HDR={context['hdr']}")
        print('Backend: ' + json.dumps(context['backend'], ensure_ascii=False))
        print(f"Window {report['window_start_us']}..{report['window_end_us']} us; {report['window_frames']} captured observations")
        print('Lifetime counts: ' + json.dumps(report['counts'], sort_keys=True))
        print(f"{'Stage (ms)':25s} {'N':>5s} {'p50':>9s} {'p95':>9s} {'p99':>9s} {'p99.9':>9s} {'max':>9s}")
        for name, distribution in report['stages'].items():
            values = [distribution[key] / 1000 for key in ('p50_us', 'p95_us', 'p99_us', 'p999_us', 'max_us')]
            print(f"{name:25s} {distribution['samples']:5d}" + ''.join(f' {value:9.3f}' for value in values))
        print(f"Slow input ID: {report['slow_input_id']}; exported observations: {len(report['frames'])}")


if __name__ == '__main__':
    main()
