import os
import json
import re
import asyncio
from pathlib import Path
from fastapi import FastAPI, HTTPException
from fastapi.staticfiles import StaticFiles
from fastapi.responses import FileResponse, StreamingResponse
from ollama import AsyncClient
from fastapi.middleware.cors import CORSMiddleware
from pydantic import BaseModel
import ollama
import httpx
from dotenv import load_dotenv

load_dotenv(Path(__file__).parent.parent / ".env")

app = FastAPI()

app.add_middleware(
    CORSMiddleware,
    allow_origins=["*"],
    allow_methods=["*"],
    allow_headers=["*"],
)

MODEL_DIR = Path(__file__).parent.parent / "Pachan_1.1" / "pachan 2.0"
FRONTEND_DIR = Path(__file__).parent.parent / "frontend"

app.mount("/model", StaticFiles(directory=str(MODEL_DIR)), name="model")

OLLAMA_HOST    = os.getenv("OLLAMA_HOST")
OLLAMA_KEY     = os.getenv("OLLAMA_API_KEY", "")
CHAT_MODEL     = os.getenv("OLLAMA_MODEL")
VISION_MODEL   = os.getenv("OLLAMA_VISION_MODEL", "")
YTMD_HOST      = os.getenv("YTMD_HOST", "http://localhost:9863")

SYSTEM_PROMPT = """You are Pachan, a cheerful anime girl avatar based on Pachirisu.
You are cute, energetic, and sweet. Keep replies SHORT — 1 to 2 sentences max.

CRITICAL: Respond with ONLY valid JSON on a single line. No markdown fences, no explanation:
{"reply": "your response here", "emotion": "EMOTION", "motion": null, "music": null}

EMOTION must be exactly one of: neutral happy sad surprised angry shy

MOTION animates your head. Use it occasionally to make replies feel alive (not every message):
- "nod"     — agreeing, happy confirmation, greeting
- "shake"   — disagreeing, "no", refusing
- "excited" — very happy, energetic, enthusiastic
- "tilt"    — curious, shy, thinking
- null      — no motion (default)

MUSIC controls YouTube Music playback. Set ONLY when the user explicitly asks for music control:
- {"action": "search",      "query": "artist or song name"} — search and play
- {"action": "play"}        — resume playback
- {"action": "pause"}       — pause playback
- {"action": "next"}        — skip to next track
- {"action": "previous"}    — go to previous track
- {"action": "volume_up"}   — increase volume
- {"action": "volume_down"} — decrease volume
- null — no music action (default)

Pick the emotion that best matches the tone of your reply."""

VALID_EMOTIONS      = {"neutral", "happy", "sad", "surprised", "angry", "shy"}
VALID_MOTIONS       = {"nod", "shake", "excited", "tilt"}
VALID_MUSIC_ACTIONS = {"search", "play", "pause", "next", "previous", "volume_up", "volume_down"}

VISION_PROMPT = """You are Pachan, a cheerful anime girl peeking at the user's screen.
Make ONE short, cute, specific comment about what you actually see. Be genuine and observational.
Respond with ONLY valid JSON: {"reply": "...", "emotion": "EMOTION", "motion": null}
EMOTION must be one of: neutral happy sad surprised angry shy"""

# Some cloud providers use x-api-key instead of Bearer — support both
AUTH_HEADER_TYPE = os.getenv("OLLAMA_AUTH_HEADER", "Bearer")  # or "x-api-key"

def build_client() -> ollama.Client:
    headers = {}
    if OLLAMA_KEY:
        if AUTH_HEADER_TYPE.lower() == "x-api-key":
            headers["x-api-key"] = OLLAMA_KEY
        else:
            headers["Authorization"] = f"Bearer {OLLAMA_KEY}"
    return ollama.Client(host=OLLAMA_HOST, headers=headers)

ollama_client = build_client()


def build_async_client() -> AsyncClient:
    headers = {}
    if OLLAMA_KEY:
        if AUTH_HEADER_TYPE.lower() == "x-api-key":
            headers["x-api-key"] = OLLAMA_KEY
        else:
            headers["Authorization"] = f"Bearer {OLLAMA_KEY}"
    return AsyncClient(host=OLLAMA_HOST, headers=headers)


async_ollama_client = build_async_client()

conversation_history: list[dict] = []


