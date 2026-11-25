"""Stable Audio bridge: async generation and a bounded, continuous MP3 radio."""

import asyncio
import contextlib
import logging
import os
import secrets
import time
from collections import deque
from contextlib import asynccontextmanager
from dataclasses import dataclass, field
from pathlib import Path
from urllib.parse import quote

import httpx
from fastapi import FastAPI, HTTPException, Request
from fastapi.responses import StreamingResponse

from engine import Engine

log = logging.getLogger("stable-audio")
DATA = Path(os.environ.get("DATA_DIR", "/var/lib/quasar-stable-audio"))
API = os.environ.get("ALICE_API_URL", "").rstrip("/")
PUBLIC = os.environ.get("STREAM_URL", "").rstrip("/")
TOKEN = os.environ.get("ALICE_API_TOKEN", "")
BRIDGE_TOKEN = os.environ.get("BRIDGE_TOKEN", "")


def proto_struct(value):
    def wrap(item):
        if isinstance(item, bool):
            return {"boolValue": item}
        if isinstance(item, str):
            return {"stringValue": item}
        if isinstance(item, (int, float)):
            return {"numberValue": item}
        return {"structValue": proto_struct(item)}
    return {"fields": [{"key": key, "value": wrap(item)} for key, item in value.items()]}


@dataclass
class Session:
    duid: str
    prompt: str
    duration: int
    radio: bool
    id: str = field(default_factory=lambda: secrets.token_hex(24))
    stream_id: str = field(default_factory=lambda: "quasar-stable-audio-" + secrets.token_hex(16))
    state: str = "preparing"
    queue: deque = field(default_factory=deque)
    current: Path | None = None
    offset: int = 0
    generating: bool = False
    generated: int = 0
    generation_seconds: float | None = None
    repeating: bool = False
    error: str | None = None
    revision: int = 0
    listener: str | None = None
    changed: asyncio.Event = field(default_factory=asyncio.Event)
    commands: asyncio.Lock = field(default_factory=asyncio.Lock)
    task: asyncio.Task | None = None

    def status(self):
        return {"duid": self.duid, "id": self.id, "stream_id": self.stream_id,
                "mode": "radio" if self.radio else "track", "state": self.state,
                "prompt": self.prompt, "duration": self.duration, "generated": self.generated,
                "queued": len(self.queue), "generating": self.generating,
                "generation_seconds": self.generation_seconds, "repeating": self.repeating,
                "error": self.error}


