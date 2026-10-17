#!/usr/bin/env python3
"""UU Remote Linux controller prototype.

Validates the full signalling + WebRTC media path recovered from the
Windows client. Not the final Rust implementation; this is the reference
behaviour we port to `webrtc-rs`.

Usage:
    python proto.py /tmp/room.json [seconds]
"""
import asyncio, gzip, inspect, json, sys, time, uuid
import websockets
from aiortc import (RTCPeerConnection, RTCSessionDescription, RTCConfiguration,
                    RTCIceServer)

ROOM = json.load(open(sys.argv[1]))
D = ROOM["data"]
TOKEN = D["token"]
SECS = float(sys.argv[2]) if len(sys.argv) > 2 else 30
SIG = D["signaling_server"].rstrip("/") + "/socket.io/?EIO=4&transport=websocket"
HDR = {
    "X-NRD-AUTH": TOKEN,
    "streamer_flag": '{"sdp_flags":{"gzip_sdp":true}}',
    "streamer_version": "V4.7.3",
    "User-Agent": "WebSocket++/0.8.2",
}

class Sio:
    def __init__(self, ws): self.ws = ws
    async def emit(self, event, data, attachments=()):
        msg = f'45{len(attachments)}-' if attachments else '42'
        msg += json.dumps([event, data], separators=(',', ':'))
        await self.ws.send(msg)
        for a in attachments: await self.ws.send(a)
    async def recv_event(self):
        while True:
            m = await self.ws.recv()
            if isinstance(m, bytes):
                continue
            if m == '2':
                await self.ws.send('3'); continue
            if m.startswith('45'):
                rest = m[2:]
                i = rest.find('-')
                n = int(rest[:i]); payload = rest[i+1:]
                arr = json.loads(payload[payload.find('['):])
                atts = []
                for _ in range(n):
                    a = await self.ws.recv()
                    atts.append(a)
                return arr[0], arr[1], atts
            if m.startswith('42'):
                arr = json.loads(m[2:])
                return arr[0], arr[1], []
            if m.startswith('43'):
                rest = m[2:]
                i = rest.find('[')
                arr = json.loads(rest[i:])
                return arr[0], arr[1], []
            if m.startswith('44'):
                print("SOCKET_ERROR", m[2:], flush=True); continue
            print("SIO", repr(m)[:200], flush=True)