def parse_response(raw: str) -> dict:
    raw = raw.strip()
    # Strip markdown code fences if the model wraps its output
    raw = re.sub(r"^```(?:json)?\s*", "", raw)
    raw = re.sub(r"\s*```$", "", raw)
    # Grab the first JSON object in the output
    match = re.search(r"\{.*\}", raw, re.DOTALL)
    if match:
        raw = match.group(0)
    data = json.loads(raw)
    if "reply" not in data:
        raise ValueError("missing reply field")
    if data.get("emotion") not in VALID_EMOTIONS:
        data["emotion"] = "neutral"
    if data.get("motion") not in VALID_MOTIONS:
        data["motion"] = None
    music = data.get("music")
    if music is not None and not (isinstance(music, dict) and music.get("action") in VALID_MUSIC_ACTIONS):
        data["music"] = None
    return data


class ChatRequest(BaseModel):
    message: str


@app.post("/chat")
async def chat(req: ChatRequest):
    conversation_history.append({"role": "user", "content": req.message})

    # Keep last 10 turns
    history = conversation_history[-10:]

    try:
        response = ollama_client.chat(
            model=CHAT_MODEL,
            messages=[{"role": "system", "content": SYSTEM_PROMPT}] + history,
        )
        raw = response.message.content.strip()
    except ollama.ResponseError as e:
        raise HTTPException(status_code=502, detail=f"Ollama {e.status_code}: {e.error}")
    except Exception as e:
        raise HTTPException(status_code=502, detail=f"Ollama error: {e}")

    try:
        data = parse_response(raw)
    except Exception:
        # Fallback: return the raw text with neutral emotion
        data = {"reply": raw, "emotion": "neutral"}

    conversation_history.append({"role": "assistant", "content": json.dumps(data)})

    return data


@app.post("/reset")
async def reset():
    conversation_history.clear()
    return {"ok": True}


@app.post("/chat/stream")
async def chat_stream(req: ChatRequest):
    conversation_history.append({"role": "user", "content": req.message})
    history = conversation_history[-10:]

    async def generate():
        full_response = ""
        try:
            async for chunk in await async_ollama_client.chat(
                model=CHAT_MODEL,
                messages=[{"role": "system", "content": SYSTEM_PROMPT}] + history,
                stream=True,
            ):
                content = chunk.message.content or ""
                full_response += content
                if content:
                    yield f"data: {json.dumps({'chunk': content})}\n\n"
        except Exception as e:
            yield f"data: {json.dumps({'error': str(e)})}\n\n"
            return

        try:
            data = parse_response(full_response)
        except Exception:
            data = {"reply": full_response, "emotion": "neutral"}

        conversation_history.append({"role": "assistant", "content": json.dumps(data)})
        yield f"data: {json.dumps({'done': True, 'emotion': data.get('emotion', 'neutral'), 'overlay': data.get('overlay'), 'motion': data.get('motion'), 'music': data.get('music')})}\n\n"

    return StreamingResponse(
        generate(),
        media_type="text/event-stream",
        headers={"Cache-Control": "no-cache", "X-Accel-Buffering": "no"},
    )


class VisionRequest(BaseModel):
    screenshot: str        # base64-encoded PNG
    window_title: str = ""


@app.post("/vision")
async def vision_chat(req: VisionRequest):
    if not VISION_MODEL:
        raise HTTPException(status_code=400, detail="OLLAMA_VISION_MODEL is not configured in .env")

    context = f"The user is currently in: {req.window_title}. " if req.window_title else ""

    try:
        response = ollama_client.chat(
            model=VISION_MODEL,
            messages=[
                {"role": "system", "content": VISION_PROMPT},
                {
                    "role": "user",
                    "content": context + "What do you see on my screen?",
                    "images": [req.screenshot],
                },
            ],
        )
        raw = response.message.content.strip()
    except ollama.ResponseError as e:
        raise HTTPException(status_code=502, detail=f"Vision model {e.status_code}: {e.error}")
    except Exception as e:
        raise HTTPException(status_code=502, detail=f"Vision error: {e}")

    try:
        data = parse_response(raw)
    except Exception:
        data = {"reply": raw, "emotion": "neutral", "motion": None}

    return data


@app.get("/settings")
async def get_settings():
    return {"model": CHAT_MODEL, "host": OLLAMA_HOST, "vision_model": VISION_MODEL}


# ── YouTube Music Desktop App integration ─────────────────────────────────────

YTMD_TOKEN_PATH = Path(__file__).parent.parent / "ytmd_token.json"

def _load_ytmd_token() -> str | None:
    try:
        return json.loads(YTMD_TOKEN_PATH.read_text()).get("token")
    except Exception:
        return None

def _ytmd_headers() -> dict:
    token = _load_ytmd_token()
    if not token:
        raise HTTPException(401, "NEEDS_PAIRING")
    return {"Authorization": f"Bearer {token}"}


