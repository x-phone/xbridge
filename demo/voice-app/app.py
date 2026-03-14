"""
xbridge Voice AI Demo

Two modes:
  Voice AI  -- Deepgram STT/TTS conversational agent (needs API key)
  IVR Demo  -- DTMF phone menu with tone feedback (no API key needed)

Auto-detects xbridge WS encoding from the 'start' event:
  native mode  — binary PCM16 frames  (encoding: audio/x-l16)
  twilio mode  — JSON base64 mu-law   (encoding: audio/x-mulaw)
"""

import asyncio
import audioop
import base64
import json
import logging
import math
import os
import struct
import time
from pathlib import Path

import httpx
import uvicorn
import websockets
from fastapi import FastAPI, Request, WebSocket, WebSocketDisconnect
from fastapi.responses import JSONResponse
from fastapi.staticfiles import StaticFiles

logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s %(levelname)-5s %(message)s",
    datefmt="%H:%M:%S",
)
log = logging.getLogger("voice-app")

XBRIDGE = os.getenv("XBRIDGE_HOST", "localhost:8090")
PORT = int(os.getenv("PORT", "3000"))

app = FastAPI()
http_client = httpx.AsyncClient()


# -- PCM16 tone generation ----------------------------------------------------

def _tone_pcm16(freq: float, dur: float, vol: float = 0.4) -> bytes:
    """Generate a pure sine tone as PCM16 LE bytes (8 kHz)."""
    n = int(8000 * dur)
    amp = int(32000 * vol)
    return b"".join(
        struct.pack("<h", max(-32768, min(32767, int(amp * math.sin(2 * math.pi * freq * i / 8000)))))
        for i in range(n)
    )


def _silence_pcm16(dur: float) -> bytes:
    """Generate silence as PCM16 LE bytes (8 kHz)."""
    return b"\x00\x00" * int(8000 * dur)


def _melody_pcm16(notes: list[tuple[float, float]]) -> bytes:
    """Stitch (freq, duration) pairs into a melody. freq=0 is silence."""
    return b"".join(
        _tone_pcm16(f, d) if f else _silence_pcm16(d) for f, d in notes
    )


# Pre-generate IVR audio clips (PCM16)
_JINGLE = _melody_pcm16([(523, 0.12), (659, 0.12), (784, 0.12), (1047, 0.35), (0, 0.25)])
_BEEP = _tone_pcm16(880, 0.12, 0.3)
_ERR = _melody_pcm16([(220, 0.2), (0, 0.08), (220, 0.2)])
_BYE = _melody_pcm16([(1047, 0.12), (784, 0.12), (659, 0.12), (523, 0.35)])


# -- IVR menu tree -----------------------------------------------------------

IVR = {
    "main": {
        "text": (
            "Main Menu \u2014 Press 1 for business hours, "
            "2 for directions, 3 for a fun fact, * to hang up."
        ),
        "audio": _JINGLE,
        "next": {"1": "hours", "2": "directions", "3": "fact", "*": "_hangup"},
    },
    "hours": {
        "text": (
            "We\u2019re open Monday\u2013Friday 9 AM \u2013 5 PM, "
            "Saturday 10 AM \u2013 2 PM, closed Sunday. "
            "Press * for main menu."
        ),
        "audio": _BEEP,
        "next": {"*": "main", "1": "hours", "2": "directions", "3": "fact"},
    },
    "directions": {
        "text": (
            "123 Innovation Drive, Silicon Valley, CA 94025. "
            "Take exit 42 off Highway 101. "
            "Press * for main menu."
        ),
        "audio": _BEEP,
        "next": {"*": "main", "1": "hours", "2": "directions", "3": "fact"},
    },
    "fact": {
        "text": (
            "The first phone call was made on March 10, 1876 \u2014 "
            'Alexander Graham Bell said "Mr. Watson, come here, '
            'I want to see you." Press * for main menu.'
        ),
        "audio": _BEEP,
        "next": {"*": "main", "1": "hours", "2": "directions", "3": "fact"},
    },
}


