#!/usr/bin/env python3
"""Run immutable native-stream variants in ABBA order through a private machine adapter.

The adapter accepts --request JSON --output DIRECTORY and must save viewer.json,
host.log and worker.json there, including failure/cleanup evidence. No shell,
Git mutation, installation, automatic merge or release is performed by this tool.
"""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import re
import signal
import statistics
import subprocess
import time
import uuid


def write(path, value):
    temporary = path.with_suffix(path.suffix + '.partial')
    temporary.write_text(json.dumps(value, indent=2, allow_nan=False) + '\n')
    temporary.replace(path)


def digest(path):
    with Path(path).open('rb') as source:
        return hashlib.file_digest(source, 'sha256').hexdigest()


def implementation_files(config, config_path):
    files = config.get('implementation_files', [])
    if not files or not {'runner', 'adapter'}.issubset({f.get('role') for f in files}):
        raise ValueError('runner and private adapter must be frozen before execution')
    for item in files:
        item['path'] = str((config_path.parent / item['path']).resolve(strict=True))
        if not item.get('source') or not re.fullmatch('[0-9a-f]{64}', item.get('sha256', '')) or digest(item['path']) != item['sha256']:
            raise ValueError('implementation source/digest mismatch: ' + item.get('role', '?'))
    runner = next(item for item in files if item['role']=='runner')
    adapter = next(item for item in files if item['role']=='adapter')
    if Path(runner['path']) != Path(__file__).resolve() or adapter['path'] not in config['adapter']:
        raise ValueError('frozen runner/adapter do not match the executed entrypoints')
    return files


def steady_host_windows(viewer, final_context, final_epoch):
    """Independent clocks: client time selects a fresh anchor, then only host
    time + a duration define the conservative fence. Never subtract host/client
    timestamps. Require the entire rolling window to cross that fence.
    """
    warmup_us = viewer['configuration']['warmup_seconds'] * 1_000_000
    client_boundary_ms = (viewer.get('first_frame_ms') or 0) + warmup_us / 1000
    seen = set(); reports = []; anchor = None; previous_end = None
    for sample in viewer.get('samples', []):
        trace = sample.get('host_frame_trace')
        if not trace:
            continue
        if (trace.get('clock_domain') != 'host_monotonic' or trace.get('epoch') != final_epoch
                or trace.get('context') != final_context):
            raise ValueError('host trace clock/epoch/generation/configuration changed')
        start, end = trace.get('window_start_us'), trace.get('window_end_us')
        if not isinstance(start, int) or not isinstance(end, int) or start < 0 or start > end:
            raise ValueError('invalid host clock window')
        key = (trace['epoch'], end)
        if key in seen:
            continue
        if previous_end is not None and end <= previous_end:
            raise ValueError('host clock moved backwards')
        seen.add(key); previous_end = end
        reports.append(trace)
        if anchor is None and sample.get('at_ms', 0) >= client_boundary_ms:
            # Additional host-side warmup after this freshly observed anchor is
            # deliberately conservative; receive-time alone cannot exclude the
            # early frames still contained in a 512-frame rolling window.
            anchor = end
    fence = None if anchor is None else anchor + warmup_us
    valid = [t for t in reports if fence is not None and t['window_start_us'] > fence]
    intervals = sorted((t['window_start_us'], t['window_end_us']) for t in valid)
    merged = []
    for start, end in intervals:
        if merged and start <= merged[-1][1]:
            merged[-1][1] = max(merged[-1][1], end)
        else:
            merged.append([start, end])
    coverage = sum(end-start for start, end in merged)
    return valid, {'clock_domain': 'host_monotonic', 'epoch': final_epoch,
                   'generation': final_context.get('generation'), 'anchor_end_us': anchor,
                   'fence_us': fence, 'extra_host_warmup_us': warmup_us,
                   'observed_windows': len(reports), 'valid_windows': len(valid),
                   'excluded_windows': len(reports)-len(valid), 'coverage_us': coverage,
                   'coverage_intervals_us': merged}


