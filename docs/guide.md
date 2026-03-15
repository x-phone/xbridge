# xbridge Integration Guide

Build AI voice agents, IVRs, and call center apps with xbridge — a self-hosted voice gateway that bridges SIP calls to WebSocket audio and a REST API.

This guide walks you through building a complete voice agent from scratch. All examples use Python with FastAPI, but xbridge works with any language that speaks HTTP and WebSocket.

> **Want to see it working first?** Run the [Voice AI Demo](../demo/voice-ai/) — a full-stack example with softphone, PBX, xbridge, and a Deepgram-powered AI agent with live transcription. One `docker compose up` and you're talking to an AI on the phone.

## Table of Contents

- [Architecture Overview](#architecture-overview)
- [1. Choose a Connection Mode](#1-choose-a-connection-mode)
- [2. Start xbridge](#2-start-xbridge)
- [3. Handle Incoming Calls](#3-handle-incoming-calls)
- [4. Stream Audio via WebSocket](#4-stream-audio-via-websocket)
- [5. Send Audio Back (TTS)](#5-send-audio-back-tts)
- [6. Make Outbound Calls](#6-make-outbound-calls)
- [7. Call Control (Hold, Transfer, DTMF)](#7-call-control-hold-transfer-dtmf)
- [8. Play Audio Prompts](#8-play-audio-prompts)
- [9. Monitor with Webhooks](#9-monitor-with-webhooks)
- [10. Production Checklist](#10-production-checklist)
- [Migrating from Twilio](#migrating-from-twilio)

---

## Architecture Overview

xbridge connects your app to the phone network. Your app does two things:

1. **Webhook handler** — receives HTTP POSTs from xbridge when calls arrive, end, etc.
2. **WebSocket client** — connects to xbridge to send/receive real-time audio.

```
                    ┌──────────────────────────────────┐
                    │         Your Voice App            │
                    │  (webhook handler + WS client)    │
                    └──────────┬───────────┬────────────┘
                               │           │
                    webhooks   │           │  WebSocket
                   (HTTP POST) │           │  (audio stream)
                               │           │
                    ┌──────────▼───────────▼────────────┐
                    │           xbridge                  │
                    │     (SIP ↔ WebSocket gateway)      │
                    │                                    │
 ┌──────────┐      │  ┌────────────┐  ┌──────────────┐  │
 │  PBX     │◀─SIP─┤  │ Trunk Host │  │ SIP Client   │  ├─SIP──▶ ┌──────────┐
 │ Asterisk │  :5080│  │ (server)   │  │ (register)   │  │        │ Telnyx   │
 │ xpbx     │──SIP─▶  │            │  │              │  ◀─SIP──┤ VoIP.ms  │
 └──────────┘      │  └────────────┘  └──────────────┘  │        └──────────┘
                    └───────────────────────────────────┘
```

All three connection modes deliver the same experience to your app — same webhooks, same WebSocket protocol, same REST API.

---

## 1. Choose a Connection Mode

xbridge supports three ways to connect to the phone network. Pick the one that fits your setup.

### SIP Extension (register on a PBX)

The simplest mode. xbridge registers on your existing PBX as an extension — just like a softphone. Calls to that extension trigger your webhooks and audio streams.

**Use this when:** you have an existing PBX (Asterisk, FreePBX, 3CX, xpbx) and want to add a voice AI or IVR on a specific extension.

```yaml
sip:
  username: "2000"
  password: "secret"
  host: "192.168.1.10"       # your PBX address
  rtp_port_min: 10000
  rtp_port_max: 10100

webhook:
  url: "http://your-app:3000"
stream:
  encoding: "audio/x-l16"
  sample_rate: 8000
```

This is what the [Voice AI Demo](../demo/voice-ai/) uses (via trunk host mode, but the extension approach is even simpler for a single PBX).

### SIP Trunk Client (cloud provider)

Register with a SIP trunk provider to get a real phone number (DID). Calls to that number trigger your webhooks.

**Use this when:** you want a real phone number without running a PBX, or you're replacing Twilio.

```yaml
sip:
  username: "your-username"
  password: "your-password"
  host: "sip.telnyx.com"
  transport: "tls"
  srtp: true

webhook:
  url: "http://your-app:3000"
stream:
  encoding: "audio/x-mulaw"
  sample_rate: 8000
```

For multiple providers, use the `trunks` array instead of `sip`:

```yaml
trunks:
  - name: "telnyx"
    username: "1001"
    password: "secret1"
    host: "sip.telnyx.com"
    transport: "tls"
    srtp: true
  - name: "twilio"
    username: "2001"
    password: "secret2"
    host: "sip.twilio.com"
```

### SIP Trunk Host (accept calls from PBX)

Run a SIP server that PBX systems connect to directly. Your PBX sends calls to xbridge over a SIP trunk.

**Use this when:** you want xbridge to act as a trunk endpoint — PBX systems or cloud trunk providers send SIP INVITEs directly to it.

```yaml
server:
  listen: "0.0.0.0:5080"
  peers:
    - name: "office-pbx"
      host: "192.168.1.10"        # IP-based auth
      codecs: ["ulaw", "alaw"]
    - name: "remote-office"
      auth:                       # SIP digest auth
        username: "remote-trunk"
        password: "secret"

webhook:
  url: "http://your-app:3000"
stream:
  encoding: "audio/x-l16"
  sample_rate: 8000
```

### All three modes can run simultaneously

You can register as a SIP extension, connect to a cloud trunk, and accept direct PBX connections — all in the same config file. See [`config.example.yaml`](../config.example.yaml) for all options.

---

## 2. Start xbridge

Create `config.yaml` using one of the connection modes above, then start xbridge:

```bash
# Docker
docker run -v ./config.yaml:/etc/xbridge/config.yaml \
  -p 8080:8080 -p 10000-10100:10000-10100/udp \
  ghcr.io/x-phone/xbridge:latest

# Or from source
RUST_LOG=info cargo run --release -- --config config.yaml
```

Verify it's running:

```bash
curl http://localhost:8080/health
# {"status":"ok","sip_trunks":1,"sip_server":false,"active_calls":0}
```

---

## 3. Handle Incoming Calls

When a call arrives, xbridge POSTs to `{webhook_url}/incoming`. Your app decides whether to accept or reject.

```python
# app.py
from fastapi import FastAPI
import uvicorn

app = FastAPI()

@app.post("/incoming")
async def handle_incoming(call: dict):
    print(f"Incoming call from {call['from']} to {call['to']}")

    # Accept the call and enable audio streaming
    return {"action": "accept", "stream": True}

    # Or reject:
    # return {"action": "reject", "reason": "busy"}

if __name__ == "__main__":
    uvicorn.run(app, host="0.0.0.0", port=3000)
```

Once you return `{"action": "accept"}`, xbridge answers the SIP call. Audio becomes available via WebSocket after the `call.answered` event fires.

For calls from trunk host peers, the webhook includes a `peer` field:

```python
@app.post("/incoming")
async def handle_incoming(call: dict):
    peer = call.get("peer")  # "office-pbx", or None for cloud trunk / extension

    if peer:
        print(f"Call from PBX peer {peer}: {call['from']} → {call['to']}")
    else:
        print(f"Call from trunk: {call['from']} → {call['to']}")

    return {"action": "accept", "stream": True}
```

---

## 4. Stream Audio via WebSocket

Connect to the WebSocket to receive caller audio in real-time. xbridge supports two modes.

### Twilio-Compatible Mode (default)

JSON text frames with base64-encoded audio. Drop-in compatible with apps built for Twilio Media Streams.

```python
import asyncio
import json
import websockets
import base64

async def handle_audio_stream(call_id: str):
    uri = f"ws://localhost:8080/ws/{call_id}"
    headers = {"Authorization": "Bearer your-secret-key"}

    async with websockets.connect(uri, additional_headers=headers) as ws:
        async for message in ws:
            event = json.loads(message)

            if event["event"] == "connected":
                print("WebSocket connected")

            elif event["event"] == "start":
                print(f"Stream started: {event['start']['mediaFormat']}")

            elif event["event"] == "media":
                # Decode audio (mulaw, 8kHz, mono)
                audio_bytes = base64.b64decode(event["media"]["payload"])
                await process_audio(audio_bytes)

            elif event["event"] == "dtmf":
                digit = event["dtmf"]["digit"]
                print(f"DTMF: {digit}")

            elif event["event"] == "stop":
                print("Call ended")
                break
```

### Native Binary Mode

Connect with `?mode=native` for lower overhead — binary frames instead of JSON/base64. After the initial JSON `connected` and `start` text frames, audio is sent as binary frames:

```
[0x01][2 bytes: length big-endian][PCM16 LE audio bytes]
```

```python
async def handle_audio_native(call_id: str):
    uri = f"ws://localhost:8080/ws/{call_id}?mode=native"
    headers = {"Authorization": "Bearer your-secret-key"}

    async with websockets.connect(uri, additional_headers=headers) as ws:
        async for message in ws:
            if isinstance(message, bytes):
                # Binary frame: [0x01][2-byte length][PCM16 LE data]
                if len(message) > 3 and message[0] == 0x01:
                    length = int.from_bytes(message[1:3], "big")
                    pcm_audio = message[3:3 + length]
                    # pcm_audio is raw PCM16 LE, 8kHz, mono
                    await process_audio(pcm_audio)
            else:
                # Text frame: JSON events (connected, start, dtmf, stop)
                event = json.loads(message)
                if event["event"] == "stop":
                    break
                elif event["event"] == "dtmf":
                    print(f"DTMF: {event['dtmf']['digit']}")
```

Native mode is better when you're processing PCM audio directly (feeding to STT, writing to disk) since it avoids the base64 encode/decode overhead. The [Voice AI Demo](../demo/voice-ai/) uses native mode.

### Starting the Pipeline on call.answered

The WebSocket audio stream is available after the call is answered, not after `/incoming`. Start your audio pipeline when you receive the `call.answered` webhook event:

```python
@app.post("/incoming")
async def handle_incoming(call: dict):
    return {"action": "accept", "stream": True}

@app.post("/")
async def handle_events(event: dict):
    if event["event"] == "call.answered":
        call_id = event["call_id"]
        asyncio.create_task(handle_audio_stream(call_id))

    return {"ok": True}
```

---

## 5. Send Audio Back (TTS)

To play audio to the caller (e.g., TTS output), send media frames back through the same WebSocket.

### Twilio Mode

```python
async def send_audio(ws, call_id: str, audio_bytes: bytes):
    """Send mulaw audio back to the caller."""
    payload = base64.b64encode(audio_bytes).decode()
    await ws.send(json.dumps({
        "event": "media",
        "streamSid": call_id,
        "media": {"payload": payload}
    }))
```

### Native Mode

```python
async def send_audio_native(ws, pcm_bytes: bytes):
    """Send PCM16 audio back to the caller (native mode)."""
    frame = bytes([0x01]) + len(pcm_bytes).to_bytes(2, "big") + pcm_bytes
    await ws.send(frame)
```

### Full Duplex Example

A complete bidirectional audio handler (using native mode):

```python
async def voice_agent(call_id: str):
    uri = f"ws://localhost:8080/ws/{call_id}?mode=native"
    headers = {"Authorization": "Bearer your-secret-key"}

    async with websockets.connect(uri, additional_headers=headers) as ws:
        async for message in ws:
            if isinstance(message, bytes) and len(message) > 3 and message[0] == 0x01:
                length = int.from_bytes(message[1:3], "big")
                caller_audio = message[3:3 + length]

                # Your AI pipeline:
                # 1. Speech-to-text
                transcript = await stt(caller_audio)
                if not transcript:
                    continue

                # 2. LLM response
                response_text = await llm(transcript)

                # 3. Text-to-speech (must return PCM16 LE, 8kHz, mono)
                response_pcm = await tts(response_text)

                # 4. Send audio back to caller
                await send_audio_native(ws, response_pcm)

            elif isinstance(message, str):
                event = json.loads(message)
                if event["event"] == "stop":
                    break
```

### Using Marks for Interruption

Send a `mark` after your TTS audio to know when it finishes playing. This lets you detect barge-in (caller interrupting the agent):

```python
async def send_audio_with_mark(ws, call_id: str, audio_bytes: bytes, mark_name: str):
    # Send the audio
    await send_audio(ws, call_id, audio_bytes)

    # Send a mark — xbridge echoes it back when reached
    await ws.send(json.dumps({
        "event": "mark",
        "streamSid": call_id,
        "mark": {"name": mark_name}
    }))

# In your receive loop:
if event["event"] == "mark":
    print(f"Audio finished: {event['mark']['name']}")
```

---

## 6. Make Outbound Calls

Initiate calls via the REST API:

```python
import httpx

XBRIDGE = "http://localhost:8080"
HEADERS = {"Authorization": "Bearer your-secret-key"}

async def make_call(to: str, from_number: str) -> dict:
    async with httpx.AsyncClient() as client:
        resp = await client.post(
            f"{XBRIDGE}/v1/calls",
            json={"to": to, "from": from_number},
            headers=HEADERS,
        )
        resp.raise_for_status()
        data = resp.json()

        # data = {"call_id": "abc123", "status": "dialing", "ws_url": "ws://..."}
        print(f"Call initiated: {data['call_id']}")
        return data
```

### Via Specific Trunk or Peer

```python
# Via a named SIP trunk
resp = await client.post(
    f"{XBRIDGE}/v1/calls",
    json={"to": "+15551234567", "from": "+15559876543", "trunk": "telnyx"},
    headers=HEADERS,
)

# Via a PBX peer (ring extension 1001 on the office PBX)
resp = await client.post(
    f"{XBRIDGE}/v1/calls",
    json={"to": "1001", "from": "ai-agent", "peer": "office-pbx"},
    headers=HEADERS,
)
```

Connect to the WebSocket for audio once the `call.answered` event fires — same as inbound calls.

---

## 7. Call Control (Hold, Transfer, DTMF)

All call control is done via REST API calls.

```python
async def hold(call_id: str):
    async with httpx.AsyncClient() as client:
        await client.post(f"{XBRIDGE}/v1/calls/{call_id}/hold", headers=HEADERS)

async def resume(call_id: str):
    async with httpx.AsyncClient() as client:
        await client.post(f"{XBRIDGE}/v1/calls/{call_id}/resume", headers=HEADERS)

async def transfer(call_id: str, target: str):
    async with httpx.AsyncClient() as client:
        await client.post(
            f"{XBRIDGE}/v1/calls/{call_id}/transfer",
            json={"target": target},
            headers=HEADERS,
        )

async def send_dtmf(call_id: str, digits: str):
    async with httpx.AsyncClient() as client:
        await client.post(
            f"{XBRIDGE}/v1/calls/{call_id}/dtmf",
            json={"digits": digits},
            headers=HEADERS,
        )

async def mute(call_id: str):
    async with httpx.AsyncClient() as client:
        await client.post(f"{XBRIDGE}/v1/calls/{call_id}/mute", headers=HEADERS)

async def unmute(call_id: str):
    async with httpx.AsyncClient() as client:
        await client.post(f"{XBRIDGE}/v1/calls/{call_id}/unmute", headers=HEADERS)

async def hangup(call_id: str):
    async with httpx.AsyncClient() as client:
        await client.delete(f"{XBRIDGE}/v1/calls/{call_id}", headers=HEADERS)
```

### Handling DTMF in Your Agent

DTMF digits arrive both via WebSocket (real-time) and webhook (for logging). Use the WebSocket events for your agent logic:

```python
# In your WebSocket handler:
if event["event"] == "dtmf":
    digit = event["dtmf"]["digit"]

    if digit == "1":
        await send_audio(ws, call_id, sales_greeting)
    elif digit == "2":
        await send_audio(ws, call_id, support_greeting)
    elif digit == "0":
        await transfer(call_id, "sip:operator@pbx")
```

See the **[API Reference](api-reference.md)** for full request/response schemas.

---

## 8. Play Audio Prompts

For pre-recorded prompts or hold music, use the play endpoint instead of streaming through WebSocket:

```python
# Play a WAV file from URL (must be 8kHz mono 16-bit PCM)
async def play_prompt(call_id: str, url: str, loop_count: int = 1):
    async with httpx.AsyncClient() as client:
        resp = await client.post(
            f"{XBRIDGE}/v1/calls/{call_id}/play",
            json={"url": url, "loop_count": loop_count},
            headers=HEADERS,
        )
        return resp.json()  # {"play_id": "play_0"}

# Play hold music (loop forever)
await play_prompt(call_id, "https://example.com/hold-music.wav", loop_count=0)

# Stop playback
async def stop_play(call_id: str):
    async with httpx.AsyncClient() as client:
        await client.post(f"{XBRIDGE}/v1/calls/{call_id}/play/stop", headers=HEADERS)
```

### Inline Base64 Audio

For short prompts generated at runtime (e.g., TTS), send base64-encoded raw PCM directly:

```python
import base64

async def play_pcm(call_id: str, pcm_bytes: bytes, loop_count: int = 1):
    """Play raw PCM audio (8kHz mono 16-bit little-endian)."""
    async with httpx.AsyncClient() as client:
        resp = await client.post(
            f"{XBRIDGE}/v1/calls/{call_id}/play",
            json={
                "audio": base64.b64encode(pcm_bytes).decode(),
                "loop_count": loop_count,
            },
            headers=HEADERS,
        )
        return resp.json()
```

---

## 9. Monitor with Webhooks

xbridge sends webhook events for call lifecycle tracking. Use these for logging, analytics, or triggering actions.

```python
@app.post("/")
async def handle_webhook(event: dict):
    match event["event"]:
        case "call.ringing":
            print(f"Call {event['call_id']} ringing: {event['from']} → {event['to']}")

        case "call.answered":
            print(f"Call {event['call_id']} answered")
            # Start the audio pipeline here (see section 4)

        case "call.ended":
            print(f"Call {event['call_id']} ended: {event['reason']} ({event['duration']}s)")
            # Save CDR, update dashboard, etc.

        case "call.dtmf":
            print(f"Call {event['call_id']} DTMF: {event['digit']}")

        case "call.hold":
            print(f"Call {event['call_id']} placed on hold")

        case "call.resumed":
            print(f"Call {event['call_id']} resumed")

        case "call.play_finished":
            print(f"Play {event['play_id']} finished (interrupted: {event['interrupted']})")

    return {"ok": True}
```

### Dead Letter Queue

If your webhook endpoint is down, events are stored in an in-memory DLQ (max 1000). Check and drain them:

```python
# Check for missed events
resp = await client.get(f"{XBRIDGE}/v1/webhooks/failures", headers=HEADERS)
failures = resp.json()["failures"]

# Process and clear
if failures:
    for f in failures:
        print(f"Missed: {f['event']} (error: {f['error']}, attempts: {f['attempts']})")

    await client.delete(f"{XBRIDGE}/v1/webhooks/failures", headers=HEADERS)
```

---

## 10. Production Checklist

### Security

- [ ] Set `auth.api_key` in config — all API/WS requests require `Authorization: Bearer <key>`
- [ ] Use TLS for the HTTP/WS server (`cargo build --features tls`, configure `tls.cert` and `tls.key`)
- [ ] Use SRTP for SIP media encryption (`sip.srtp: true`)
- [ ] Use TLS transport for SIP signaling (`sip.transport: "tls"`)
- [ ] For trunk host peers over the internet, use digest auth (not just IP allowlist)

### Reliability

- [ ] Set `webhook.retry: 3` for webhook delivery resilience
- [ ] Monitor the DLQ endpoint (`GET /v1/webhooks/failures`) — alert if non-empty
- [ ] Use a STUN server if behind NAT (`sip.stun_server: "stun.l.google.com:19302"`)
- [ ] Monitor `/health` — status is `"starting"` until SIP registration or trunk host server is running

### Monitoring

- [ ] Scrape `/metrics` with Prometheus
- [ ] Key metrics to alert on:
  - `xbridge_active_calls` — capacity planning
  - `xbridge_webhooks_total{result="failure"}` — delivery failures
  - `xbridge_ws_connections` — should match active calls
  - `xbridge_call_duration_seconds` — call duration distribution
  - `xbridge_http_request_duration_seconds` — API latency

### Networking

- [ ] Open RTP port range (default: OS-assigned, or configure `rtp_port_min`/`rtp_port_max`) in firewall
- [ ] Open SIP port (5060 UDP/TCP or 5061 TLS) for cloud trunks
- [ ] Open trunk host port (e.g., 5080 UDP) for PBX peers
- [ ] Ensure WebSocket port (default: 8080) is accessible to your app

---

## Migrating from Twilio

xbridge is designed as a drop-in replacement for Twilio Media Streams. If you're migrating from Twilio:

### What stays the same

- **WebSocket protocol** — same JSON events (`connected`, `start`, `media`, `stop`), same `streamSid`/`media.payload` structure
- **Audio format** — `audio/x-mulaw` at 8kHz by default, same as Twilio
- **Client→server media** — same `{"event": "media", "streamSid": "...", "media": {"payload": "..."}}`

### What changes

| Twilio | xbridge |
|---|---|
| TwiML `<Stream>` to start streaming | Return `{"action": "accept", "stream": true}` from incoming webhook |
| `wss://media-stream.twilio.com/...` | `ws://your-xbridge:8080/ws/{call_id}` |
| REST API at `api.twilio.com` | REST API at `your-xbridge:8080/v1/...` |
| Account SID + Auth Token | Bearer token (`auth.api_key`) |
| Twilio manages SIP | You bring your own SIP trunk |
| Per-minute billing | Self-hosted, no per-minute cost |

### Migration Steps

1. Set up a SIP trunk with any provider (Telnyx, VoIP.ms, etc.)
2. Point your SIP trunk's inbound routing to xbridge's IP
3. Configure xbridge with your SIP credentials
4. Update your webhook URL in xbridge config to point to your app
5. Change your WebSocket connection URL from Twilio to xbridge
6. Replace Twilio REST API calls with xbridge equivalents
7. Remove TwiML — xbridge uses simple JSON webhooks instead

### New Features (Not in Twilio Media Streams)

- **DTMF over WebSocket** — get digits in real-time without polling
- **Mark echo** — send marks and get confirmation when they're reached
- **REST call control** — hold, resume, transfer, mute directly via API
- **Play audio** — server-side audio playback without streaming through WebSocket
- **Native binary mode** — lower overhead than JSON/base64 (connect with `?mode=native`)
- **Multi-trunk** — register with multiple SIP providers simultaneously
- **Trunk host mode** — accept calls directly from PBX systems without a cloud provider
- **Self-hosted** — full control, no vendor lock-in, no per-minute fees
