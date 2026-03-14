# xbridge API Reference

Complete reference for the xbridge REST API, WebSocket protocol, webhook events, and configuration.

## Table of Contents

- [Authentication](#authentication)
- [REST API](#rest-api)
  - [Health & Metrics](#health--metrics)
  - [Calls](#calls)
  - [Call Control](#call-control)
  - [Audio Playback](#audio-playback)
  - [Webhook Failures (DLQ)](#webhook-failures-dlq)
- [WebSocket Protocol](#websocket-protocol)
  - [Connection](#connection)
  - [Server Events](#server-events-server--client)
  - [Client Events](#client-events-client--server)
  - [Native Binary Mode](#native-binary-mode)
- [Webhooks](#webhooks)
  - [Incoming Call](#incoming-call)
  - [Lifecycle Events](#lifecycle-events)
  - [Delivery & Retries](#delivery--retries)
- [Configuration](#configuration)
  - [Core](#core)
  - [SIP Trunks](#sip-trunks)
  - [Trunk Host (Server)](#trunk-host-server)
  - [Environment Variables](#environment-variables)
- [Error Responses](#error-responses)

---

## Authentication

All `/v1/*` endpoints and WebSocket connections require authentication when `auth.api_key` is configured.

```
Authorization: Bearer <api_key>
```

Unauthenticated requests return `401 Unauthorized`. Health and metrics endpoints (`/health`, `/metrics`) are always public.

---

## REST API

Base URL: `http://<host>:<port>` (default port: 8080)

### Health & Metrics

#### `GET /health`

Returns server status. No authentication required.

**Response** `200 OK`
```json
{
  "status": "ok",
  "sip_trunks": 1,
  "active_calls": 3
}
```

| Field | Type | Description |
|---|---|---|
| `status` | string | `"ok"` when SIP is registered, `"starting"` during initialization |
| `sip_trunks` | integer | Number of connected SIP trunks |
| `active_calls` | integer | Number of calls currently in progress |

#### `GET /metrics`

Prometheus-format metrics. No authentication required.

**Response** `200 OK` (`text/plain; version=0.0.4; charset=utf-8`)
```
# HELP xbridge_calls_total Total calls processed
# TYPE xbridge_calls_total counter
xbridge_calls_total {direction="inbound"} 105
xbridge_calls_total {direction="outbound"} 42
# HELP xbridge_active_calls Currently active calls
# TYPE xbridge_active_calls gauge
xbridge_active_calls 3
# HELP xbridge_http_requests_total Total HTTP requests
# TYPE xbridge_http_requests_total counter
xbridge_http_requests_total 1520
# HELP xbridge_ws_connections Active WebSocket connections
# TYPE xbridge_ws_connections gauge
xbridge_ws_connections 3
# HELP xbridge_ws_frames_total WebSocket frames processed
# TYPE xbridge_ws_frames_total counter
xbridge_ws_frames_total {direction="sent"} 45230
xbridge_ws_frames_total {direction="received"} 44100
# HELP xbridge_webhooks_total Total webhook deliveries
# TYPE xbridge_webhooks_total counter
xbridge_webhooks_total {result="success"} 310
xbridge_webhooks_total {result="failure"} 2
# HELP xbridge_trunk_calls_total Total calls from trunk host peers
# TYPE xbridge_trunk_calls_total counter
xbridge_trunk_calls_total 58
# HELP xbridge_rate_limit_rejections_total HTTP requests rejected by rate limiter
# TYPE xbridge_rate_limit_rejections_total counter
xbridge_rate_limit_rejections_total 0
# HELP xbridge_call_duration_seconds Call duration
# TYPE xbridge_call_duration_seconds histogram
xbridge_call_duration_seconds_bucket{le="1"} 5
xbridge_call_duration_seconds_bucket{le="5"} 12
...
xbridge_call_duration_seconds_bucket{le="+Inf"} 147
xbridge_call_duration_seconds_sum 8532.5
xbridge_call_duration_seconds_count 147
# HELP xbridge_http_request_duration_seconds HTTP request duration
# TYPE xbridge_http_request_duration_seconds histogram
...
# HELP xbridge_webhook_duration_seconds Webhook delivery duration
# TYPE xbridge_webhook_duration_seconds histogram
...
```

---

### Calls

#### `POST /v1/calls`

Create an outbound call.

**Request Body**

| Field | Type | Required | Description |
|---|---|---|---|
| `to` | string | yes | Destination number or SIP address |
| `from` | string | yes | Caller ID |
| `trunk` | string | no | Trunk name (default: `"default"`) |
| `peer` | string | no | Trunk host peer name (mutually exclusive with `trunk`) |
| `webhook_url` | string | no | Override webhook URL for this call |
| `stream` | boolean | no | Enable WebSocket audio streaming |

```json
{
  "to": "+15551234567",
  "from": "+15559876543",
  "trunk": "telnyx"
}
```

**Response** `201 Created`

| Field | Type | Description |
|---|---|---|
| `call_id` | string | Unique call identifier |
| `status` | string | Always `"dialing"` |
| `ws_url` | string | WebSocket URL for audio streaming |

```json
{
  "call_id": "a1b2c3d4",
  "status": "dialing",
  "ws_url": "ws://localhost:8080/ws/a1b2c3d4"
}
```

**Errors**

| Status | Condition |
|---|---|
| `404` | Unknown trunk or peer name; no server config for peer calls |
| `422` | Peer has no `host` configured (can't determine outbound address) |
| `503` | No SIP trunk connected; trunk host server not running |

#### `GET /v1/calls`

List all active calls.

**Response** `200 OK`

```json
{
  "calls": [
    {
      "call_id": "a1b2c3d4",
      "from": "+15559876543",
      "to": "+15551234567",
      "direction": "outbound",
      "status": "in_progress"
    }
  ]
}
```

#### `GET /v1/calls/{call_id}`

Get details for a specific call.

**Response** `200 OK`

| Field | Type | Description |
|---|---|---|
| `call_id` | string | Unique call identifier |
| `from` | string | Caller ID |
| `to` | string | Called number/address |
| `direction` | string | `"inbound"` or `"outbound"` |
| `status` | string | See [Call Status](#call-status) |
| `peer` | string? | Trunk host peer name (omitted for cloud trunk calls) |

```json
{
  "call_id": "a1b2c3d4",
  "from": "1001",
  "to": "+15551234567",
  "direction": "inbound",
  "status": "in_progress",
  "peer": "office-pbx"
}
```

**Errors:** `404` if call not found.

#### `DELETE /v1/calls/{call_id}`

Hang up a call.

**Response:** `204 No Content`

**Errors:** `404` if call not found.

#### Call Status

| Value | Description |
|---|---|
| `dialing` | Outbound call initiated, waiting for response |
| `ringing` | Remote side is ringing |
| `in_progress` | Call is active with media flowing |
| `on_hold` | Call is on hold |
| `completed` | Call has ended |

---

### Call Control

All call control endpoints return `200 OK` on success, `404` if the call is not found, and `500` if the operation fails.

#### `POST /v1/calls/{call_id}/hold`

Place a call on hold (sends SIP re-INVITE with held SDP).

#### `POST /v1/calls/{call_id}/resume`

Resume a held call.

#### `POST /v1/calls/{call_id}/transfer`

Blind transfer the call to another destination (sends SIP REFER).

**Request Body**

| Field | Type | Required | Description |
|---|---|---|---|
| `target` | string | yes | SIP address or phone number |

```json
{"target": "sip:operator@pbx.local"}
```

#### `POST /v1/calls/{call_id}/dtmf`

Send DTMF tones to the remote party.

**Request Body**

| Field | Type | Required | Description |
|---|---|---|---|
| `digits` | string | yes | Digit sequence (0-9, *, #) |

```json
{"digits": "1234#"}
```

#### `POST /v1/calls/{call_id}/mute`

Mute the call (stop sending audio to remote party).

#### `POST /v1/calls/{call_id}/unmute`

Unmute the call.

---

### Audio Playback

#### `POST /v1/calls/{call_id}/play`

Play audio into a call. Provide either a URL to a WAV file or inline base64 audio.

**Request Body**

| Field | Type | Required | Description |
|---|---|---|---|
| `url` | string | no* | HTTP(S) URL to a WAV file (8kHz, mono, 16-bit PCM) |
| `audio` | string | no* | Base64-encoded raw PCM16 audio (8kHz, mono, 16-bit LE) |
| `loop_count` | integer | no | Number of times to play. `0` = infinite loop. Default: `1` |

*One of `url` or `audio` is required.

**Response** `200 OK`

```json
{"play_id": "play_0"}
```

**Errors**

| Status | Condition |
|---|---|
| `400` | No audio source; invalid base64; WAV format error (wrong sample rate, channels, bit depth) |
| `404` | Call not found |
| `500` | Audio writer not available; URL fetch failed |

#### `POST /v1/calls/{call_id}/play/stop`

Stop the current playback on a call.

**Response:** `200 OK`

**Errors:** `404` if no active playback on the call.

---

### Webhook Failures (DLQ)

#### `GET /v1/webhooks/failures`

List webhook delivery failures stored in the dead letter queue.

**Response** `200 OK`

```json
{
  "failures": [
    {
      "event": {"event": "call.ended", "call_id": "abc", "reason": "normal", "duration": 45},
      "error": "Connection refused",
      "attempts": 3,
      "timestamp": "2026-03-12T10:30:00Z"
    }
  ]
}
```

#### `DELETE /v1/webhooks/failures`

Drain (clear) the dead letter queue.

**Response** `200 OK`

```json
{"drained": 5}
```

---

## WebSocket Protocol

### Connection

```
GET /ws/{call_id}[?mode=native]
Authorization: Bearer <api_key>
Upgrade: websocket
```

Connect after accepting an incoming call or creating an outbound call. The `call_id` comes from the webhook payload or the create-call response.

**Query Parameters**

| Parameter | Type | Default | Description |
|---|---|---|---|
| `mode` | string | `"twilio"` | Stream mode: `"twilio"` (JSON/base64) or `"native"` (binary frames) |

Returns `404` if the call doesn't exist.

---

### Server Events (Server → Client)

#### `connected`

Sent immediately after WebSocket upgrade.

```json
{
  "event": "connected",
  "protocol": "Call",
  "version": "1.0.0"
}
```

#### `start`

Sent when audio streaming begins. Contains the media format.

```json
{
  "event": "start",
  "streamSid": "a1b2c3d4",
  "start": {
    "callSid": "a1b2c3d4",
    "tracks": ["inbound"],
    "mediaFormat": {
      "encoding": "audio/x-mulaw",
      "sampleRate": 8000,
      "channels": 1
    }
  }
}
```

#### `media`

Audio frame from the caller. Sent continuously while the call is active.

```json
{
  "event": "media",
  "streamSid": "a1b2c3d4",
  "media": {
    "timestamp": "0",
    "payload": "<base64-encoded audio>"
  }
}
```

The `payload` encoding depends on config:
- `audio/x-mulaw` — 8-bit mu-law, 8kHz. 160 bytes per 20ms frame.
- `audio/x-l16` — 16-bit linear PCM, little-endian. 320 bytes per 20ms frame at 8kHz.

#### `dtmf`

DTMF digit detected from the caller.

```json
{
  "event": "dtmf",
  "streamSid": "a1b2c3d4",
  "dtmf": {
    "digit": "5"
  }
}
```

#### `mark`

Echo of a client-sent mark. Delivered when the mark's position in the audio buffer is reached (i.e., all audio sent before the mark has been played).

```json
{
  "event": "mark",
  "streamSid": "a1b2c3d4",
  "mark": {
    "name": "greeting-end"
  }
}
```

#### `stop`

Call has ended. The WebSocket will close after this event.

```json
{
  "event": "stop",
  "streamSid": "a1b2c3d4"
}
```

---

### Client Events (Client → Server)

#### `media`

Send audio to the caller (e.g., TTS output).

```json
{
  "event": "media",
  "streamSid": "a1b2c3d4",
  "media": {
    "payload": "<base64-encoded audio>"
  }
}
```

Audio format must match the `encoding` in the `start` event.

#### `mark`

Insert a marker in the audio buffer. The server echoes it back as a `mark` event when reached.

```json
{
  "event": "mark",
  "streamSid": "a1b2c3d4",
  "mark": {
    "name": "utterance-42"
  }
}
```

#### `clear`

Clear the server-side audio buffer. Use for barge-in (stop playing queued TTS when the caller interrupts).

```json
{
  "event": "clear",
  "streamSid": "a1b2c3d4"
}
```

---

### Native Binary Mode

When the `mode=native` query parameter is set on the WebSocket connection URL, audio frames are sent as binary WebSocket frames instead of JSON, reducing overhead.

**Binary frame format:**

```
[0x01] [length: 2 bytes, big-endian] [PCM16 LE audio: N bytes]
```

- Tag byte `0x01` identifies an audio frame
- Length is the audio payload size in bytes (not including the 3-byte header)
- Audio is raw PCM16, little-endian, mono

Control messages (`mark`, `clear`) are still sent as JSON text frames in native mode.

---

## Webhooks

xbridge sends HTTP POST requests to your webhook URL for call lifecycle events.

### Incoming Call

**Endpoint:** `POST {webhook_url}/incoming`

Sent when a new inbound call arrives. Your app must respond synchronously to accept or reject the call.

**Payload**

| Field | Type | Description |
|---|---|---|
| `call_id` | string | Unique call identifier |
| `from` | string | Caller ID |
| `to` | string | Called number/address |
| `direction` | string | Always `"inbound"` |
| `peer` | string? | Trunk host peer name (omitted for cloud trunk calls) |

```json
{
  "call_id": "a1b2c3d4",
  "from": "1001",
  "to": "+15551234567",
  "direction": "inbound",
  "peer": "office-pbx"
}
```

**Expected Response**

| Field | Type | Required | Description |
|---|---|---|---|
| `action` | string | yes | `"accept"` or `"reject"` |
| `stream` | boolean | no | Enable WebSocket audio streaming |
| `reason` | string | no | Rejection reason (e.g., `"busy"`, `"declined"`) |

```json
{"action": "accept", "stream": true}
```

```json
{"action": "reject", "reason": "busy"}
```

---

### Lifecycle Events

**Endpoint:** `POST {webhook_url}/`

All lifecycle events are POSTed to the webhook base URL.

#### `call.ringing`

Remote side is ringing (outbound calls).

```json
{
  "event": "call.ringing",
  "call_id": "a1b2c3d4",
  "from": "+15559876543",
  "to": "+15551234567"
}
```

#### `call.answered`

Call is connected and media is flowing.

```json
{
  "event": "call.answered",
  "call_id": "a1b2c3d4"
}
```

#### `call.ended`

Call has terminated.

| Field | Type | Description |
|---|---|---|
| `event` | string | `"call.ended"` |
| `call_id` | string | Call identifier |
| `reason` | string | End reason (e.g., `"normal"`, `"busy"`, `"no_answer"`, `"rejected"`, `"error"`) |
| `duration` | number | Call duration in seconds |

```json
{
  "event": "call.ended",
  "call_id": "a1b2c3d4",
  "reason": "normal",
  "duration": 127
}
```

#### `call.dtmf`

DTMF digit received.

```json
{
  "event": "call.dtmf",
  "call_id": "a1b2c3d4",
  "digit": "5"
}
```

#### `call.hold`

Call placed on hold.

```json
{
  "event": "call.hold",
  "call_id": "a1b2c3d4"
}
```

#### `call.resumed`

Call resumed from hold.

```json
{
  "event": "call.resumed",
  "call_id": "a1b2c3d4"
}
```

#### `call.play_finished`

Audio playback completed or was interrupted.

| Field | Type | Description |
|---|---|---|
| `event` | string | `"call.play_finished"` |
| `call_id` | string | Call identifier |
| `play_id` | string | Playback session identifier |
| `interrupted` | boolean | `true` if stopped before completion |

```json
{
  "event": "call.play_finished",
  "call_id": "a1b2c3d4",
  "play_id": "play_0",
  "interrupted": false
}
```

---

### Delivery & Retries

| Setting | Default | Description |
|---|---|---|
| `webhook.timeout` | `"5s"` | HTTP timeout per attempt |
| `webhook.retry` | `1` | Number of retries after first failure |

Retry uses exponential backoff with jitter:
- Base delay: 100ms
- Formula: `100ms * 2^(attempt-1) + random(0..50ms)`
- Example with `retry: 2`: attempt 1 → fail → ~100ms → attempt 2 → fail → ~200ms → attempt 3

Events that exhaust all retries are stored in the dead letter queue (max 1000 entries, oldest evicted when full).

---

## Configuration

xbridge loads configuration from a YAML or TOML file, with environment variable overrides.

```bash
xbridge --config config.yaml
# or
xbridge --config config.toml
```

### Core

```yaml
listen:
  http: "0.0.0.0:8080"       # Required. HTTP/WS listen address.

webhook:
  url: "http://your-app:3000" # Required. Base webhook URL.
  timeout: "5s"               # HTTP timeout per webhook attempt. Default: "5s"
  retry: 1                    # Retry count after first failure. Default: 1

stream:
  encoding: "audio/x-mulaw"   # "audio/x-mulaw" or "audio/x-l16". Default: "audio/x-mulaw"
  sample_rate: 8000           # Audio sample rate in Hz. Default: 8000

auth:
  api_key: "your-secret-key"  # Bearer token for API/WS auth. Optional (no auth if omitted).

rate_limit:
  requests_per_second: 100    # Rate limit for authenticated endpoints. Optional (no limit if omitted).

tls:
  cert: "/path/to/cert.pem"   # TLS certificate. Optional.
  key: "/path/to/key.pem"     # TLS private key. Optional.
```

### SIP Trunks

Single trunk (legacy format):

```yaml
sip:
  username: "user"
  password: "pass"
  host: "sip.provider.com"
  transport: "udp"            # "udp", "tcp", or "tls". Default: "udp"
  rtp_port_min: 0             # Minimum RTP port. 0 = OS-assigned. Default: 0
  rtp_port_max: 0             # Maximum RTP port. 0 = OS-assigned. Default: 0
  srtp: false                 # Enable SRTP media encryption. Default: false
  stun_server: ""             # STUN server for NAT traversal. Optional.
```

Multiple trunks:

```yaml
trunks:
  - name: "telnyx"
    username: "user1"
    password: "pass1"
    host: "sip.telnyx.com"
    transport: "udp"

  - name: "voipms"
    username: "user2"
    password: "pass2"
    host: "sip.voip.ms"
    transport: "udp"
```

When `trunks` is set, the `sip` block is ignored. When only `sip` is set, it creates a single trunk named `"default"`.

### Trunk Host (Server)

Accept SIP calls directly from PBX systems.

```yaml
server:
  listen: "0.0.0.0:5080"     # Required. SIP UDP listen address.
  rtp_port_min: 0             # Minimum RTP port. 0 = OS-assigned. Default: 0
  rtp_port_max: 0             # Maximum RTP port. 0 = OS-assigned. Default: 0
  peers:
    # IP-based authentication
    - name: "office-pbx"
      host: "192.168.1.10"    # Accept INVITEs from this IP without challenge.
      port: 5060              # SIP port for outbound calls to this peer. Default: 5060
      codecs: ["ulaw", "alaw"] # Allowed codecs. Empty = accept any. Default: []

    # Digest authentication
    - name: "remote-office"
      auth:
        username: "remote-trunk"
        password: "s3cret"
      port: 5060
      codecs: ["ulaw"]
```

**Peer authentication order:**
1. Check source IP against peer `host` fields (fastest path)
2. If no IP match, check `Authorization` header against peer digest credentials
3. If no `Authorization` header but digest-auth peers exist, respond with `401` challenge
4. Otherwise, reject with `403`

A peer can have both `host` and `auth` — IP match takes priority.

### Environment Variables

Every config field can be overridden via environment variables:

| Variable | Config Path |
|---|---|
| `XBRIDGE_LISTEN_HTTP` | `listen.http` |
| `XBRIDGE_SIP_USERNAME` | `sip.username` |
| `XBRIDGE_SIP_PASSWORD` | `sip.password` |
| `XBRIDGE_SIP_HOST` | `sip.host` |
| `XBRIDGE_SIP_TRANSPORT` | `sip.transport` |
| `XBRIDGE_SIP_RTP_PORT_MIN` | `sip.rtp_port_min` |
| `XBRIDGE_SIP_RTP_PORT_MAX` | `sip.rtp_port_max` |
| `XBRIDGE_SIP_SRTP` | `sip.srtp` |
| `XBRIDGE_SIP_STUN_SERVER` | `sip.stun_server` |
| `XBRIDGE_WEBHOOK_URL` | `webhook.url` |
| `XBRIDGE_WEBHOOK_TIMEOUT` | `webhook.timeout` |
| `XBRIDGE_WEBHOOK_RETRY` | `webhook.retry` |
| `XBRIDGE_STREAM_ENCODING` | `stream.encoding` |
| `XBRIDGE_STREAM_SAMPLE_RATE` | `stream.sample_rate` |
| `XBRIDGE_AUTH_API_KEY` | `auth.api_key` |
| `XBRIDGE_RATE_LIMIT_RPS` | `rate_limit.requests_per_second` |
| `XBRIDGE_TLS_CERT` | `tls.cert` |
| `XBRIDGE_TLS_KEY` | `tls.key` |

Environment variables take precedence over the config file.

---

## Error Responses

All error responses return a JSON body with a `message` field:

```json
{"message": "Call not found"}
```

| Status | Meaning |
|---|---|
| `400 Bad Request` | Invalid JSON, missing required fields, invalid audio format |
| `401 Unauthorized` | Missing or invalid `Authorization` header |
| `404 Not Found` | Call, trunk, or peer not found |
| `422 Unprocessable Entity` | Valid request but can't be fulfilled (e.g., peer has no host for outbound) |
| `429 Too Many Requests` | Rate limit exceeded |
| `500 Internal Server Error` | Call operation failed, audio fetch error |
| `503 Service Unavailable` | No SIP trunk connected, trunk host server not running |
