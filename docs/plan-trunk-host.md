# Implementation Plan: SIP Trunk Host Mode

**Status:** Draft
**Date:** 2026-03-12

## Goal

Add a SIP server listener to xbridge so it can accept incoming calls directly from PBX systems (xpbx, Asterisk, FreePBX) — not just from cloud trunk providers.

Every call that arrives on this listener enters the **existing xbridge flow**: webhook notification, REST control, WebSocket audio streaming. No routing engine, no call bridging — xbridge stays as the programmable API gateway, not a soft-switch.

```
# Both paths deliver the same experience to your app:

Telnyx ──INVITE──▶ xbridge ──webhook──▶ your app ──WS audio──▶ AI
xpbx   ──INVITE──▶ xbridge ──webhook──▶ your app ──WS audio──▶ AI
```

## Current State

xbridge is a **SIP client only**:
- Registers WITH external trunk providers using `xphone::Phone`
- Receives inbound calls from those trunks
- Makes outbound calls through those trunks
- Full REST API, WebSocket streaming, and webhook system already working

## Key Design Decision: No xphone Changes

The SIP server listener is **xbridge's concern**, not xphone's. xphone is a SIP client/extension library — registration, outbound calls, handling calls on a trunk. The server listener is a different problem.

However, xphone's `Call` type is fully reusable without `Phone`:

- `Call::new_inbound(dlg: Arc<dyn Dialog>)` is **public** and takes only a `Dialog` trait object
- `Call` has **no back-reference** to `Phone` — it's a standalone call state machine
- The `Dialog` trait is **public** and implementable by external crates (11 methods: `respond()`, `send_bye()`, `send_cancel()`, etc.)
- SDP negotiation, codec handling, RTP media pipeline — all inside `Call`
- RTP socket is provided externally via `call.set_rtp_socket()`

This means xbridge builds its own SIP transport layer and implements the `Dialog` trait to bridge it to xphone's `Call`. All the hard stuff (media, codecs, SRTP, DTMF, jitter buffer) comes for free.

```
xbridge SIP listener (new code)        xphone (unchanged)
┌─────────────────────────────┐        ┌──────────────────┐
│ UDP socket on :5080         │        │                  │
│ SIP message parser          │        │                  │
│ Peer auth (IP + digest)     │        │                  │
│                             │        │                  │
│ TrunkDialog (impl Dialog) ──┼───────▶│ Call::new_inbound │
│                             │        │ Media pipeline    │
│                             │        │ Codecs, SRTP      │
│                             │        │ DTMF, jitter buf  │
└─────────────────────────────┘        └──────────────────┘
```

## What's Needed

### 1. SIP Server Listener

A new module (or sub-crate) in xbridge that listens on a SIP port and handles the SIP protocol server-side.

**SIP messages to handle:**
- **INVITE** — new incoming call → auth, create Call, fire webhook
- **ACK** — confirms call setup (after 200 OK)
- **BYE** — remote side hangs up → end Call
- **CANCEL** — remote side cancels before answer
- **OPTIONS** — health check / keepalive → respond 200

**What it produces:** An authenticated, parsed INVITE with SDP, ready to be wrapped in a `Dialog` impl and handed to `Call::new_inbound()`.

### 2. TrunkDialog Implementation

xbridge implements `xphone::Dialog` to map SIP server-side operations back to the UDP transport:

| Dialog method | SIP action |
|---|---|
| `respond(200, "OK", sdp)` | Send SIP 200 OK with SDP answer |
| `respond(180, "Ringing", _)` | Send SIP 180 Ringing |
| `send_bye()` | Send SIP BYE to caller |
| `send_cancel()` | Send SIP CANCEL (not typical for UAS but needed by trait) |
| `send_reinvite(sdp)` | Send re-INVITE for hold/resume |
| `send_refer(target)` | Send SIP REFER for transfer |
| `send_info_dtmf(digit, ms)` | Send SIP INFO with DTMF payload |

This is the glue layer — translating xphone's call control into SIP messages on xbridge's own transport.

### 3. Peer Authentication

Two auth methods, usable independently or together:

**IP allowlist** (default for LAN deployments):
- Peer defines a `host` — only INVITEs from that IP are accepted
- Simple, zero-config on the PBX side

**SIP digest auth** (for peers connecting over the internet):
- Peer defines `username` + `password`
- xbridge challenges unknown INVITEs with 401 + nonce
- Standard SIP authentication flow

Unauthenticated INVITEs are rejected with 403.

### 4. Peer Configuration

```yaml
# New config section
server:
  listen: "0.0.0.0:5080"
  peers:
    - name: "office-pbx"
      host: "192.168.1.10"          # IP-based auth
      codecs: ["ulaw", "alaw"]

    - name: "remote-office"
      auth:                          # Digest auth
        username: "remote-trunk"
        password: "secret"
      codecs: ["ulaw"]

# Existing config unchanged
trunks:
  - name: "telnyx"
    username: "..."
    password: "..."
    host: "sip.telnyx.com"
```