# -- Shared state ------------------------------------------------------------

class Session:
    def __init__(self):
        self.mode: str | None = None  # "ai" or "ivr"
        self.deepgram_key: str | None = None
        self.system_prompt: str = "You are a friendly AI receptionist."
        self.frontend: WebSocket | None = None
        self.tasks: dict[str, asyncio.Task] = {}


session = Session()


async def _send(msg: dict):
    """Send JSON to the connected frontend (best-effort)."""
    if session.frontend:
        try:
            await session.frontend.send_json(msg)
        except Exception:
            session.frontend = None


async def _event(text: str, call_id: str | None = None):
    msg: dict = {"type": "event", "time": time.strftime("%H:%M:%S"), "message": text}
    if call_id:
        msg["call_id"] = call_id
    await _send(msg)


# -- Audio helpers (dual-mode) -----------------------------------------------

def _pcm16_to_ulaw(pcm_bytes: bytes) -> bytes:
    """Convert PCM16 LE to mu-law."""
    return audioop.lin2ulaw(pcm_bytes, 2)


def _ulaw_to_pcm16(ulaw_bytes: bytes) -> bytes:
    """Convert mu-law to PCM16 LE."""
    return audioop.ulaw2lin(ulaw_bytes, 2)


async def _send_audio(ws, pcm_bytes: bytes, stream_sid: str | None, chunk_ms: int = 20):
    """Send audio over xbridge WS, auto-detecting native vs twilio mode.

    stream_sid=None → native mode (binary PCM16 frames)
    stream_sid=str  → twilio mode (JSON base64 mu-law)
    """
    chunk_size = 2 * 8 * chunk_ms  # 320 bytes (PCM16 @ 8kHz, 20ms)
    for i in range(0, len(pcm_bytes), chunk_size):
        chunk = pcm_bytes[i : i + chunk_size]
        if stream_sid is None:
            # Native: [0x01][2-byte BE length][PCM16 LE data]
            frame = bytes([0x01]) + len(chunk).to_bytes(2, "big") + chunk
            await ws.send(frame)
        else:
            # Twilio: JSON media event with base64 mu-law
            ulaw = _pcm16_to_ulaw(chunk)
            await ws.send(json.dumps({
                "event": "media",
                "streamSid": stream_sid,
                "media": {"payload": base64.b64encode(ulaw).decode()},
            }))
        await asyncio.sleep(chunk_ms / 1000)


def _extract_audio(msg, stream_sid_holder: list) -> bytes | None:
    """Extract PCM16 audio from a WS message (native or twilio).
    Returns PCM16 bytes, or None if this is not an audio message.
    Also captures stream_sid from 'start' events and returns None for events.
    """
    if isinstance(msg, bytes):
        # Native binary frame: [0x01][2-byte len][PCM16 data]
        if len(msg) > 3 and msg[0] == 0x01:
            length = int.from_bytes(msg[1:3], "big")
            return msg[3 : 3 + length]
        return None
    # Text frame (JSON)
    data = json.loads(msg)
    ev = data.get("event")
    if ev == "start":
        sid = data.get("streamSid", "")
        encoding = data.get("start", {}).get("mediaFormat", {}).get("encoding", "")
        if encoding == "audio/x-mulaw":
            stream_sid_holder[0] = sid
            log.info("Detected twilio mode (stream_sid=%s)", sid)
        else:
            stream_sid_holder[0] = None
            log.info("Detected native mode (encoding=%s)", encoding)
    elif ev == "media":
        payload = data.get("media", {}).get("payload", "")
        if payload:
            return _ulaw_to_pcm16(base64.b64decode(payload))
    return None


def _is_stop(msg) -> bool:
    """Check if a WS message is a stop event."""
    if isinstance(msg, str):
        data = json.loads(msg)
        return data.get("event") == "stop"
    return False


def _is_dtmf(msg) -> str | None:
    """Extract DTMF digit from a WS message, or None."""
    if isinstance(msg, str):
        data = json.loads(msg)
        if data.get("event") == "dtmf":
            return data.get("dtmf", {}).get("digit", "")
    return None