class Bridge:
    def __init__(self):
        self.sessions = {}
        self.commands = {}
        self.engine = Engine(DATA)
        self.ready = False
        self.error = None
        self.gpu = asyncio.Lock()
        self.client = httpx.AsyncClient(timeout=10, trust_env=False)

    async def load(self):
        try:
            await asyncio.to_thread(self.engine.load)
            self.ready = True
        except Exception as error:
            log.error("Model loading failed: %s", type(error).__name__)
            self.error = ("Hugging Face не предоставил доступ к модели Stable Audio. Примите условия модели и войдите в аккаунт на сервере."
                          if type(error).__name__ == "GatedRepoError" else
                          "Не удалось загрузить Stable Audio. Проверьте журнал сервиса.")

    def status(self, duid):
        session = self.sessions.get(duid)
        result = session.status() if session else {"duid": duid, "state": "idle"}
        model_name = {"small-music": "Small Music", "medium": "Medium"}.get(self.engine.model_name, self.engine.model_name)
        return {**result, "configured": True, "ready": self.ready, "model": f"Stable Audio 3 {model_name}",
                "error": result.get("error") or self.error}

    async def push(self, session, name, payload):
        response = await self.client.post(
            f"{API}/devices/{quote(session.duid, safe='')}/push-directive",
            headers={"Authorization": f"Bearer {TOKEN}"} if TOKEN else {},
            json={"Name": name, "Type": "client_action", "Payload": proto_struct(payload), "IsLedSilent": True},
        )
        response.raise_for_status()
        if response.json().get("delivered", 0) < 1:
            raise RuntimeError("Колонка не подключена")

    async def play(self, session):
        async with session.commands:
            if session.state == "stopped":
                return
            session.revision += 1
            session.state = "buffering"
            url = f"{PUBLIC}/{session.id}.mp3?v={session.revision}"
            await self.push(session, "audio_play", {
                "background_mode": "Ducking", "callbacks": {}, "screen_type": "Music",
                "stream": {"format": "MP3", "id": session.stream_id, "type": "Track", "url": url,
                           "title": session.prompt, "subtitle": "Stable Audio"},
            })

    async def start(self, duid, arguments, radio):
        prompt = arguments.get("prompt")
        duration = arguments.get("duration", 120 if radio else 60)
        if not isinstance(prompt, str) or not 1 <= len(prompt.strip()) <= 1000:
            raise HTTPException(422, "Опишите музыку: от 1 до 1000 символов")
        if type(duration) is not int or not 10 <= duration <= 120:
            raise HTTPException(422, "Длительность должна быть целым числом от 10 до 120 секунд")
        if not self.ready:
            raise HTTPException(503, self.error or "Модель ещё загружается")
        if not API or not PUBLIC:
            raise HTTPException(503, "У сервиса не настроены ALICE_API_URL и STREAM_URL")
        # Check the destination before replacing an existing job or using GPU time.
        response = await self.client.get(f"{API}/devices", headers={"Authorization": f"Bearer {TOKEN}"} if TOKEN else {})
        response.raise_for_status()
        if not any(d.get("duid") == duid and d.get("connected") for d in response.json()):
            raise HTTPException(409, "Колонка не подключена")
        old = self.sessions.get(duid)
        if old:
            await self.stop(old)
        session = Session(duid, prompt.strip(), duration, radio)
        self.sessions[duid] = session
        session.task = asyncio.create_task(self.produce(session))
        return self.status(duid)

    async def produce(self, session):
        try:
            while session.state != "stopped":
                if len(session.queue) >= 2:
                    session.changed.clear()
                    await session.changed.wait()
                    continue
                session.generating = True
                # Never cancel an in-flight GPU call: to_thread would keep it
                # running and allow a second model call to overlap on the GPU.
                try:
                    async with self.gpu:
                        if session.state == "stopped":
                            break
                        path, elapsed = await asyncio.to_thread(self.engine.generate, session.prompt, session.duration)
                except Exception:
                    session.generating = False
                    if session.radio and (session.current or session.queue) and session.state != "stopped":
                        session.error = "Не удалось создать следующий трек. Продолжаем готовую музыку и повторим попытку."
                        log.warning("Radio generation failed; retrying in 30 seconds")
                        await asyncio.sleep(30)
                        continue
                    raise
                session.generating = False
                if session.state == "stopped":
                    path.unlink(missing_ok=True)
                    break
                session.queue.append(path)
                session.error = None
                session.generated += 1
                session.generation_seconds = elapsed
                session.changed.set()
                if session.generated == 1:
                    await self.play(session)
                if not session.radio:
                    break
        except Exception as error:
            if session.state == "stopped":
                return
            log.error("Generation/playback failed: %s", type(error).__name__)
            session.error = "Не удалось создать или включить музыку. Проверьте журнал Stable Audio и подключение колонки."
            session.state = "error"
            session.changed.set()
        finally:
            session.generating = False

    async def stop(self, session):
        async with session.commands:
            previous = session.state
            session.state = "stopped"
            session.revision += 1
            session.changed.set()
            if previous in ("playing", "buffering"):
                try:
                    await self.push(session, "audio_stop", {})
                except (httpx.HTTPError, RuntimeError):
                    # Cancellation still completes when the station is offline.
                    session.error = "Генерация остановлена. Колонка не подтвердила команду остановки."
        for path in session.queue:
            path.unlink(missing_ok=True)
        session.queue.clear()
        if session.current:
            session.current.unlink(missing_ok=True)
            session.current = None

    async def stream(self, session, revision):
        listener = secrets.token_hex(8)
        session.listener = listener
        session.state = "playing"
        started = time.monotonic()
        sent = 0
        try:
            while session.state == "playing" and revision == session.revision and listener == session.listener:
                if not session.current or session.offset >= session.current.stat().st_size:
                    if session.queue:
                        previous = session.current
                        session.current = session.queue.popleft()
                        if previous:
                            previous.unlink(missing_ok=True)
                        session.repeating = False
                        session.changed.set()
                    elif not session.current:
                        break
                    else:
                        # A slow generation must not turn an endless radio into silence.
                        session.repeating = True
                    session.offset = 0
                with session.current.open("rb") as source:
                    source.seek(session.offset)
                    while chunk := source.read(4800):
                        if session.state != "playing" or revision != session.revision or listener != session.listener:
                            return
                        session.offset += len(chunk)
                        yield chunk
                        sent += len(chunk)
                        # One clock for the entire connection: read-ahead must
                        # not grow by another second at every track boundary.
                        await asyncio.sleep(max(0, sent / 24000 - (time.monotonic() - started) - 1))
                if not session.radio:
                    if session.state == "playing" and revision == session.revision and listener == session.listener:
                        session.state = "finished"
                    return
        finally:
            if session.listener == listener:
                session.listener = None
                if session.state == "playing":
                    session.state = "paused"


