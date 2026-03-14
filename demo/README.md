# xbridge Demos

Two demo setups showing different ways to use xbridge:

## [Voice AI Demo](voice-ai/) — Full-stack with PBX

Self-contained demo with softphone, PBX (xpbx), xbridge, and an AI voice agent.
Dial a number, talk to AI, see live transcription in the browser.

```bash
cd demo/voice-ai
docker compose up --build
```

Requires [xpbx](https://github.com/x-phone/xpbx) cloned as a sibling directory.

## [SIP Trunk Demo](sip-trunk/) — Direct PSTN calls

Connect xbridge directly to Twilio or Telnyx — no PBX needed.
Receive PSTN calls and handle them with the voice app.

```bash
cd demo/sip-trunk
cp twilio.yaml xbridge.yaml   # or telnyx.yaml
# Edit xbridge.yaml with your public IP
docker compose up --build
```

## Shared Voice App

Both demos use the same [voice-app](voice-app/) backend (Python/FastAPI) and
React frontend. It supports two modes:

- **Voice AI** — Deepgram STT/TTS conversational agent (needs API key)
- **IVR Demo** — DTMF phone menu with tone feedback (no API key needed)

Open **http://localhost:3000** after starting either demo.