def _is_start(msg) -> bool:
    """Check if a WS message is a start event."""
    if isinstance(msg, str):
        data = json.loads(msg)
        return data.get("event") == "start"
    return False


# -- Frontend WebSocket ------------------------------------------------------

@app.websocket("/app")
async def ws_frontend(ws: WebSocket):
    await ws.accept()
    session.frontend = ws
    log.info("Frontend connected")
    await _send({"type": "status", "connected": True})
    await _event("Connected to voice-app backend")
    try:
        while True:
            data = await ws.receive_json()
            t = data.get("type")
            if t == "configure":
                session.mode = data.get("mode", "ai")
                session.deepgram_key = data.get("deepgram_key")
                session.system_prompt = data.get(
                    "system_prompt", session.system_prompt
                )
                label = "Voice AI (Deepgram)" if session.mode == "ai" else "IVR Demo"
                await _event(f"Mode: {label}")
                await _send(
                    {"type": "configured", "ok": True, "mode": session.mode}
                )
                log.info("Configured: mode=%s", session.mode)
            elif t == "hangup":
                cid = data.get("call_id")
                if cid:
                    await _hangup(cid)
    except WebSocketDisconnect:
        session.frontend = None
        log.info("Frontend disconnected")


# -- Webhook handlers --------------------------------------------------------

@app.post("/webhook/incoming")
async def webhook_incoming(req: Request):
    body = await req.json()
    call_id = body.get("call_id", "?")
    caller = body.get("from", "?")
    callee = body.get("to", "?")
    log.info("Incoming %s: %s -> %s", call_id, caller, callee)
    await _send(
        {"type": "call.started", "call_id": call_id, "from": caller, "to": callee}
    )
    await _event(f"Incoming call {caller} \u2192 {callee}", call_id)
    return JSONResponse({"action": "accept", "stream": True})


@app.post("/webhook")
async def webhook_events(req: Request):
    body = await req.json()
    ev = body.get("event", "?")
    cid = body.get("call_id", "?")

    if ev == "call.answered":
        log.info("Call %s answered", cid)
        await _event("Call accepted, audio stream starting", cid)
        if cid in session.tasks:
            session.tasks.pop(cid).cancel()
        session.tasks[cid] = asyncio.create_task(_pipeline(cid))

    elif ev == "call.ended":
        log.info("Call %s ended", cid)
        task = session.tasks.pop(cid, None)
        if task:
            task.cancel()
        await _send(
            {
                "type": "call.ended",
                "call_id": cid,
                "reason": body.get("reason", "?"),
                "duration": body.get("duration", 0),
            }
        )
        await _event(f"Call ended ({body.get('reason', '?')})", cid)

    elif ev == "call.dtmf":
        digit = body.get("digit", "?")
        log.info("Call %s DTMF: %s", cid, digit)
        await _event(f"DTMF: {digit}", cid)

    else:
        log.info("Call %s: %s", cid, ev)
        await _event(f"{ev}", cid)

    return JSONResponse({"ok": True})


# -- Audio pipeline (dispatcher) ---------------------------------------------

async def _pipeline(call_id: str):
    url = f"ws://{XBRIDGE}/ws/{call_id}?mode=native"
    try:
        async with websockets.connect(url) as ws:
            log.info("xbridge WS connected for %s", call_id)
            if session.mode == "ai" and session.deepgram_key:
                await _ai_pipeline(ws, call_id)
            elif session.mode == "ivr":
                await _ivr_pipeline(ws, call_id)
            else:
                await _event("No Deepgram key \u2014 echo mode", call_id)
                await _echo(ws)
    except asyncio.CancelledError:
        log.info("Pipeline cancelled for %s", call_id)
    except Exception as e:
        log.error("Pipeline error for %s: %s", call_id, e)
        await _event(f"Error: {e}", call_id)
    finally:
        session.tasks.pop(call_id, None)
        # Always notify frontend when pipeline ends (WS closed, cancelled, error)
        await _send({"type": "call.ended", "call_id": call_id, "reason": "ended"})
        await _event("Call ended", call_id)