### 5. Call Flow

Incoming calls from peers follow the **exact same path** as trunk inbound calls:

1. SIP INVITE arrives on `:5080`
2. Auth check (IP or digest) — reject 403 if unknown
3. Parse SDP from INVITE body
4. Create `TrunkDialog` wrapping the SIP transport + remote address
5. `Call::new_inbound(trunk_dialog)` — reuse xphone's Call
6. `call.set_rtp_socket(socket)` — allocate RTP port
7. `call.set_remote_sdp(sdp)` — provide the offer
8. Create `CallInfo` with `direction: inbound` and `peer: "office-pbx"`
9. Fire `call.incoming` webhook (existing)
10. App responds: `accept` + stream, `reject`, etc. (existing)
11. If accepted, Call handles SDP answer + media pipeline automatically

The only addition to the webhook payload is a `peer` field:

```json
{
  "event": "call.incoming",
  "call_id": "abc123",
  "from": "1001",
  "to": "+15551234567",
  "direction": "inbound",
  "peer": "office-pbx"
}
```

## Implementation Steps

### Step 1: SIP Message Parser
1. Minimal SIP message parser — parse request line, headers, body
2. Only needs to handle INVITE, ACK, BYE, CANCEL, OPTIONS
3. SIP response builder for sending replies (100 Trying, 180 Ringing, 200 OK, 401, 403)
4. Can reference xphone's `sip/message.rs` for patterns, but keep it independent

### Step 2: SIP Server Listener
1. Bind UDP socket on configured `server.listen` address
2. Receive loop: parse incoming SIP messages, dispatch by method
3. OPTIONS → respond 200 immediately
4. INVITE → hand off to auth + call creation pipeline
5. BYE/CANCEL → look up active call, end it
6. ACK → complete call setup

### Step 3: Peer Auth
1. Parse `server.peers` config section
2. On INVITE: check source IP against peer `host` fields
3. If no IP match, check for digest auth credentials
4. If digest auth configured: send 401 challenge, validate response
5. Reject with 403 if no match
6. Map authenticated peer name onto call metadata

### Step 4: TrunkDialog + Call Creation
1. Implement `xphone::Dialog` trait as `TrunkDialog`
2. `TrunkDialog` holds: UDP socket, remote addr, SIP call-id, CSeq, branch, tags
3. On authenticated INVITE: create `TrunkDialog`, then `Call::new_inbound(dialog)`
4. Allocate RTP port, set remote SDP
5. Feed into existing `handle_incoming()` — same path as trunk calls
6. Add `peer` field to `CallInfo` and webhook payload

### Step 5: Outbound to Peers
1. Extend `POST /v1/calls` to accept a `peer` + `destination` instead of just `trunk` + `to`
2. Create `TrunkDialog` targeting the peer's known address
3. `Call::new_outbound(dialog, opts)` — send INVITE to peer
4. Call enters the normal active-call flow (REST control, WS streaming, webhooks)

### Step 6: Test
1. Configure xpbx/Asterisk with a SIP trunk pointing at xbridge:5080
2. Place call from PBX extension → verify webhook fires
3. Accept call via API → verify WebSocket audio streams
4. Originate call from API to PBX extension → verify it rings
5. Verify unauthenticated calls are rejected

## Architecture

```
                     ┌──────────────────────────────────────┐
                     │              xbridge                  │
                     │                                       │
 ┌──────────┐       │  ┌──────────────┐   ┌───────────┐    │      ┌──────────┐
 │  xpbx    │──SIP──▶  │ SIP Server   │   │ REST API  │◀───┼─────▶│ Your App │
 │ Asterisk │ :5080 │  │ (new module) │   │ WebSocket │    │      │ Webhooks │
 └──────────┘       │  │              │   │ :8080     │    │      └──────────┘
                     │  │ TrunkDialog  │   └───────────┘    │
                     │  └──────┬───────┘                     │
                     │         │                             │
                     │         │ xphone::Call::new_inbound() │
                     │         ▼                             │
                     │  ┌─────────────────┐                  │
                     │  │ handle_incoming()│                  │
                     │  │ (shared path)    │                  │
                     │  └────┬────────────┘                  │
                     │       ▲                               │
 ┌──────────┐       │  ┌────┴─────┐                          │
 │ Telnyx   │──SIP──▶  │ SIP      │                          │
 │ Twilio   │ client│  │ Client   │                          │
 └──────────┘       │  │ (xphone  │                          │
                     │  │  Phone)  │                          │
                     │  └──────────┘                          │
                     └──────────────────────────────────────┘
```

## Decisions

- **No xphone changes.** The SIP server listener is xbridge's concern. xphone's `Call` and `Dialog` trait are reused as-is.
- **Auth**: Ship both IP allowlist and SIP digest auth from the start.
- **Outbound to peers**: Yes — the app can originate calls to peer extensions via `POST /v1/calls` with a peer target.