def load_config(path):
    config = json.loads(path.read_text())
    if config.get('version') != 1 or not isinstance(config.get('adapter'), list) or not config['adapter']:
        raise ValueError('version 1 and an adapter argv array are required')
    if not all(isinstance(arg, str) and arg for arg in config['adapter']):
        raise ValueError('adapter arguments must be nonempty strings')
    implementation_files(config, path)
    variants = config['variants']
    if not 1 <= len(variants) <= 2 or 'baseline' not in variants or set(variants) - {'baseline', 'candidate'}:
        raise ValueError('specify baseline, and optionally candidate')
    for variant in variants.values():
        if not re.fullmatch('[0-9a-f]{40}', variant['revision']):
            raise ValueError('variants require immutable full Git revisions')
        if not variant.get('artifacts'):
            raise ValueError('verified artifacts are required')
        for artifact in variant['artifacts']:
            artifact['path'] = str((path.parent / artifact['path']).resolve(strict=True))
            if not re.fullmatch('[0-9a-f]{64}', artifact['sha256']) or digest(artifact['path']) != artifact['sha256']:
                raise ValueError('artifact SHA-256 mismatch')
    cases = config['cases']
    if not 1 <= len(cases) <= 12 or len({c['name'] for c in cases}) != len(cases):
        raise ValueError('specify 1..12 uniquely named cases')
    for case in cases:
        if not re.fullmatch('[a-zA-Z0-9_-]{1,64}', case['name']):
            raise ValueError('invalid case name')
        if not 5 <= case['seconds'] <= 3600 or not 1 <= case['warmup_seconds'] <= 30:
            raise ValueError('duration/warmup out of range')
        if case['source'] != 'desktop' or case['hdr'] is not False:
            raise ValueError('this first real-desktop lab requires explicit SDR desktop capture')
        if not 30 <= case['fps'] <= 240 or not 64 <= case['bitrate_kbps'] <= 1_000_000:
            raise ValueError('invalid frame rate or bitrate')
        if case['codec'] not in ('h264', 'hevc', 'av1') or not isinstance(case['vsync'], bool):
            raise ValueError('invalid codec or VSync')
        if len(case['size']) != 2 or any(not isinstance(v, int) or v < 2 or v > 8192 or v % 2 for v in case['size']):
            raise ValueError('invalid expected stream size')
        if not case.get('encoder_contains') or not case.get('decoder_contains') or not case.get('render_path'):
            raise ValueError('explicit actual encoder/decoder/render path expectations are required')
    criteria = config['criteria']
    for name in ('min_paint_fps_ratio', 'min_new_source_ratio', 'max_paint_gap_ms',
                 'max_mailbox_p99_ms', 'max_first_frame_ms', 'max_memory_growth_mb',
                 'fps_max_relative_drop', 'tail_max_added_ms', 'improvement_min_ms', 'min_host_coverage_seconds'):
        if not isinstance(criteria.get(name), (int, float)) or not math.isfinite(criteria[name]) or criteria[name] < 0:
            raise ValueError('missing or invalid criterion: ' + name)
    if not 0 < criteria['min_paint_fps_ratio'] <= 1 or not 0 < criteria['min_new_source_ratio'] <= 1:
        raise ValueError('coverage ratios must be in (0, 1]')
    if criteria.get('objective') not in ('host_ready_to_last_rtp_p99_window_median_ms', 'mailbox_to_draw_p99_ms'):
        raise ValueError('choose an actually measured optimization objective')
    return config