# -- Echo mode (works in both native and twilio mode) ----------------------

async def _echo(ws):
    async for msg in ws:
        if isinstance(msg, bytes):
            await ws.send(msg)  # native: echo binary frame as-is
        elif isinstance(msg, str):
            data = json.loads(msg)
            if data.get("event") == "stop":
                break
            elif data.get("event") == "media":
                await ws.send(msg)  # twilio: echo JSON media frame as-is


# -- IVR mode ----------------------------------------------------------------

async def _ivr_pipeline(ws, call_id: str):
    await _event("IVR demo started", call_id)

    started = asyncio.Event()
    dtmf_q: asyncio.Queue[str] = asyncio.Queue()
    stream_sid = [None]  # None = native, str = twilio

    async def reader():
        async for msg in ws:
            if _is_start(msg):
                _extract_audio(msg, stream_sid)  # detect mode
                started.set()
            elif _is_stop(msg):
                await dtmf_q.put("_stop")
                break
            else:
                digit = _is_dtmf(msg)
                if digit:
                    await dtmf_q.put(digit)
                # ignore audio frames in IVR mode

    async def controller():
        await started.wait()
        state = "main"

        while True:
            node = IVR.get(state)
            if not node:
                break

            await _send(
                {
                    "type": "transcript",
                    "call_id": call_id,
                    "role": "system",
                    "text": node["text"],
                    "is_final": True,
                }
            )
            await _send_audio(ws, node["audio"], stream_sid[0])

            while True:
                digit = await dtmf_q.get()
                if digit == "_stop":
                    return

                await _send(
                    {
                        "type": "transcript",
                        "call_id": call_id,
                        "role": "caller",
                        "text": f"Pressed {digit}",
                        "is_final": True,
                    }
                )
                await _event(f"DTMF: {digit}")

                nxt = node["next"].get(digit)
                if nxt == "_hangup":
                    await _send(
                        {
                            "type": "transcript",
                            "call_id": call_id,
                            "role": "system",
                            "text": "Goodbye! Thanks for trying the xbridge demo.",
                            "is_final": True,
                        }
                    )
                    await _send_audio(ws, _BYE, stream_sid[0])
                    await asyncio.sleep(0.5)
                    await _hangup(call_id)
                    return
                elif nxt:
                    state = nxt
                    break
                else:
                    await _send(
                        {
                            "type": "transcript",
                            "call_id": call_id,
                            "role": "system",
                            "text": "Invalid option. Please try again.",
                            "is_final": True,
                        }
                    )
                    await _send_audio(ws, _ERR, stream_sid[0])

    await asyncio.gather(reader(), controller(), return_exceptions=True)


# -- AI mode (Deepgram Voice Agent API) --------------------------------------

AGENT_URL = "wss://agent.deepgram.com/v1/agent/converse"


