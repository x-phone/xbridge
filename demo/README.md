# xbridge Echo Bot Demo

A minimal demo that accepts incoming calls and echoes the caller's audio back, proving the full pipeline:

```
Caller → SIP → xbridge → WebSocket → echo-bot → WebSocket → xbridge → SIP → Caller
```

## Prerequisites

- A SIP trunk account (free tiers: [Telnyx](https://telnyx.com), [VoIP.ms](https://voip.ms), [Twilio](https://twilio.com))
- Docker and Docker Compose
- A SIP softphone for testing (e.g., [Linphone](https://linphone.org), [Zoiper](https://zoiper.com), or [MicroSIP](https://microsip.org))

## Quick Start

1. **Configure your SIP credentials:**

   ```bash
   cd demo
   # Edit config.yaml with your SIP provider credentials
   ```

2. **Start the stack:**

   ```bash
   docker compose up --build
   ```

   This starts:
   - **xbridge** on port 8080 (SIP gateway)
   - **echo-bot** on port 3000 (webhook handler + WebSocket echo client)

3. **Make a test call:**

   Call the number associated with your SIP trunk. You should hear your own voice echoed back.

4. **Watch the logs:**

   ```
   echo-bot  | 14:30:01  INFO   Incoming call abc123  +15551234567 → +15559876543
   echo-bot  | 14:30:01  INFO   Call abc123 answered — connecting WebSocket
   echo-bot  | 14:30:01  INFO   WebSocket connected for call abc123
   echo-bot  | 14:30:01  INFO     stream connected (protocol=Call)
   echo-bot  | 14:30:01  INFO     stream started  encoding=audio/x-mulaw  rate=8000
   echo-bot  | 14:30:15  INFO   Call abc123 ended  reason=normal  duration=14s
   ```

5. **Press `*` during a call** to hang up via the REST API (demonstrates call control).

## How It Works

The echo bot has two webhook endpoints:

- `POST /incoming` — xbridge sends this when a call arrives. The bot responds with `{"action": "accept", "stream": true}` to accept the call.
- `POST /events` — xbridge sends call lifecycle events here (`call.answered`, `call.ended`, `call.dtmf`, etc.).

When a call is answered, the bot connects to xbridge's WebSocket at `ws://xbridge:8080/ws/{call_id}` and echoes every audio frame back to the caller.

## Running Without Docker

```bash
# Terminal 1: start xbridge
cargo build --release
./target/release/xbridge --config demo/config.yaml

# Terminal 2: start the echo bot
cd demo
pip install -r requirements.txt
XBRIDGE_HOST=localhost:8080 python app.py
```

## Environment Variables

| Variable | Default | Description |
|---|---|---|
| `XBRIDGE_HOST` | `localhost:8080` | xbridge host:port |
| `XBRIDGE_API_KEY` | (empty) | API key for Bearer auth |
| `DEMO_PORT` | `3000` | Port the echo bot listens on |

## Next Steps

This demo echoes audio, but you can extend it to:

- **AI voice agent** — pipe audio to a speech-to-text service, process with an LLM, and send TTS audio back
- **IVR menu** — listen for DTMF digits and route calls accordingly
- **Call recording** — save the WebSocket audio stream to a file
- **Conference bridge** — mix audio from multiple calls

See the [Integration Guide](../docs/guide.md) for more examples.
