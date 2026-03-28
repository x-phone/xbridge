# xbridge

[![CI](https://github.com/x-phone/xbridge/actions/workflows/ci.yml/badge.svg)](https://github.com/x-phone/xbridge/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Docker](https://img.shields.io/badge/ghcr.io-x--phone%2Fxbridge-blue?logo=docker)](https://ghcr.io/x-phone/xbridge)

A self-hosted voice gateway that connects SIP phone calls to WebSocket audio and a REST API. One binary, a YAML config, and your app gets real-time call audio over WebSocket and full call control over REST.

Powered by [xphone-rust](https://github.com/x-phone/xphone-rust).

## Table of Contents

- [Status](#status--beta) | [Scope and Limitations](#scope-and-limitations) | [Use Cases](#use-cases)
- [Quick Start](#quick-start) | [Demo](#demo) | [Connection Modes](#connection-modes)
- [Configuration](#configuration) | [REST API](#rest-api) | [WebSocket Audio](#websocket-audio) | [Webhooks](#webhooks)
- [Architecture](#architecture) | [Monitoring](#monitoring) | [Features](#features)
- [Development](#development) | [License](#license)

---

## Status — Beta

xbridge is in active development and used in production alongside xphone-rust. Single-node only — no built-in clustering or HA. Run multiple instances behind a load balancer for redundancy.

---

## Scope and limitations

xbridge is a **voice data plane** — real-time SIP signaling, RTP media, and WebSocket audio streaming. It is intentionally stateless: no database, no persistent storage, no recording engine.

**What xbridge is not:**

- **Not a full Twilio replacement.** xbridge provides Twilio-compatible WebSocket audio framing, but it does not include number provisioning, billing, CDR storage, call recording, or dashboards. Those are your application's responsibility.
- **Audio only** — no video support. xbridge handles voice calls exclusively.
- **Blind transfer only** — attended transfer is not available via the REST API.
- **Narrowband audio** — WebSocket audio is 8 kHz (mu-law or PCM16). Wideband codecs (G.722, Opus) are negotiated on the SIP side but downsampled for the WebSocket stream.
- **Single-node** — no built-in clustering or HA.
- **SRTP uses SDES key exchange only** — DTLS-SRTP is not supported.

For production deployments that need recordings, CDR storage, billing, or dashboards, build a separate **control plane** that consumes xbridge's webhooks and WebSocket audio.

---

## Use cases

- **AI voice agents** — caller audio arrives over WebSocket, your app runs STT + LLM + TTS, sends audio back
- **IVR systems** — DTMF detection, audio playback, and call routing driven by your backend via REST + webhooks
- **Call centers** — route incoming calls via webhooks, hold/transfer/mute via REST, tap audio for transcription
- **Call recording** — connect to the WebSocket audio stream, write frames to disk or S3
- **Outbound dialers** — originate calls via REST API, play audio, detect DTMF responses

---

## Quick Start

### With Docker

```bash
docker run -v ./config.yaml:/etc/xbridge/config.yaml \
  -p 8080:8080 -p 10000-10100:10000-10100/udp \
  ghcr.io/x-phone/xbridge:latest
```

### Pre-built Binaries

Download from [GitHub Releases](https://github.com/x-phone/xbridge/releases) for Linux and macOS (amd64/arm64):

```bash
curl -L https://github.com/x-phone/xbridge/releases/latest/download/xbridge-linux-amd64 -o xbridge
chmod +x xbridge
./xbridge --config config.yaml
```

### From Source

```bash
cargo build --release
./target/release/xbridge --config config.yaml
```

Set `RUST_LOG=info` (or `debug`, `trace`) for logging output.

---

## Demo

A full-stack Voice AI demo is included: softphone → PBX → xbridge → AI agent with live transcription.

```
Softphone (ext 1001)
    → Asterisk/xpbx (dial 2000)
        → xbridge (SIP trunk host, WebSocket audio)
            → voice-app (Deepgram STT/TTS, React UI)
```

```bash
git clone https://github.com/x-phone/xpbx.git   # sibling directory
cd xbridge/demo/voice-ai
echo "EXTERNAL_IP=$(ipconfig getifaddr en0)" > .env  # macOS
docker compose up --build
```

Open http://localhost:3000, enter a Deepgram API key, register a softphone as extension 1001 (password: `password123`) on your machine's IP port 5060, and dial 2000 to talk to the AI agent.

A simpler **[SIP Trunk Demo](demo/sip-trunk/)** connects xbridge directly to Twilio or Telnyx for PSTN calls, no PBX needed.

---

## Connection Modes

xbridge supports three ways to connect to the phone network. All three deliver the same interface to your app — same webhooks, same WebSocket audio, same REST API. Your app speaks HTTP and WebSocket; xbridge handles all the SIP/RTP complexity.

1. **SIP extension** — register on any PBX (Asterisk, FreePBX, 3CX) as an extension, just like a softphone would
2. **SIP trunk client** — register with a cloud provider (Telnyx, Twilio, VoIP.ms) to get a real phone number
3. **SIP trunk host** — run a SIP server and accept calls directly from PBX systems or trunk providers

All three modes can run simultaneously in the same configuration.

---

## Configuration

xbridge loads config from a YAML or TOML file. See [`config.example.yaml`](config.example.yaml) for all options.

```bash
xbridge --config config.yaml
```

### SIP Extension (register on a PBX)

Register as an extension on your office PBX — the simplest mode. Calls to your extension trigger webhooks and audio streams to your app:

```yaml
sip:
  username: "2000"
  password: "secret"
  host: "192.168.1.10"       # your PBX address
  rtp_port_min: 10000
  rtp_port_max: 10100
```

### SIP Trunk Client (cloud provider)

Register with a SIP trunk provider to get a real phone number:

```yaml
sip:
  username: "your-username"
  password: "your-password"
  host: "sip.telnyx.com"
  transport: "tls"
  srtp: true
```

For multiple providers, use the `trunks` array:

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

Run a SIP server that PBX systems connect to directly:

```yaml
server:
  listen: "0.0.0.0:5080"
  peers:
    - name: "office-pbx"
      host: "192.168.1.10"
      codecs: ["ulaw", "alaw"]
    - name: "remote-office"
      auth:
        username: "remote-trunk"
        password: "secret"
```

Peers authenticate via IP allowlist (`host`), SIP digest credentials (`auth`), or both.

### Webhook and Stream

```yaml
webhook:
  url: "http://localhost:3000/events"
  timeout: "5s"
  retry: 1

stream:
  encoding: "audio/x-mulaw"   # audio/x-mulaw | audio/x-l16
  sample_rate: 8000
```

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
| `XBRIDGE_SIP_RTP_PORT_MIN` | RTP port range start |
| `XBRIDGE_SIP_RTP_PORT_MAX` | RTP port range end |
| `XBRIDGE_WEBHOOK_URL` | Webhook endpoint URL |
| `XBRIDGE_WEBHOOK_TIMEOUT` | Webhook timeout (e.g. `5s`) |
| `XBRIDGE_WEBHOOK_RETRY` | Webhook retry count |
| `XBRIDGE_STREAM_ENCODING` | `audio/x-mulaw` or `audio/x-l16` |
| `XBRIDGE_STREAM_SAMPLE_RATE` | Sample rate in Hz |
| `XBRIDGE_AUTH_API_KEY` | API key for Bearer token auth |
| `XBRIDGE_RATE_LIMIT_RPS` | Requests per second limit |
| `XBRIDGE_TLS_CERT` | Path to TLS certificate PEM |
| `XBRIDGE_TLS_KEY` | Path to TLS private key PEM |

---

## REST API

All endpoints require `Authorization: Bearer <api_key>` when `auth.api_key` is configured.

| Method | Endpoint | Description |
|---|---|---|
| `POST` | `/v1/calls` | Create outbound call (via trunk, peer, or extension) |
| `GET` | `/v1/calls` | List active calls |
| `GET` | `/v1/calls/{id}` | Get call details |
| `DELETE` | `/v1/calls/{id}` | Hang up call |
| `POST` | `/v1/calls/{id}/hold` | Hold call |
| `POST` | `/v1/calls/{id}/resume` | Resume held call |
| `POST` | `/v1/calls/{id}/transfer` | Blind transfer |
| `POST` | `/v1/calls/{id}/dtmf` | Send DTMF digits |
| `POST` | `/v1/calls/{id}/mute` | Mute outbound audio |
| `POST` | `/v1/calls/{id}/unmute` | Unmute |
| `POST` | `/v1/calls/{id}/play` | Play audio (URL or inline PCM) |
| `POST` | `/v1/calls/{id}/play/stop` | Stop playback |
| `GET` | `/v1/webhooks/failures` | List failed webhook deliveries |
| `DELETE` | `/v1/webhooks/failures` | Drain failed webhook queue |
| `GET` | `/health` | Health check |
| `GET` | `/metrics` | Prometheus metrics |

See the **[API Reference](docs/api-reference.md)** for request/response schemas and examples.

---

## WebSocket Audio

Connect to `ws://host:8080/ws/{call_id}` to stream audio for a call.

**Twilio-compatible mode** (default) — JSON text frames with base64-encoded audio. Compatible with apps built for Twilio Media Streams.

**Native binary mode** (`?mode=native`) — binary frames with raw PCM16 audio. Lower overhead, no JSON/base64 encoding per frame.

Both modes deliver the same lifecycle events (`connected`, `start`, `media`, `dtmf`, `mark`, `stop`). See the **[API Reference](docs/api-reference.md#websocket-audio-streaming)** for protocol details.

---

## Webhooks

xbridge fires webhook events to your configured URL for call lifecycle management.

**Incoming calls** are POSTed to `{webhook_url}/incoming`. Your app responds with `accept` or `reject`.

**Lifecycle events** are POSTed to `{webhook_url}`:

| Event | Description |
|---|---|
| `call.ringing` | Outbound call is ringing |
| `call.answered` | Call was answered |
| `call.ended` | Call ended (includes reason and duration) |
| `call.dtmf` | DTMF digit received |
| `call.hold` | Call placed on hold |
| `call.resumed` | Call resumed from hold |
| `call.play_finished` | Audio playback completed or interrupted |

Failed deliveries are retried with exponential backoff and stored in an in-memory dead letter queue. See the **[API Reference](docs/api-reference.md#webhooks)** for payload schemas.

---

## Architecture

xbridge is a **data plane** — it handles real-time SIP signaling, RTP media, and WebSocket audio streaming. It is intentionally stateless: no database, no persistent storage, no recording engine.

```
                                  webhooks     ┌─────────────────┐
                              ──────────────> │                 │
┌──────────────┐              ┌───────────┐   │   Your App      │──> DB, S3, dashboards
│  SIP Trunk   │──SIP/RTP──> │           │ <──│                 │
│  (Telnyx,    │   client    │           │    └────────┬────────┘
│   Twilio)    │              │  xbridge  │             │
└──────────────┘              │           │        WebSocket
                              │           │       (audio stream)
┌──────────────┐              │           │             │
│  PBX         │──SIP/RTP──> │           │             v
│  (Asterisk,  │   :5080     │           │    ┌──────────────┐
│   FreePBX)   │              └───────────┘    │  Recording / │
└──────────────┘                               │  Transcription│
                                               └──────────────┘
```

---

## Monitoring

### Health Check

```
GET /health → {"status": "ok", "sip_trunks": 1, "sip_server": false, "active_calls": 0}
```

### Prometheus Metrics

```
GET /metrics
```

| Metric | Type | Description |
|---|---|---|
| `xbridge_calls_total` | counter | Total calls by direction |
| `xbridge_active_calls` | gauge | Currently active calls |
| `xbridge_call_duration_seconds` | histogram | Call duration |
| `xbridge_http_requests_total` | counter | HTTP API requests |
| `xbridge_http_request_duration_seconds` | histogram | HTTP request latency |
| `xbridge_ws_connections` | gauge | Active WebSocket connections |
| `xbridge_ws_frames_total` | counter | WebSocket frames sent/received |
| `xbridge_webhooks_total` | counter | Webhook deliveries by result |
| `xbridge_webhook_duration_seconds` | histogram | Webhook delivery latency |
| `xbridge_trunk_calls_total` | counter | Calls from trunk host peers |
| `xbridge_rate_limit_rejections_total` | counter | Rate-limited requests |

---

## Features

### Connection modes — stable

- SIP extension (register on PBX)
- SIP trunk client (Telnyx, Twilio, VoIP.ms, etc.)
- SIP trunk host (accept calls from PBX systems)
- Multi-trunk (multiple SIP providers simultaneously)

### Call control — stable

- Inbound and outbound calls
- Hold / resume
- Blind transfer
- DTMF send and receive
- Mute / unmute
- Audio playback (URL or inline PCM)

### Audio streaming — stable

- Twilio-compatible WebSocket mode (JSON/base64)
- Native binary WebSocket mode (PCM16)
- Configurable encoding (mu-law, linear16)

### Security — stable

- Bearer token API authentication
- IP allowlist and SIP digest peer authentication
- Rate limiting
- TLS (rustls)
- SRTP (via SIP trunk)

### Observability — stable

- Prometheus metrics
- Health check endpoint
- Webhook dead letter queue

### Ops — stable

- Single binary, zero runtime dependencies
- YAML / TOML / env var configuration
- Docker image (amd64 + arm64)
- Graceful shutdown (SIGTERM/SIGINT)

---

## Integration Guide

See the **[Integration Guide](docs/guide.md)** for a step-by-step walkthrough of building an AI voice agent with xbridge, including Python code examples, Twilio migration instructions, and a production checklist.

---

## Development

```bash
cargo test          # Run all tests
cargo clippy        # Lint
cargo fmt           # Format
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for project layout and architecture details.

## License

MIT
