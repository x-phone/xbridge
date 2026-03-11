# xbridge

Self-hosted voice gateway — WebSocket audio streaming and REST call control. Drop-in Twilio replacement powered by [xphone](https://github.com/x-phone/xphone).

xbridge connects SIP trunks to your application via WebSocket (audio) + REST (call control), so you can build AI voice agents, IVRs, or call centers without vendor lock-in.

## Features

- **SIP trunk registration** with any provider (Telnyx, Twilio, VoIP.ms, etc.)
- **WebSocket audio streaming** — Twilio-compatible JSON/base64 or native binary mode
- **REST call control** — create, hangup, hold, resume, transfer, DTMF, mute/unmute
- **Incoming call webhooks** — accept/reject decisions via your app
- **Multi-trunk support** — register with multiple SIP providers simultaneously
- **Bearer token auth** and **rate limiting**
- **Prometheus metrics** at `/metrics`
- **TLS support** via rustls
- **Graceful shutdown** on SIGTERM/SIGINT

## Quick Start

### With Docker

```bash
cp config.example.yaml config.yaml
# Edit config.yaml with your SIP credentials and webhook URL

docker compose up
```

### From Source

```bash
cargo build --release
./target/release/xbridge --config config.yaml
```

Set `RUST_LOG=info` (or `debug`, `trace`) for logging output.

## Configuration

xbridge loads config from a YAML or TOML file, with environment variable overrides.

```bash
xbridge --config config.yaml
```

See [`config.example.yaml`](config.example.yaml) for all options.

### Environment Variable Overrides

Every config field can be overridden via `XBRIDGE_*` environment variables:

| Variable | Description |
|---|---|
| `XBRIDGE_LISTEN_HTTP` | HTTP listen address (default: `0.0.0.0:8080`) |
| `XBRIDGE_SIP_USERNAME` | SIP username |
| `XBRIDGE_SIP_PASSWORD` | SIP password |
| `XBRIDGE_SIP_HOST` | SIP registrar host |
| `XBRIDGE_SIP_TRANSPORT` | `udp`, `tcp`, or `tls` |
| `XBRIDGE_SIP_SRTP` | `true` or `false` |
| `XBRIDGE_SIP_STUN_SERVER` | STUN server address |
| `XBRIDGE_SIP_RTP_PORT_MIN` | RTP port range start (default: `10000`) |
| `XBRIDGE_SIP_RTP_PORT_MAX` | RTP port range end (default: `20000`) |
| `XBRIDGE_WEBHOOK_URL` | Webhook endpoint URL |
| `XBRIDGE_WEBHOOK_TIMEOUT` | Webhook timeout (e.g. `5s`, `500ms`) |
| `XBRIDGE_WEBHOOK_RETRY` | Webhook retry count |
| `XBRIDGE_STREAM_MODE` | `twilio` or `native` |
| `XBRIDGE_STREAM_ENCODING` | `audio/x-mulaw` or `audio/x-l16` |
| `XBRIDGE_STREAM_SAMPLE_RATE` | Sample rate in Hz |
| `XBRIDGE_AUTH_API_KEY` | API key for Bearer token auth |
| `XBRIDGE_RATE_LIMIT_RPS` | Requests per second limit |
| `XBRIDGE_TLS_CERT` | Path to TLS certificate PEM |
| `XBRIDGE_TLS_KEY` | Path to TLS private key PEM |

### Multi-Trunk

To register with multiple SIP providers, use the `trunks` array instead of the `sip` block:

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

Specify a trunk for outbound calls via the `trunk` field in the create call request. If omitted, the `"default"` trunk is used.

## REST API

All API endpoints require `Authorization: Bearer <api_key>` when `auth.api_key` is configured.

### Create Call (Outbound)

```
POST /v1/calls
```

```json
{
  "to": "+15551234567",
  "from": "+15559876543",
  "trunk": "telnyx"
}
```

Response:

```json
{
  "call_id": "abc123",
  "status": "dialing",
  "ws_url": "ws://host:8080/ws/abc123"
}
```

### List Calls

```
GET /v1/calls
```

### Get Call

```
GET /v1/calls/{call_id}
```

### Hangup

```
DELETE /v1/calls/{call_id}
```

### Hold / Resume

```
POST /v1/calls/{call_id}/hold
POST /v1/calls/{call_id}/resume
```

### Transfer

```
POST /v1/calls/{call_id}/transfer
```

```json
{ "target": "sip:1003@pbx" }
```

### Send DTMF

```
POST /v1/calls/{call_id}/dtmf
```

```json
{ "digits": "1234" }
```

### Mute / Unmute

```
POST /v1/calls/{call_id}/mute
POST /v1/calls/{call_id}/unmute
```

### Play Audio

```
POST /v1/calls/{call_id}/play
```

```json
{
  "url": "https://example.com/greeting.wav",
  "loop_count": 1
}
```

Or with inline base64 PCM (8kHz mono 16-bit LE):

```json
{
  "audio": "<base64-encoded PCM>",
  "loop_count": 0
}
```

`loop_count: 0` loops forever (useful for hold music). WAV files must be 8kHz mono 16-bit PCM.

Response:

```json
{"play_id": "play_0"}
```

Stop playback:

```
POST /v1/calls/{call_id}/play/stop
```

A `call.play_finished` webhook is fired when playback completes or is interrupted.

### Webhook Failures (Dead Letter Queue)

Events that fail delivery after all retries are stored in an in-memory dead letter queue (max 1000 entries).

```
GET /v1/webhooks/failures
```

```json
{
  "failures": [
    {
      "event": {"event": "call.ended", "call_id": "abc123", "reason": "normal", "duration": 45},
      "error": "HTTP 502",
      "attempts": 3,
      "timestamp": "2025-03-10T14:30:00Z"
    }
  ]
}
```

Drain (acknowledge and clear):

```
DELETE /v1/webhooks/failures
```

```json
{"drained": 1}
```

## WebSocket Audio Streaming

Connect to `ws://host:8080/ws/{call_id}` to stream audio for a call.

### Twilio-Compatible Mode (`stream.mode: twilio`)

Server sends JSON text frames:

```json
{"event": "connected", "protocol": "Call", "version": "1.0.0"}
{"event": "start", "streamSid": "call_id", "start": {"callSid": "call_id", "tracks": ["inbound"], "mediaFormat": {"encoding": "audio/x-mulaw", "sampleRate": 8000, "channels": 1}}}
{"event": "media", "streamSid": "call_id", "media": {"timestamp": "0", "payload": "<base64>"}}
{"event": "dtmf", "streamSid": "call_id", "dtmf": {"digit": "5"}}
{"event": "mark", "streamSid": "call_id", "mark": {"name": "utterance_end"}}
{"event": "stop", "streamSid": "call_id"}
```

DTMF digits are delivered both as WebSocket events (for real-time agent processing) and as webhooks (for control plane logging). Mark events are echoed back when the client sends a mark.

Client sends:

```json
{"event": "media", "streamSid": "call_id", "media": {"payload": "<base64>"}}
```

### Native Mode (`stream.mode: native`)

After the initial JSON `connected` and `start` text frames, audio is sent as binary frames:

```
[0x01][2 bytes: length big-endian][PCM16 LE audio bytes]
```

This reduces overhead by avoiding JSON encoding and base64 for every audio frame.

## Webhooks

xbridge sends webhook events to your configured URL:

### Incoming Call

When a call arrives, xbridge POSTs to `{webhook_url}/incoming`:

```json
{
  "call_id": "abc123",
  "from": "+15551234567",
  "to": "+15559876543",
  "direction": "inbound"
}
```

Your app responds with:

```json
{"action": "accept", "stream": true}
```

or:

```json
{"action": "reject", "reason": "busy"}
```

### Call Events

Events are POSTed to `{webhook_url}`:

| Event | Fields |
|---|---|
| `call.ringing` | `call_id`, `from`, `to` |
| `call.answered` | `call_id` |
| `call.ended` | `call_id`, `reason`, `duration` |
| `call.dtmf` | `call_id`, `digit` |
| `call.hold` | `call_id` |
| `call.resumed` | `call_id` |
| `call.play_finished` | `call_id`, `play_id`, `interrupted` |

### Retry and Dead Letter Queue

Event delivery uses exponential backoff (100ms base, doubling each retry). The number of retries is configured via `webhook.retry` (default: 1).

Events that fail all delivery attempts are stored in an in-memory dead letter queue (max 1000 entries, oldest evicted). Inspect and drain via `GET/DELETE /v1/webhooks/failures`.

## Monitoring

### Health Check

```
GET /health
```

```json
{"status": "ok", "sip_trunks": 1, "active_calls": 0}
```

### Prometheus Metrics

```
GET /metrics
```

Exposed metrics:
- `xbridge_calls_total{direction}` — total calls (counter)
- `xbridge_active_calls` — currently active calls (gauge)
- `xbridge_ws_connections` — active WebSocket connections (gauge)
- `xbridge_http_requests_total` — total HTTP requests (counter)
- `xbridge_webhooks_total{result}` — webhook deliveries (counter)

## TLS

Build with TLS support and configure cert/key:

```bash
cargo build --release --features tls
```

```yaml
tls:
  cert: "/path/to/cert.pem"
  key: "/path/to/key.pem"
```

## Architecture

xbridge is designed as a **data plane** — it handles real-time SIP signaling, RTP media, and WebSocket audio streaming. It is intentionally stateless: no database, no persistent storage, no recording engine.

For production deployments that need call recordings, CDR storage, billing, or dashboards, the intended architecture is a separate **control plane** that consumes xbridge's webhook events and WebSocket audio:

```
┌─────────────┐    webhooks     ┌─────────────────┐
│             │ ──────────────> │                 │
│   xbridge   │                 │  Control Plane  │──> DB, S3, dashboards
│ (data plane)│ <────────────── │                 │
│             │   REST calls    │                 │
└──────┬──────┘                 └────────┬────────┘
       │                                 │
   SIP/RTP                          WebSocket
       │                           (audio tap)
       v                                 │
┌──────────────┐                         v
│  SIP Trunk   │                 ┌──────────────┐
│  (Telnyx,    │                 │  Recording / │
│   Twilio)    │                 │  Transcription│
└──────────────┘                 └──────────────┘
```

**How it works:**

1. xbridge fires webhook events (`call.ringing`, `call.answered`, `call.ended`, etc.) to the control plane — this is the existing `webhook.url` config.
2. The control plane persists call events, manages state, and exposes dashboards.
3. For recordings, the control plane connects to the `ws_url` returned by xbridge (from the create call response or incoming call webhook), receives the audio stream, and writes it to storage (disk, S3, etc.).
4. The control plane drives call actions (hangup, transfer, DTMF) via xbridge's REST API.

This separation keeps xbridge fast and simple — it never touches disk for call data — while letting you build whatever persistence and business logic you need on top.

## Integration Guide

See the **[Integration Guide](docs/guide.md)** for a step-by-step walkthrough of building an AI voice agent with xbridge, including Python code examples, Twilio migration instructions, and a production checklist.

## Development

```bash
cargo test          # Run all tests
cargo clippy        # Lint
cargo fmt           # Format
```

## License

MIT
