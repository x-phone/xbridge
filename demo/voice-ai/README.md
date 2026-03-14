# Voice AI Demo

A self-contained demo of the x-phone ecosystem: dial a number from a softphone,
talk to an AI voice agent (or navigate an IVR), and see real-time updates in a
React UI.

## Prerequisites

Clone the [xpbx](https://github.com/x-phone/xpbx) repo as a sibling directory:

```
x-phone/
├── xbridge/    ← this repo
└── xpbx/       ← git clone https://github.com/x-phone/xpbx.git
```

## Quick Start

```bash
cd demo/voice-ai
docker compose up --build
```

Open **http://localhost:3000** in your browser.

### Voice AI Mode (needs Deepgram API key)

1. Enter your [Deepgram API key](https://console.deepgram.com)
2. Click **Connect with Voice AI**
3. Speak into your softphone — see live transcription and hear AI responses

### IVR Demo (no API key)

1. Click **Try IVR Demo**
2. Navigate the phone menu with DTMF keypad — see menu state in the UI

### Make a Call

Register a SIP softphone (Zoiper, Ooma, Linphone, etc.):

| Setting    | Value                          |
|------------|--------------------------------|
| SIP Server | `localhost` (or your host IP)  |
| Username   | `1001`                         |
| Password   | `password123`                  |

Then dial **2000**.

## Architecture

```
Softphone ──SIP──▶ Asterisk (xpbx) ──SIP──▶ xbridge ──WebSocket──▶ voice-app
                                                                      │
                                                       ┌──────────────┤
                                                       ▼              ▼
                                                  Deepgram        React UI
                                                  STT/TTS      (transcription)
```

## Services

| Service    | Port | Description                 |
|------------|------|-----------------------------|
| asterisk   | 5060 | SIP server (Asterisk/xpbx)  |
| xpbx       | 8080 | PBX web UI                  |
| xbridge    | 8090 | SIP-to-WebSocket bridge     |
| voice-app  | 3000 | AI backend + React frontend |

## Network

The `EXTERNAL_IP` environment variable controls what IP softphones use to reach Asterisk for RTP media. It is auto-detected via STUN if not set.

Override for specific setups:

```bash
EXTERNAL_IP=100.96.49.117 docker compose up --build   # Tailscale/VPN
EXTERNAL_IP=192.168.1.50 docker compose up --build     # LAN-only
```

Or set in `.env`:

```
EXTERNAL_IP=100.96.49.117
```

## Development

Run the frontend with hot reload:

```bash
cd ../voice-app/frontend
npm install
npm run dev           # Vite dev server on :5173
```

Run the Python backend:

```bash
cd ../voice-app
pip install -r requirements.txt
XBRIDGE_HOST=localhost:8090 python app.py
```
