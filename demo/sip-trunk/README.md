# SIP Trunk Demo

Connect xbridge directly to a SIP trunk provider (Twilio, Telnyx, etc.) to handle
PSTN calls with the voice-app — no PBX needed.

```
Phone call ──PSTN──▶ Twilio/Telnyx ──SIP──▶ xbridge ──WebSocket──▶ voice-app
```

## Quick Start

1. **Pick a provider** and copy the example config:

   ```bash
   cd demo/sip-trunk

   # For Twilio:
   cp twilio.yaml xbridge.yaml

   # For Telnyx:
   cp telnyx.yaml xbridge.yaml
   ```

2. **Edit `xbridge.yaml`** — replace `YOUR_SERVER_IP` with your server's public IP.

3. **Configure your provider** (see setup instructions in the YAML file).

4. **Start the demo:**

   ```bash
   docker compose up --build
   ```

5. Open **http://localhost:3000** and configure the voice app (Deepgram API key or IVR mode).

6. Call your PSTN phone number — the call arrives at xbridge and connects to the voice app.

## Firewall

Open these ports on your server:

| Port | Protocol | Purpose |
|------|----------|---------|
| 5080 | UDP      | SIP signaling from trunk provider |
| 10200-10300 | UDP | RTP media |
| 8090 | TCP      | xbridge HTTP API (optional, for local access) |
| 3000 | TCP      | Voice app UI (optional, for local access) |

## Provider Configs

- **[twilio.yaml](twilio.yaml)** — Twilio SIP Trunk with North America, Europe, Asia, and South America signaling IPs
- **[telnyx.yaml](telnyx.yaml)** — Telnyx SIP Connection with US signaling IPs

These configs use IP-based authentication — the trunk provider's signaling IPs are allowlisted as peers.
