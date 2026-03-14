# xbridge Integration Guide

Build AI voice agents with xbridge — a self-hosted, Twilio-compatible voice gateway.

This guide walks you through building a complete voice agent from scratch. All examples use Python with FastAPI, but xbridge works with any language that supports HTTP webhooks and WebSocket.

## Table of Contents

- [Architecture Overview](#architecture-overview)
- [1. Start xbridge](#1-start-xbridge)
- [2. Handle Incoming Calls](#2-handle-incoming-calls)
- [3. Stream Audio via WebSocket](#3-stream-audio-via-websocket)
- [4. Send Audio Back (TTS)](#4-send-audio-back-tts)
- [5. Make Outbound Calls](#5-make-outbound-calls)
- [6. Call Control (Hold, Transfer, DTMF)](#6-call-control-hold-transfer-dtmf)
- [7. Play Audio Prompts](#7-play-audio-prompts)
- [8. Monitor with Webhooks](#8-monitor-with-webhooks)
- [9. Trunk Host Mode (PBX Integration)](#9-trunk-host-mode-pbx-integration)
- [10. Production Checklist](#10-production-checklist)
- [Migrating from Twilio](#migrating-from-twilio)

---

## Architecture Overview

xbridge connects your app to the phone network via two paths: cloud SIP trunks and direct PBX peering.

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
 │ Asterisk │  :5080│  │ (server)   │  │ (xphone)     │  │        │ Telnyx   │
 │ xpbx     │──SIP─▶  │            │  │              │  ◀─SIP──┤ VoIP.ms  │
 └──────────┘      │  └────────────┘  └──────────────┘  │        └──────────┘
                    └───────────────────────────────────┘
```

Your app does two things:

1. **Webhook handler** — receives HTTP POSTs from xbridge when calls arrive, end, etc.
2. **WebSocket client** — connects to xbridge to send/receive real-time audio.

Both paths (cloud trunk and PBX peer) deliver the same experience to your app — same webhooks, same WebSocket protocol, same REST API.

---

## 1. Start xbridge

Create `config.yaml`:

```yaml
listen:
  http: "0.0.0.0:8080"

sip:
  username: "your_sip_username"
  password: "your_sip_password"
  host: "sip.telnyx.com"
  transport: "udp"

webhook:
  url: "http://your-app:3000"
  timeout: "5s"
  retry: 2

stream:
  encoding: "audio/x-mulaw"
  sample_rate: 8000

auth:
  api_key: "your-secret-key"
```

Start with Docker:

```bash
docker compose up -d
```

Or from source:

```bash
RUST_LOG=info cargo run --release -- --config config.yaml
```

Verify it's running:

```bash
curl http://localhost:8080/health
# {"status":"ok","sip_trunks":1,"active_calls":0}
```

---

## 2. Handle Incoming Calls

When someone calls your SIP number, xbridge POSTs to `{webhook_url}/incoming`. Your app decides whether to accept or reject.

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

Once you return `{"action": "accept"}`, xbridge answers the SIP call and makes audio available via WebSocket at `ws://xbridge:8080/ws/{call_id}`.

---

## 3. Stream Audio via WebSocket

Connect to the WebSocket to receive caller audio in real-time. The protocol is Twilio-compatible — if you've used Twilio Media Streams, this is the same format.

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
                # Send to your AI model (STT, LLM, etc.)
                await process_audio(audio_bytes)

            elif event["event"] == "dtmf":
                digit = event["dtmf"]["digit"]
                print(f"DTMF: {digit}")

            elif event["event"] == "stop":
                print("Call ended")
                break
```

### Connecting After Accept

Update your webhook handler to start the WebSocket stream after accepting:

```python
@app.post("/incoming")
async def handle_incoming(call: dict):
    call_id = call["call_id"]

    # Accept the call
    response = {"action": "accept", "stream": True}

    # Start audio processing in background
    asyncio.create_task(handle_audio_stream(call_id))

    return response
```

---

## 4. Send Audio Back (TTS)

To play audio to the caller (e.g., TTS output), send media frames back through the same WebSocket:

```python
async def send_audio(ws, call_id: str, audio_bytes: bytes):
    """Send mulaw audio back to the caller."""
    payload = base64.b64encode(audio_bytes).decode()
    await ws.send(json.dumps({
        "event": "media",
        "streamSid": call_id,
        "media": {
            "payload": payload
        }
    }))
```

### Full Duplex Example

A complete bidirectional audio handler:

```python
async def voice_agent(call_id: str):
    uri = f"ws://localhost:8080/ws/{call_id}"
    headers = {"Authorization": "Bearer your-secret-key"}

    async with websockets.connect(uri, additional_headers=headers) as ws:
        async for message in ws:
            event = json.loads(message)

            if event["event"] == "media":
                caller_audio = base64.b64decode(event["media"]["payload"])

                # Your AI pipeline:
                # 1. Speech-to-text
                transcript = await stt(caller_audio)
                if not transcript:
                    continue

                # 2. LLM response
                response_text = await llm(transcript)

                # 3. Text-to-speech
                response_audio = await tts(response_text)

                # 4. Send audio back to caller
                await send_audio(ws, call_id, response_audio)

            elif event["event"] == "stop":
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

## 5. Make Outbound Calls

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

        # Connect to WebSocket for audio
        asyncio.create_task(voice_agent(data["call_id"]))

        return data
```

### With Multi-Trunk

If you have multiple SIP providers, specify which trunk to use:

```python
resp = await client.post(
    f"{XBRIDGE}/v1/calls",
    json={
        "to": "+15551234567",
        "from": "+15559876543",
        "trunk": "telnyx"  # Use the "telnyx" trunk
    },
    headers=HEADERS,
)
```

---

## 6. Call Control (Hold, Transfer, DTMF)

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

---

## 7. Play Audio Prompts

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

## 8. Monitor with Webhooks

xbridge sends webhook events for call lifecycle tracking. Use these for logging, analytics, or triggering actions:

```python
@app.post("/")
async def handle_webhook(event: dict):
    match event["event"]:
        case "call.ringing":
            print(f"Call {event['call_id']} ringing: {event['from']} → {event['to']}")

        case "call.answered":
            print(f"Call {event['call_id']} answered")

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

## 9. Trunk Host Mode (PBX Integration)

Trunk host mode lets xbridge accept SIP calls directly from PBX systems (Asterisk, FreePBX, xpbx) without a cloud trunk provider. Your PBX points a SIP trunk at xbridge, and calls flow into the same webhook + WebSocket pipeline.

### Configuration

Add a `server` section to your `config.yaml`:

```yaml
# Existing config (webhook, stream, auth, etc.) stays the same

server:
  listen: "0.0.0.0:5080"
  peers:
    # IP-based auth — accept any INVITE from this IP
    - name: "office-pbx"
      host: "192.168.1.10"
      codecs: ["ulaw", "alaw"]

    # Digest auth — challenge with 401, verify credentials
    - name: "remote-office"
      auth:
        username: "remote-trunk"
        password: "s3cret"
      codecs: ["ulaw"]
```

You can use both `sip` (cloud trunk) and `server` (trunk host) simultaneously — xbridge will register with cloud providers while also accepting direct SIP from peers.

### PBX Setup

On your PBX, create a SIP trunk pointing at xbridge:

| PBX Setting | Value |
|---|---|
| Trunk type | SIP (UDP) |
| Server/Host | `xbridge-ip:5080` |
| Authentication | IP-based (no credentials needed if peer uses `host`) or username/password |
| Codec | ulaw or alaw |

### Inbound Calls from PBX

Calls from peers arrive through the same `/incoming` webhook. The `peer` field identifies which PBX the call came from:

```python
@app.post("/incoming")
async def handle_incoming(call: dict):
    peer = call.get("peer")  # "office-pbx", "remote-office", or None for cloud trunk

    if peer == "office-pbx":
        print(f"Call from office PBX: {call['from']} → {call['to']}")
    elif peer:
        print(f"Call from peer {peer}: {call['from']} → {call['to']}")
    else:
        print(f"Call from cloud trunk: {call['from']} → {call['to']}")

    return {"action": "accept", "stream": True}
```

Everything else is identical — same WebSocket audio, same REST call control, same webhook events.

### Outbound Calls to PBX Extensions

Originate calls to PBX extensions using the `peer` field instead of `trunk`:

```python
# Ring extension 1001 on the office PBX
resp = await client.post(
    f"{XBRIDGE}/v1/calls",
    json={
        "to": "1001",
        "from": "ai-agent",
        "peer": "office-pbx"
    },
    headers=HEADERS,
)
data = resp.json()
# {"call_id": "trunk-abc123", "status": "dialing", "ws_url": "ws://..."}

# Then connect WebSocket for audio, same as any other call
asyncio.create_task(voice_agent(data["call_id"]))
```

### Example: AI Receptionist for Office PBX

A complete example — PBX forwards incoming calls to xbridge, where an AI agent handles them:

```yaml
# config.yaml
listen:
  http: "0.0.0.0:8080"

webhook:
  url: "http://ai-agent:3000"
  timeout: "10s"
  retry: 2

auth:
  api_key: "secret"

server:
  listen: "0.0.0.0:5080"
  peers:
    - name: "office"
      host: "192.168.1.10"
```

```python
# ai_receptionist.py
from fastapi import FastAPI
import asyncio, json, base64, websockets

app = FastAPI()
XBRIDGE = "http://localhost:8080"
HEADERS = {"Authorization": "Bearer secret"}

@app.post("/incoming")
async def incoming(call: dict):
    asyncio.create_task(handle_call(call["call_id"], call["from"]))
    return {"action": "accept", "stream": True}

async def handle_call(call_id: str, caller: str):
    uri = f"ws://localhost:8080/ws/{call_id}"
    headers = {"Authorization": "Bearer secret"}

    async with websockets.connect(uri, additional_headers=headers) as ws:
        async for message in ws:
            event = json.loads(message)

            if event["event"] == "media":
                audio = base64.b64decode(event["media"]["payload"])
                # Feed to STT → LLM → TTS pipeline
                response = await ai_pipeline(audio)
                if response:
                    payload = base64.b64encode(response).decode()
                    await ws.send(json.dumps({
                        "event": "media",
                        "streamSid": call_id,
                        "media": {"payload": payload}
                    }))

            elif event["event"] == "stop":
                break
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
- [ ] Use a STUN server if behind NAT (`sip.stun_server: "stun:stun.l.google.com:19302"`)
- [ ] Monitor `/health` — status is `"starting"` until SIP registration succeeds

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