async def main():
    ctrl = str(uuid.uuid4())
    streamer = json.dumps({
        "control_id": ctrl,
        "device_capability": {
            "display_info": [{"id": 0, "fps": 60, "type": 0, "hdr": -2}],
            "video_codec_capability": [{
                "video_codec": 1, "width": 1920, "height": 1080,
                "chroma_sampling": 1, "bit_depth": 8, "codec_impl": 32}],
            "ice_id": "",
        },
    }, separators=(',', ':'))
    url = SIG + f"&t={int(time.time())}"
    kw = ({'additional_headers': HDR}
          if 'additional_headers' in inspect.signature(websockets.connect).parameters
          else {'extra_headers': HDR})
    async with websockets.connect(url, open_timeout=10, max_size=None, **kw) as ws:
        sio = Sio(ws)
        print("engine", (await ws.recv())[:120], flush=True)
        await ws.send('40')
        print("socket", (await ws.recv())[:160], flush=True)
        app = open('/tmp/app_data.bin', 'rb').read().replace(
            b'aeawvih7lubbnfna', b'aeawvuyzpabe2dlf')
        await sio.emit('control', {
            'app_control_id': ctrl,
            'app_data': {'_placeholder': True, 'num': 0},
            'geo_exit_ip': '36.159.243.209',
            'streamer_data': streamer,
        }, [app])
        print("control sent", flush=True)

        ice = None; target = None; ice_id = None
        while ice is None:
            ev, data, atts = await sio.recv_event()
            print("event", ev, str(data)[:200], flush=True)
            if ev == 'success':
                ice = data['iceServers']
                target = data.get('client_id'); ice_id = data.get('ice_id')
            elif ev == 'fail':
                print("FAIL", data, flush=True); return

        # Give the controlled side time to publish its capability + signal_app_data.
        for _ in range(3):
            try:
                ev, data, atts = await asyncio.wait_for(sio.recv_event(), 2)
                print('pre-event', ev, str(data)[:120], 'atts', [len(a) for a in atts], flush=True)
            except asyncio.TimeoutError:
                break

        servers = []
        for s in ice:
            servers.append(RTCIceServer(urls=s['urls'], username=s.get('username'),
                                        credential=s.get('credential')))
        print('creating pc', flush=True)
        pc = RTCPeerConnection(RTCConfiguration(iceServers=servers))

        @pc.on("connectionstatechange")
        async def state():
            print("pc state", pc.connectionState, flush=True)
            if pc.connectionState == "connected":
                await asyncio.sleep(2)
                await sio.emit('ice_finished_log', {
                    'app_control_id': ctrl, 'ice_id': ice_id})
                print("ice_finished_log sent", flush=True)

        vidx = [0]

        @pc.on("track")
        def track(t):
            print("track", t.kind, flush=True)
            if t.kind == "video":
                i = vidx[0]; vidx[0] += 1
                asyncio.ensure_future(save(t, i))

        async def save(t, i):
            n = 0
            try:
                while n < 300:
                    f = await t.recv()
                    if n == 0:
                        path = f'/tmp/uu_frame_{i}.png'
                        f.to_image().save(path)
                        print("saved", path, f.width, f.height, flush=True)
                    n += 1
            except Exception as e:
                print("track end", i, e, flush=True)

        for _ in range(5):
            pc.addTransceiver('video', direction='recvonly')
        pc.addTransceiver('audio', direction='recvonly')
        pc.createDataChannel('webrtc-datachannel')
        print('transceivers added', flush=True)
        off = await pc.createOffer()
        print('offer created', len(off.sdp), flush=True)
        await pc.setLocalDescription(off)
        print('local desc set', flush=True)
        # let ICE gather for a moment
        print('gathering', pc.iceGatheringState, flush=True)
        for _ in range(20):
            if pc.iceGatheringState == 'complete': break
            await asyncio.sleep(0.5)
        print('gathered', pc.iceGatheringState, flush=True)
        sdp = pc.localDescription.sdp
        open('/tmp/offer.sdp','w').write(sdp)
        print("offer sdp len", len(sdp), flush=True)
        gz = b'\x04' + gzip.compress(sdp.encode())
        await sio.emit('soac', {
            'client_id': target,
            'data': {
                'app_control_id': ctrl,
                'gzip_sdp': {'_placeholder': True, 'num': 0},
                'ice_id': ice_id,
                'ice_network_type': 0,
                'sdp': '',
                'type': 'offer',
            },
        }, [gz])
        print("offer sent", len(gz), flush=True)

        async def stats():
            await asyncio.sleep(8)
            try:
                for st in await pc.getStats():
                    if getattr(st, 'type', '') == 'inbound-rtp' and getattr(st, 'kind', '') == 'video':
                        print('STATS', st.ssrc, 'pkts', getattr(st,'packetsReceived',None),
                              'bytes', getattr(st,'bytesReceived',None),
                              'frames', getattr(st,'framesDecoded',None),
                              'lost', getattr(st,'packetsLost',None), flush=True)
            except Exception as e:
                print('stats err', e, flush=True)
        asyncio.ensure_future(stats())

        end = time.time() + SECS
        while time.time() < end:
            try:
                ev, data, atts = await asyncio.wait_for(sio.recv_event(), 20)
            except asyncio.TimeoutError:
                print("no event", flush=True); continue
            print("event", ev, str(data)[:160], flush=True)
            if ev == 'soac':
                d = data.get('data', {}) if isinstance(data, dict) else {}
                if d.get('type') == 'answer' and atts:
                    a0 = atts[0][1:] if atts[0][:1] == b'\x04' else atts[0]
                    raw = gzip.decompress(a0).decode()
                    open('/tmp/answer.sdp','w').write(raw)
                    print("ANSWER", raw[:300], flush=True)
                    await pc.setRemoteDescription(RTCSessionDescription(raw, 'answer'))
                elif d.get('type') == 'candidate':
                    c = d.get('candidate', {})
                    try:
                        from aiortc.sdp import candidate_from_sdp
                        cand = candidate_from_sdp(c['candidate'].split(':', 1)[1])
                        cand.sdpMid = c.get('sdpMid'); cand.sdpMLineIndex = c.get('sdpMLineIndex')
                        await pc.addIceCandidate(cand)
                    except Exception as e:
                        print("cand err", e, flush=True)
        await pc.close()

asyncio.run(main())