@asynccontextmanager
async def lifespan(app):
    DATA.mkdir(parents=True, exist_ok=True)
    # Sessions are deliberately not auto-resumed after a service restart.
    for pattern in ("*.mp3", "*.wav"):
        for path in DATA.glob(pattern):
            path.unlink()
    app.state.bridge = Bridge()
    load = asyncio.create_task(app.state.bridge.load())
    yield
    for session in app.state.bridge.sessions.values():
        session.state = "stopped"
        session.changed.set()
    tasks = [s.task for s in app.state.bridge.sessions.values() if s.task]
    await asyncio.gather(load, *tasks, return_exceptions=True)
    await app.state.bridge.client.aclose()


app = FastAPI(lifespan=lifespan)


@app.middleware("http")
async def authenticate(request, call_next):
    if request.url.path in ("/functions", "/state") and BRIDGE_TOKEN:
        if not secrets.compare_digest(request.query_params.get("token", ""), BRIDGE_TOKEN):
            from fastapi.responses import JSONResponse
            return JSONResponse({"error": "Unauthorized"}, status_code=401)
    return await call_next(request)


def device(body):
    duid = body.get("metadata", {}).get("deviceId")
    if not isinstance(duid, str) or not 1 <= len(duid) <= 256:
        raise HTTPException(422, "Не указана колонка")
    return duid


@app.get("/health")
async def health(request: Request):
    bridge = request.app.state.bridge
    return {"ready": bridge.ready, "error": bridge.error}


@app.get("/functions")
async def functions():
    prompt = {"type": "string", "description": "Music description in English: genre, mood, instruments, tempo. Translate the user's request into English."}
    duration = {"type": "integer", "minimum": 10, "maximum": 120, "description": "Track duration in seconds"}
    specs = {
        "generate_music": ("Create original music with Stable Audio and play it on this speaker when ready. Use for requests to generate/compose music, not to find existing songs. Return immediately; report preparation, never claim it is already playing. Do not cache.", {"prompt": prompt, "duration": duration}),
        "start_music_radio": ("Start endless AI-generated music on this speaker. Prepare subsequent tracks in advance until stopped. Translate the desired style into English. Report preparation, not immediate playback. Do not cache.", {"prompt": prompt, "duration": duration}),
        "stop_music_radio": ("Stop generated music/radio and cancel pending generation on this speaker. Do not cache.", {}),
        "generated_music_status": ("Read the current generated music/radio status on this speaker. Do not cache.", {}),
        "pause_generated_music": ("Pause Stable Audio music/radio on this speaker. Do not cache.", {}),
        "resume_generated_music": ("Resume prepared Stable Audio music/radio on this speaker. Do not cache.", {}),
    }
    return {name: {"description": description, "arguments": arguments, "hasResponse": True}
            for name, (description, arguments) in specs.items()}


@app.patch("/functions")
async def call(request: Request):
    body = await request.json()
    duid = device(body)
    bridge = request.app.state.bridge
    name = body.get("name")
    arguments = body.get("arguments", {})
    if not isinstance(arguments, dict):
        raise HTTPException(422, "Некорректные параметры")
    if name == "generated_music_status":
        return bridge.status(duid)
    async with bridge.commands.setdefault(duid, asyncio.Lock()):
        return await command(bridge, duid, name, arguments)


async def command(bridge, duid, name, arguments):
    if name in ("generate_music", "start_music_radio"):
        return await bridge.start(duid, arguments, name == "start_music_radio")
    session = bridge.sessions.get(duid)
    if name == "stop_music_radio":
        if session:
            await bridge.stop(session)
    elif name == "pause_generated_music":
        if session:
            async with session.commands:
                if session.state in ("playing", "buffering"):
                    session.state = "paused"
                    session.revision += 1
                    await bridge.push(session, "audio_stop", {})
    elif name == "resume_generated_music":
        if not session or session.state not in ("paused", "finished") or not (session.current or session.queue):
            raise HTTPException(409, "Нет готовой музыки для воспроизведения")
        if session.state == "finished":
            session.offset = 0
        await bridge.play(session)
    else:
        raise HTTPException(422, "Неизвестная команда")
    return bridge.status(duid)


@app.post("/state")
async def state(request: Request):
    body = await request.json()
    status = request.app.state.bridge.status(device(body))
    return {"generated_music": {"description": "Stable Audio music and endless radio on this speaker", "value": status}}


@app.get("/stream/{session_id}.mp3")
async def stream(session_id: str, request: Request, v: int):
    bridge = request.app.state.bridge
    session = next((s for s in bridge.sessions.values() if s.id == session_id), None)
    if not session or v != session.revision or session.state not in ("buffering", "playing", "paused"):
        raise HTTPException(404, "Поток завершён")
    return StreamingResponse(bridge.stream(session, v), media_type="audio/mpeg", headers={"Cache-Control": "no-store", "Accept-Ranges": "none"})