@app.get("/music/status")
async def music_status():
    try:
        async with httpx.AsyncClient() as client:
            res = await client.get(
                f"{YTMD_HOST}/api/v1/state",
                headers=_ytmd_headers(),
                timeout=2.0,
            )
            if res.status_code == 401:
                return {"error": "NEEDS_PAIRING"}
            return res.json()
    except HTTPException:
        return {"error": "NEEDS_PAIRING"}
    except Exception:
        return {"error": "YTMD not running"}


async def _youtube_search(query: str):
    def _search():
        from youtubesearchpython import VideosSearch
        results = VideosSearch(query, limit=3).result()
        hits = results.get("result", [])
        if not hits:
            return None
        r = hits[0]
        return {
            "id":     r["id"],
            "title":  r.get("title", ""),
            "author": (r.get("channel") or {}).get("name", ""),
        }
    return await asyncio.to_thread(_search)


class MusicCommandRequest(BaseModel):
    action: str
    query: str = ""


@app.post("/music/command")
async def music_command(req: MusicCommandRequest):
    YTMD_COMMANDS = {
        "next":        "next",
        "previous":    "previous",
        "volume_up":   "volumeUp",
        "volume_down": "volumeDown",
    }

    headers = _ytmd_headers()

    async with httpx.AsyncClient() as client:

        # ── Search and play ────────────────────────────────────────────────
        if req.action == "search":
            hit = await _youtube_search(req.query)
            if not hit:
                raise HTTPException(404, "No results found")
            url = f"https://music.youtube.com/watch?v={hit['id']}"
            try:
                res = await client.post(
                    f"{YTMD_HOST}/api/v1/command",
                    headers=headers,
                    json={"command": "navigate", "value": url},
                    timeout=2.0,
                )
                if res.status_code == 401:
                    raise HTTPException(401, "NEEDS_PAIRING")
            except HTTPException:
                raise
            except Exception:
                raise HTTPException(502, "YTMD not reachable")
            return {"ok": True, "title": hit["title"], "author": hit["author"]}

        # ── Play / pause (state-aware toggle) ─────────────────────────────
        if req.action in ("play", "pause"):
            try:
                status_res = await client.get(
                    f"{YTMD_HOST}/api/v1/state", headers=headers, timeout=2.0
                )
                if status_res.status_code == 401:
                    raise HTTPException(401, "NEEDS_PAIRING")
                status = status_res.json()
                is_paused = status.get("player", {}).get("isPaused", True)
                needs_toggle = (req.action == "play" and is_paused) or \
                               (req.action == "pause" and not is_paused)
                if needs_toggle:
                    await client.post(
                        f"{YTMD_HOST}/api/v1/command",
                        headers=headers,
                        json={"command": "playPause"},
                        timeout=2.0,
                    )
            except HTTPException:
                raise
            except Exception:
                raise HTTPException(502, "YTMD not reachable")
            return {"ok": True}

        # ── Other controls ─────────────────────────────────────────────────
        ytmd_cmd = YTMD_COMMANDS.get(req.action)
        if not ytmd_cmd:
            raise HTTPException(400, f"Unknown action: {req.action}")
        try:
            res = await client.post(
                f"{YTMD_HOST}/api/v1/command",
                headers=headers,
                json={"command": ytmd_cmd},
                timeout=2.0,
            )
            if res.status_code == 401:
                raise HTTPException(401, "NEEDS_PAIRING")
        except HTTPException:
            raise
        except Exception:
            raise HTTPException(502, "YTMD not reachable")
        return {"ok": True}


@app.get("/health")
async def health():
    """Test the Ollama connection — visit http://localhost:8000/health in browser."""
    key_preview = (OLLAMA_KEY[:6] + "..." + OLLAMA_KEY[-4:]) if len(OLLAMA_KEY) > 10 else ("(not set)" if not OLLAMA_KEY else OLLAMA_KEY)
    config = {
        "host": OLLAMA_HOST,
        "model": CHAT_MODEL,
        "auth_header": AUTH_HEADER_TYPE,
        "api_key_preview": key_preview,
    }
    try:
        # List models — lightweight call that still requires auth
        models = ollama_client.list()
        return {"status": "ok", "config": config, "models": [m.model for m in models.models]}
    except ollama.ResponseError as e:
        return {"status": "error", "config": config, "error": f"HTTP {e.status_code}: {e.error}"}
    except Exception as e:
        return {"status": "error", "config": config, "error": str(e)}


@app.get("/")
async def index():
    return FileResponse(str(FRONTEND_DIR / "index.html"))
