#!/usr/bin/env python3
"""Offline, conservative identity join. Never used by packet/security handling.

Each report is one connection instance. Raw RTP headers precede authentication;
only a unique wire header AND unique authenticated primary read may be joined.
FEC-generated and RFC4588-recovered packets are deliberately absent from the
primary tap. Socket returns are API observations, never NIC timestamps.
"""
from collections import Counter, defaultdict


def identity(header):
    return tuple(header[name] for name in ('ssrc', 'rtp', 'seq', 'pt', 'marker'))


def analyze(report):
    wire = report['wire']
    received = [(index, row) for index, row in enumerate(wire)
                if row['direction'] == 0 and row['outcome'] == 0 and row['header'] is not None]
    counts = Counter(identity(row['header']) for _, row in received)
    authenticated = Counter(identity(row['header']) for row in report['primary'])
    unique = {identity(row['header']):(index, row) for index, row in received
              if counts[identity(row['header'])] == 1}
    frames = defaultdict(list)
    matched = ambiguous = unmatched = negative = 0
    for primary in report['primary']:
        key = identity(primary['header'])
        if counts[key] > 1 or authenticated[key] > 1:
            ambiguous += 1
            continue
        item = unique.get(key)
        if item is None:
            unmatched += 1
            continue
        index, packet = item
        if primary['read_us'] < packet['return_us']:
            negative += 1
            continue
        matched += 1
        frame = (primary['epoch'], primary['track'], key[0], key[1])
        frames[frame].append(dict(wire_index=index, header=primary['header'],
                                 socket_start_us=packet['start_us'], socket_return_us=packet['return_us'],
                                 previous_return_us=packet['previous_return_us'], read_us=primary['read_us'],
                                 pipeline_us=primary['pipeline_us'],
                                 socket_wait_us=packet['return_us']-packet['start_us'],
                                 recall_gap_us=max(0,packet['start_us']-packet['previous_return_us']) if packet['previous_return_us'] else None,
                                 queue_us=primary['read_us']-packet['return_us']))
    rows = []
    for (epoch, track, ssrc, rtp), packets in frames.items():
        packets.sort(key=lambda p:p['read_us'])
        rows.append(dict(epoch=epoch,track=track,ssrc=ssrc,rtp=rtp,matched_packets=len(packets),
                         first=packets[0],last=packets[-1],max_queue=max(packets,key=lambda p:p['queue_us']),
                         max_wait=max(packets,key=lambda p:p['socket_wait_us']),
                         max_recall=max(packets,key=lambda p:p['recall_gap_us'] or 0)))
    rows.sort(key=lambda r:r['first']['read_us'])
    sources = {r['header']['ssrc'] for r in report['primary']}
    return dict(instance=report['instance'], matched=matched, ambiguous=ambiguous, unmatched=unmatched,
                negative=negative, authenticated_primary=len(report['primary']), wire_rtp=len(received),
                wire_rtp_without_unique_authenticated_match=len(received)-matched,
                duplicate_wire_headers=sum(n-1 for n in counts.values()),
                unknown_ssrc_packets=sum(r['header']['ssrc'] not in sources for _,r in received),
                ignored_non_rtp=sum(r['header'] is None for r in wire),
                wire_evicted=report['wire_evicted'],primary_evicted=report['primary_evicted'],
                frames=rows,
                limitations=['Connection-local clock only; pre-SRTP identities remain unauthenticated until joined.',
                             'Ambiguous same-header original-SSRC retransmissions are excluded; no guess of earliest arrival.',
                             'Socket API return does not measure NIC arrival or isolate kernel/driver/scheduler.'])