async def _ai_pipeline(ws, call_id: str):
    stream_sid = [None]  # None = native, str = twilio

    # Consume connected + start events to detect encoding mode
    for _ in range(5):
        msg = await ws.recv()
        if _is_start(msg):
            _extract_audio(msg, stream_sid)
            break
    is_native = stream_sid[0] is None
    mode_label = "native/linear16" if is_native else "twilio/mulaw"
    await _event(f"Audio mode: {mode_label}", call_id)

    # Connect to Deepgram Voice Agent
    async with websockets.connect(
        AGENT_URL,
        additional_headers={"Authorization": f"Token {session.deepgram_key}"},
    ) as dg:
        # Wait for Welcome
        welcome = json.loads(await dg.recv())
        log.info("Deepgram Agent welcome: %s", welcome.get("request_id", "?"))

        # Send Settings — use mulaw 8kHz (telephony native rate, always respected)
        settings = {
            "type": "Settings",
            "audio": {
                "input": {"encoding": "mulaw", "sample_rate": 8000},
                "output": {"encoding": "mulaw", "sample_rate": 8000, "container": "none"},
            },
            "agent": {
                "language": "en",
                "listen": {"provider": {"type": "deepgram", "model": "nova-3"}},
                "think": {
                    "provider": {"type": "open_ai", "model": "gpt-4o-mini", "temperature": 0.7},
                    "prompt": session.system_prompt,
                },
                "speak": {
                    "provider": {"type": "deepgram", "model": "aura-asteria-en"},
                },
                "greeting": "Hello! Thank you for calling. How can I help you today?",
            },
        }
        await dg.send(json.dumps(settings))

        applied = json.loads(await dg.recv())
        log.info("Deepgram Agent settings applied: %s", applied.get("type"))
        await _event("Deepgram Voice Agent connected", call_id)

        async def fwd_caller():
            """xbridge → Deepgram Agent (caller audio as mulaw)."""
            try:
                async for msg in ws:
                    if _is_stop(msg):
                        break
                    pcm = _extract_audio(msg, stream_sid)
                    if pcm:
                        await dg.send(_pcm16_to_ulaw(pcm))
            finally:
                try:
                    await dg.send(b"")
                except Exception:
                    pass

        async def read_agent():
            """Deepgram Agent → xbridge (TTS audio) + frontend (transcripts).
            xbridge's paced_pcm_writer handles RTP timing — just forward audio."""
            try:
                async for msg in dg:
                    if isinstance(msg, bytes):
                        # mulaw from Deepgram → forward to xbridge (paced by xbridge)
                        if stream_sid[0] is not None:
                            # Twilio mode: forward mulaw directly
                            await ws.send(json.dumps({
                                "event": "media",
                                "streamSid": stream_sid[0],
                                "media": {"payload": base64.b64encode(msg).decode()},
                            }))
                        else:
                            # Native mode: mulaw → PCM16 → binary frame
                            pcm = _ulaw_to_pcm16(msg)
                            frame = bytes([0x01]) + len(pcm).to_bytes(2, "big") + pcm
                            await ws.send(frame)
                    elif isinstance(msg, str):
                        data = json.loads(msg)
                        t = data.get("type")
                        if t == "ConversationText":
                            role = "caller" if data.get("role") == "user" else "ai"
                            await _send({
                                "type": "transcript",
                                "call_id": call_id,
                                "role": role,
                                "text": data.get("content", ""),
                                "is_final": True,
                            })
                        elif t == "Error":
                            log.error("Agent error: %s", data)
                            await _event(f"Agent error: {data.get('description', '?')}")
            except Exception as e:
                log.error("read_agent error: %s", e)

        async def keepalive():
            """Send KeepAlive every 5s to prevent Deepgram timeout."""
            try:
                while True:
                    await asyncio.sleep(5)
                    await dg.send(json.dumps({"type": "KeepAlive"}))
            except Exception:
                pass

        await asyncio.gather(
            fwd_caller(), read_agent(), keepalive(),
            return_exceptions=True,
        )





# -- Hangup helper -----------------------------------------------------------

async def _hangup(call_id: str):
    try:
        r = await http_client.delete(f"http://{XBRIDGE}/v1/calls/{call_id}")
        log.info("Hangup %s: %s", call_id, r.status_code)
    except Exception as e:
        log.error("Hangup failed: %s", e)
    # Cancel pipeline and notify frontend
    task = session.tasks.pop(call_id, None)
    if task:
        task.cancel()
    await _send({"type": "call.ended", "call_id": call_id, "reason": "hangup"})
    await _event("Call ended", call_id)


# -- Static files ------------------------------------------------------------

_static = Path(__file__).parent / "static"
if _static.exists():
    app.mount("/", StaticFiles(directory=str(_static), html=True), name="static")


# -- Entrypoint --------------------------------------------------------------

if __name__ == "__main__":
    log.info("Starting on :%d  xbridge=%s", PORT, XBRIDGE)
    uvicorn.run(app, host="0.0.0.0", port=PORT, log_level="warning")