def summarize(output, request):
    worker = json.loads((output / 'worker.json').read_text())
    if not (output / 'viewer.json').is_file():
        raise RuntimeError(worker.get('error') or 'viewer did not create a report')
    viewer = json.loads((output / 'viewer.json').read_text())
    variant, case = request['variant'], request['case']
    if not viewer.get('completed') or not worker.get('completed'):
        return {'failures': [worker.get('error') or viewer.get('close_error') or 'native trial did not complete'],
                'metrics': {}, 'environment': worker.get('environment')}
    failures = []
    def require(condition, message):
        if not condition:
            failures.append(message)
    require(viewer.get('version') == 1 and viewer.get('completed') is True, 'viewer did not complete')
    require(worker.get('completed') is True and worker.get('cleanup_ok') is True, 'worker/owned-process cleanup failed')
    require(worker.get('revision') == variant['revision'], 'worker revision mismatch')
    expected_hashes = sorted(a['sha256'] for a in variant['artifacts'])
    require(sorted(worker.get('artifact_sha256', [])) == expected_hashes, 'worker artifact identity mismatch')
    environment = worker.get('environment')
    require(isinstance(environment, dict) and all(environment.get(key) for key in
            ('host_gpu', 'client_gpu', 'host_display', 'client_display', 'network', 'stimulus')), 'incomplete environment fingerprint')
    require(worker.get('desktop_capture') is True, 'not real desktop capture')
    require(worker.get('stimulus_updates', 0) >= case['seconds'] * case['fps'] * .5, 'insufficient desktop stimulus updates')
    require(isinstance(worker.get('stimulus_completed_hz'), (int,float)) and worker['stimulus_completed_hz'] >= case['fps']*.9,
            'desktop stimulus did not complete drawings at target cadence')
    source=environment.get('host_source', {}) if isinstance(environment,dict) else {}
    require(isinstance(source,dict) and source.get('name') and source.get('physical_bounds') and
            isinstance(source.get('hdr_enabled'),bool) and source.get('refresh_hz'), 'missing physical capture/color identity')
    require(viewer.get('errors') == 0, 'decoder errors')
    require(case['decoder_contains'].lower() in viewer.get('decoder_backend', '').lower(), 'wrong decoder / fallback')
    require(viewer.get('render_paths') == [case['render_path']], 'wrong renderer / fallback')
    require(viewer.get('coverage_ms', 0) >= (case['seconds'] - .5) * 1000, 'insufficient steady-state duration')
    configuration = viewer.get('configuration', {})
    for key, expected in (('fps', case['fps']), ('bounds', case['size']), ('bitrate_kbps', case['bitrate_kbps']),
                          ('codec', case['codec']), ('seconds', case['seconds']), ('warmup_seconds', case['warmup_seconds']), ('vsync', case['vsync'])):
        require(configuration.get(key) == expected, 'viewer configuration mismatch: ' + key)
    # Use host-side final report for lifetime identity, not the last unreliable
    # telemetry packet, whose counts can precede the end of the session.
    host_reports = []
    for line in (output / 'host.log').read_text(errors='replace').splitlines():
        try:
            value = json.loads(line)
            if value.get('event') == 'host_frame_trace':
                host_reports.append(value['report'])
        except (ValueError, TypeError):
            pass
    require(len(host_reports) == 1, 'missing/ambiguous final host trace')
    final = host_reports[0] if len(host_reports) == 1 else {}
    context, counts = final.get('context', {}), final.get('counts', {})
    require(context.get('codec') == case['codec'] and context.get('hdr') == case['hdr'], 'wrong codec/HDR')
    require([context.get('width'), context.get('height')] == case['size'], 'wrong stream geometry')
    require(context.get('fps') == case['fps'] and context.get('bitrate_kbps') == case['bitrate_kbps'], 'wrong host targets')
    require(case['encoder_contains'].lower() in context.get('backend', '').lower(), 'wrong encoder / fallback')
    require(counts.get('unknown_source', 1) == 0 and counts.get('captured', 0) > 0, 'missing real source identity')
    require(counts.get('unmatched_outputs', 1) == 0 and counts.get('pending_evictions', 1) == 0, 'lost host frame correlation')
    require(bool(final.get('frames')) and all(f.get('gpu_input') is True and f.get('source') for f in final.get('frames', [])), 'missing GPU/source proof')
    try:
        traces, host_coverage = steady_host_windows(viewer, context, final.get('epoch'))
    except ValueError as error:
        traces, host_coverage = [], {'coverage_us': 0, 'valid_windows': 0, 'error': str(error)}
        require(False, str(error))
    # Overlapping windows retain their identity. Only fully steady windows enter
    # the percentile summary, and union coverage is reported without double count.
    require(len(traces) >= max(2, case['seconds'] // 3), 'insufficient steady host telemetry windows')
    require(host_coverage['coverage_us'] >= request['criteria']['min_host_coverage_seconds'] * 1_000_000,
            'insufficient steady host telemetry coverage')
    decoded_samples = [sample for sample in viewer.get('samples', [])
                       if sample.get('at_ms',0) >= (viewer.get('first_frame_ms') or 0)+case['warmup_seconds']*1000]
    require(bool(decoded_samples) and all([sample.get('width'),sample.get('height')]==case['size'] for sample in decoded_samples),
            'decoded dimensions do not match the negotiated stream')
    gaps, mailbox = viewer.get('paint_gap_ms') or {}, viewer.get('mailbox_to_draw_ms') or {}
    require(bool(gaps) and bool(mailbox) and not gaps.get('overflow', True) and not mailbox.get('overflow', True), 'missing/overflowed paint measurements')
    def stage(name):
        values = [t['stages'][name]['p99_us'] / 1000 for t in traces if name in t['stages']]
        require(len(values) == len(traces) and bool(values), 'missing stage: ' + name)
        return statistics.median(values) if values else None
    metrics = {
        'stimulus_completed_hz': worker.get('stimulus_completed_hz'), 'paint_fps': viewer.get('paint_fps'), 'paint_gap_p99_ms': gaps.get('p99'),
        'paint_gap_max_ms': gaps.get('max'), 'paint_stalls_over_50ms': gaps.get('over_50ms'),
        'mailbox_to_draw_p99_ms': mailbox.get('p99'), 'first_frame_ms': viewer.get('first_frame_ms'),
        'new_source_ratio': counts.get('new_source', 0) / max(1, counts.get('captured', 0)),
        'host_encode_p99_window_median_ms': stage('encode_call'),
        'host_ready_to_last_rtp_p99_window_median_ms': stage('ready_to_last_rtp'),
        'host_queue_p99_window_median_ms': stage('au_to_dequeue'),
        'memory_growth_mb': worker.get('memory_growth_mb'),
    }
    require(all(isinstance(v, (int, float)) and math.isfinite(v) for v in metrics.values()), 'missing/nonfinite metrics')
    return {'failures': failures, 'metrics': metrics, 'environment': environment,
            'host_context': context, 'host_counts': counts, 'host_windows': len(traces), 'host_coverage': host_coverage,
            'scanout_measured': False, 'input_tested': False, 'audio_playback_tested': False}


def absolute_failures(row, case, criteria):
    m = row['metrics']; failures = list(row['failures'])
    if not m:
        return failures or ['missing measurements']
    for name, passes in (
        ('paint FPS below floor', m['paint_fps'] is not None and m['paint_fps'] >= case['fps'] * criteria['min_paint_fps_ratio']),
        ('insufficient unique desktop source frames', m['new_source_ratio'] >= criteria['min_new_source_ratio']),
        ('long paint stall', m['paint_gap_max_ms'] is not None and m['paint_gap_max_ms'] <= criteria['max_paint_gap_ms']),
        ('mailbox latency above limit', m['mailbox_to_draw_p99_ms'] is not None and m['mailbox_to_draw_p99_ms'] <= criteria['max_mailbox_p99_ms']),
        ('slow first frame', m['first_frame_ms'] is not None and m['first_frame_ms'] <= criteria['max_first_frame_ms']),
        ('resource growth', m['memory_growth_mb'] is not None and m['memory_growth_mb'] <= criteria['max_memory_growth_mb']),
    ):
        if not passes:
            failures.append(name)
    return failures


def compare(rows, cases, criteria, paired):
    results = {}
    for case in cases:
        group = [r for r in rows if r['case'] == case['name']]
        failures = [f"{r['run']}: {reason}" for r in group for reason in r['failures']]
        fingerprints = {json.dumps(r.get('environment'), sort_keys=True) for r in group}
        if not group:
            failures.append('no completed trials')
        elif len(fingerprints) != 1:
            failures.append('environment changed; comparison invalid')
        medians = {}
        for variant in ('baseline', 'candidate') if paired else ('baseline',):
            trials = [r for r in group if r['variant'] == variant and r.get('metrics')]
            if trials:
                medians[variant] = {name: statistics.median(r['metrics'][name] for r in trials)
                                    for name in trials[0]['metrics'] if all(isinstance(r['metrics'][name], (int, float)) for r in trials)}
        improved = False; difference = None; block_gains = []; blocks = []
        tails = ('paint_gap_p99_ms', 'mailbox_to_draw_p99_ms', 'host_ready_to_last_rtp_p99_window_median_ms')
        required = tuple(dict.fromkeys(('paint_fps', *tails, criteria['objective'])))
        if paired and set(medians) == {'baseline', 'candidate'} and all(
                name in medians[v] and math.isfinite(medians[v][name]) for v in medians for name in required):
            a, b = medians['baseline'], medians['candidate']
            if b['paint_fps'] < a['paint_fps'] * (1-criteria['fps_max_relative_drop']):
                failures.append('candidate throughput regression')
            for tail in tails:
                if b[tail] > a[tail] + criteria['tail_max_added_ms']:
                    failures.append('candidate tail regression: ' + tail)
            difference = a[criteria['objective']] - b[criteria['objective']]
            for block in sorted({r.get('cycle', 0) for r in group}):
                block_rows = [r for r in group if r.get('cycle', 0) == block]
                if ([r['variant'] for r in block_rows] != ['baseline', 'candidate', 'candidate', 'baseline']
                        or any(not all(isinstance(r.get('metrics', {}).get(name), (int, float))
                                       and math.isfinite(r['metrics'][name]) for name in required) for r in block_rows)):
                    failures.append(f'block {block}: incomplete or invalid ABBA measurements')
                    continue
                values = {v: {name: statistics.median(r['metrics'][name] for r in block_rows if r['variant'] == v)
                              for name in required} for v in ('baseline', 'candidate')}
                gain = values['baseline'][criteria['objective']] - values['candidate'][criteria['objective']]
                block_gains.append(gain)
                block_failures = []
                if values['candidate']['paint_fps'] < values['baseline']['paint_fps'] * (1-criteria['fps_max_relative_drop']):
                    block_failures.append('candidate throughput regression')
                for tail in tails:
                    if values['candidate'][tail] > values['baseline'][tail] + criteria['tail_max_added_ms']:
                        block_failures.append('candidate tail regression: ' + tail)
                failures.extend(f'block {block}: {reason}' for reason in block_failures)
                blocks.append({'cycle': block, 'medians': values, 'objective_improvement_ms': gain,
                               'failures': block_failures})
            improved = len(block_gains) >= 2 and all(gain >= criteria['improvement_min_ms'] for gain in block_gains)
        elif paired:
            failures.append('missing comparable trials')
        results[case['name']] = {'passed': not failures, 'improved': improved and not failures,
            'objective_improvement_ms': difference, 'paired_block_improvements_ms': block_gains, 'paired_blocks': blocks,
            'failures': failures, 'medians': medians}
    return results


def run_adapter(argv, request, output, timeout):
    with (output / 'adapter.log').open('wb') as log:
        child = subprocess.Popen([*argv, '--request', str(request), '--output', str(output)],
                                 stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT,
                                 start_new_session=(os.name == 'posix'))
        try:
            return child.wait(timeout=timeout)
        except BaseException:
            if os.name == 'posix':
                os.killpg(child.pid, signal.SIGTERM)
            else:
                child.terminate()
            try:
                child.wait(timeout=15)
            except subprocess.TimeoutExpired:
                if os.name == 'posix':
                    os.killpg(child.pid, signal.SIGKILL)
                else:
                    child.kill()
                child.wait(timeout=5)
            raise


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--config', required=True, type=Path)
    parser.add_argument('--output', required=True, type=Path, help='new private result directory')
    parser.add_argument('--cycles', type=int, default=2, help='1..12 ABBA blocks, or baseline reconnect trials')
    parser.add_argument('--stop-file', type=Path, help='finish current owned trial then stop; incomplete is never a pass')
    args = parser.parse_args()
    if not 1 <= args.cycles <= 12:
        parser.error('cycles must be 1..12')
    config_path = args.config.resolve(strict=True); config = load_config(config_path)
    os.umask(0o077); output = args.output.resolve(); output.mkdir(parents=True, exist_ok=False)
    # Freeze criteria and artifact identities before the first measured trial.
    write(output / 'experiment.json', {'config': config, 'cycles': args.cycles,
          'adapter_cwd': str(Path.cwd()), 'started_utc': time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime()),
          'runner_sha256': digest(__file__)})
    frozen = output / 'implementations'; frozen.mkdir()
    for index, item in enumerate(config['implementation_files']):
        (frozen / (str(index)+'-'+Path(item['path']).name)).write_bytes(Path(item['path']).read_bytes())
    paired = 'candidate' in config['variants']
    order = ['baseline', 'candidate', 'candidate', 'baseline'] if paired else ['baseline']
    rows = []; interrupted = False
    try:
        for cycle in range(args.cycles):
            for case in config['cases']:
                for variant in order:
                    if args.stop_file and args.stop_file.exists():
                        raise InterruptedError('stop file exists')
                    index = len(rows); run = f'{index:03d}-{case["name"]}-{variant}'
                    trial = output / run; trial.mkdir()
                    request = {'version': 1, 'run_id': uuid.uuid4().hex, 'variant': config['variants'][variant], 'case': case,
                               'adapter_options': config.get('adapter_options', {}), 'criteria': config['criteria'],
                               'implementation_files': config['implementation_files']}
                    implementation_files(config, config_path)
                    write(trial / 'request.json', request)
                    # Recheck immediately before staging; a build/install hook must
                    # never silently substitute the newest local executable.
                    for artifact in request['variant']['artifacts']:
                        if digest(artifact['path']) != artifact['sha256']:
                            raise ValueError('artifact changed after experiment freeze')
                    row = {'run': run, 'case': case['name'], 'variant': variant, 'cycle': cycle}
                    try:
                        code = run_adapter(config['adapter'], trial / 'request.json', trial,
                                           case['seconds'] + case['warmup_seconds'] + config.get('adapter_overhead_seconds', 180))
                        implementation_files(config, config_path)
                        row.update(summarize(trial, request))
                        row['measurement_valid'] = not row['failures']
                        row['failures'] = absolute_failures(row, case, config['criteria'])
                        if code:
                            row['failures'].append(f'adapter exit {code}')
                    except Exception as error:
                        row['failures'] = [f'{type(error).__name__}: {error}']
                    rows.append(row); write(trial / 'result.json', row)
                    write(output / 'progress.json', {'completed_trials': len(rows), 'latest': row})
                    print(json.dumps({'run': run, 'failures': row['failures'], 'metrics': row.get('metrics')}), flush=True)
                    # Infrastructure/cleanup failures stop further trials. A valid
                    # performance failure keeps its evidence and completes ABBA.
                    if not row.get('measurement_valid') or not json.loads((trial/'worker.json').read_text()).get('cleanup_ok'):
                        raise RuntimeError('invalid trial or cleanup; subsequent hardware work stopped')
    except (Exception, KeyboardInterrupt) as error:
        interrupted = True
        write(output / 'interruption.json', {'reason': f'{type(error).__name__}: {error}'})
    results = compare(rows, config['cases'], config['criteria'], paired)
    expected = args.cycles * len(config['cases']) * len(order)
    passed = not interrupted and len(rows) == expected and all(r['passed'] for r in results.values())
    summary = {'version': 1, 'passed': passed, 'interrupted': interrupted, 'trials': len(rows), 'expected_trials': expected,
               'candidate_accepted': paired and passed and all(r['improved'] for r in results.values()),
               'cases': results, 'scope': 'SDR real desktop, hardware codec and production texture test window; no scanout/input/audio acceptance'}
    write(output / 'summary.json', summary)
    print(json.dumps(summary), flush=True)
    return 0 if passed and (not paired or summary['candidate_accepted']) else 1


if __name__ == '__main__':
    raise SystemExit(main())
