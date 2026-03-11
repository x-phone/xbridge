"""
xbridge echo bot demo.

Accepts all incoming calls, connects to the xbridge WebSocket,
and echoes the caller's audio back — proving the full SIP→RTP→WS→app→WS→RTP→SIP pipeline.

DTMF digits are logged to the console. Press '*' to hang up the call.

Usage:
    pip install -r requirements.txt
    python app.py
"""

import asyncio
import json
import logging
import os

import httpx
import uvicorn
import websockets
from fastapi import FastAPI, Request
from fastapi.responses import JSONResponse

logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s  %(levelname)-5s  %(message)s",
    datefmt="%H:%M:%S",
)
log = logging.getLogger("echo-bot")

XBRIDGE_HOST = os.getenv("XBRIDGE_HOST", "localhost:8080")
XBRIDGE_API_KEY = os.getenv("XBRIDGE_API_KEY", "")
LISTEN_PORT = int(os.getenv("DEMO_PORT", "3000"))

app = FastAPI()

# Track active WebSocket tasks so we can cancel on shutdown
ws_tasks: dict[str, asyncio.Task] = {}

# Shared HTTP client for REST API calls
http_client = httpx.AsyncClient()


def _auth_headers() -> dict[str, str]:
    if XBRIDGE_API_KEY:
        return {"Authorization": f"Bearer {XBRIDGE_API_KEY}"}
    return {}


# ── Webhook Endpoints ───────────────────────────────────────────────


@app.post("/incoming")
async def incoming_call(request: Request):
    """Handle incoming call webhook from xbridge."""
    body = await request.json()
    call_id = body.get("call_id", "unknown")
    caller = body.get("from", "unknown")
    callee = body.get("to", "unknown")

    log.info("Incoming call %s  %s → %s", call_id, caller, callee)

    # Accept the call and start audio streaming
    return JSONResponse({"action": "accept", "stream": True})


@app.post("/events")
async def call_events(request: Request):
    """Handle call event webhooks from xbridge."""
    body = await request.json()
    event = body.get("event", "unknown")
    call_id = body.get("call_id", "?")

    if event == "call.answered":
        log.info("Call %s answered — connecting WebSocket", call_id)
        # Cancel any existing task for this call_id before starting a new one
        if call_id in ws_tasks:
            ws_tasks.pop(call_id).cancel()
        task = asyncio.create_task(echo_audio(call_id))
        ws_tasks[call_id] = task
    elif event == "call.ended":
        reason = body.get("reason", "unknown")
        duration = body.get("duration", 0)
        log.info("Call %s ended  reason=%s  duration=%ss", call_id, reason, duration)
        # Cancel WS task if still running
        task = ws_tasks.pop(call_id, None)
        if task:
            task.cancel()
    elif event == "call.dtmf":
        log.info("Call %s  DTMF: %s", call_id, body.get("digit", "?"))
    elif event == "call.play_finished":
        log.info(
            "Call %s  playback %s finished (interrupted=%s)",
            call_id,
            body.get("play_id", "?"),
            body.get("interrupted", False),
        )
    else:
        log.info("Call %s  event: %s", call_id, event)

    return JSONResponse({"ok": True})


# ── WebSocket Echo ──────────────────────────────────────────────────


async def echo_audio(call_id: str):
    """Connect to xbridge WebSocket and echo audio back to the caller."""
    ws_url = f"ws://{XBRIDGE_HOST}/ws/{call_id}"

    try:
        async with websockets.connect(
            ws_url, additional_headers=_auth_headers()
        ) as ws:
            log.info("WebSocket connected for call %s", call_id)

            async for raw in ws:
                msg = json.loads(raw)
                event = msg.get("event")

                if event == "connected":
                    log.info("  stream connected (protocol=%s)", msg.get("protocol"))

                elif event == "start":
                    fmt = msg.get("start", {}).get("mediaFormat", {})
                    log.info(
                        "  stream started  encoding=%s  rate=%s",
                        fmt.get("encoding"),
                        fmt.get("sampleRate"),
                    )

                elif event == "media":
                    # Echo the audio back to the caller
                    echo = {
                        "event": "media",
                        "streamSid": msg.get("streamSid"),
                        "media": {"payload": msg["media"]["payload"]},
                    }
                    await ws.send(json.dumps(echo))

                elif event == "dtmf":
                    digit = msg.get("dtmf", {}).get("digit", "?")
                    log.info("  DTMF via WS: %s", digit)

                    # Hang up on '*'
                    if digit == "*":
                        log.info("  '*' pressed — hanging up call %s", call_id)
                        await hangup_call(call_id)

                elif event == "stop":
                    log.info("  stream stopped for call %s", call_id)
                    break

    except websockets.exceptions.ConnectionClosed:
        log.info("WebSocket closed for call %s", call_id)
    except asyncio.CancelledError:
        log.info("WebSocket task cancelled for call %s", call_id)
    except Exception as e:
        log.error("WebSocket error for call %s: %s", call_id, e)
    finally:
        ws_tasks.pop(call_id, None)


async def hangup_call(call_id: str):
    """Hang up a call via the xbridge REST API."""
    url = f"http://{XBRIDGE_HOST}/v1/calls/{call_id}"
    try:
        resp = await http_client.delete(url, headers=_auth_headers())
        if resp.status_code == 204:
            log.info("Hung up call %s", call_id)
        else:
            log.warning("Hangup failed for %s: HTTP %s", call_id, resp.status_code)
    except httpx.HTTPError as e:
        log.error("Hangup request failed for %s: %s", call_id, e)


# ── Entrypoint ──────────────────────────────────────────────────────


if __name__ == "__main__":
    log.info("xbridge echo bot starting on port %d", LISTEN_PORT)
    log.info("Expecting xbridge at %s", XBRIDGE_HOST)
    uvicorn.run(app, host="0.0.0.0", port=LISTEN_PORT, log_level="warning")
