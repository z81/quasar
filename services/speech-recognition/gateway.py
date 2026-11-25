# Gigastt-compatible gateway: Whisper supplies the final transcript while
# optional GigaAM shadow recognition compares the same audio independently.

import asyncio
import io
import json
import os
import re
import time
import uuid
import wave

import requests
import webrtcvad
import websockets

SAMPLE_RATE = 16_000
BYTES_PER_SECOND = SAMPLE_RATE * 2
FRAME_MS = 20
FRAME_BYTES = SAMPLE_RATE * 2 * FRAME_MS // 1000  # 640
PAD_BYTES = int(0.2 * BYTES_PER_SECOND)
WHISPER_URL = "http://127.0.0.1:8098/inference"
GIGAAM_SHADOW_URL = os.environ.get("GIGAAM_SHADOW_URL", "")
LANGUAGE = "ru"
shadow_task = None


def wav_bytes(pcm: bytes) -> bytes:
    out = io.BytesIO()
    with wave.open(out, "wb") as wav:
        wav.setnchannels(1)
        wav.setsampwidth(2)
        wav.setframerate(SAMPLE_RATE)
        wav.writeframes(pcm)
    return out.getvalue()


def call_whisper(pcm: bytes):
    started = time.monotonic()
    response = requests.post(
        WHISPER_URL,
        files={"file": ("utterance.wav", wav_bytes(pcm), "audio/wav")},
        data={
            "response_format": "verbose_json",
            "language": LANGUAGE,
            "temperature": "0",
        },
        timeout=60,
    )
    response.raise_for_status()
    body = response.json()
    return body.get("text", "").strip(), body.get("segments", []), time.monotonic() - started


def call_gigaam(pcm: bytes):
    started = time.monotonic()
    response = requests.post(
        GIGAAM_SHADOW_URL,
        data=pcm,
        headers={"Content-Type": "application/octet-stream"},
        timeout=(1, 10),
    )
    response.raise_for_status()
    return response.json()["text"], time.monotonic() - started


async def compare_shadow(pcm, primary, utterance_id, reason, started):
    record = {
        "utterance_id": utterance_id,
        "reason": reason,
        "primary": "whisper",
        "audio_seconds": round(len(pcm) / BYTES_PER_SECOND, 3),
    }
    try:
        text, elapsed = await asyncio.to_thread(call_gigaam, pcm)
        record["gigaam"] = {"text": text, "seconds": round(elapsed, 3)}
    except Exception as error:
        record["gigaam"] = {"error": str(error)[:300]}
    try:
        text, _, elapsed = await asyncio.shield(primary)
        record["whisper"] = {"text": text, "seconds": round(elapsed, 3)}
    except Exception as error:
        record["whisper"] = {"error": str(error)[:300]}
    record["pair_seconds"] = round(time.monotonic() - started, 3)
    if "text" in record["whisper"] and "text" in record["gigaam"]:
        normalized = [
            " ".join(re.findall(r"\w+", record[name]["text"].casefold().replace("ё", "е")))
            for name in ["whisper", "gigaam"]
        ]
        record["agreement"] = normalized[0] == normalized[1]
    print("asr_comparison " + json.dumps(record, ensure_ascii=False), flush=True)


class Session:
    def __init__(self, websocket):
        self.ws = websocket
        self.vad = webrtcvad.Vad(2)
        self.endpoint_mode = "manual"
        self.min_silence_bytes = int(1.2 * BYTES_PER_SECOND)
        self.buffer = bytearray()
        self.pending = bytearray()
        self.base_bytes = 0
        self.total_bytes = 0
        self.utterance_start = None
        self.last_voice = None
        self.silence_bytes = 0
        self.closed = False

    def configure(self, message):
        self.endpoint_mode = message.get("endpoint_mode", "manual")
        if "min_silence_ms" in message:
            self.min_silence_bytes = int(message["min_silence_ms"]) * BYTES_PER_SECOND // 1000

    async def send(self, message):
        if not self.closed:
            await self.ws.send(json.dumps(message, ensure_ascii=False))

    async def final(self, reason: str):
        global shadow_task
        if self.utterance_start is None or self.last_voice is None:
            await self.send({"type": "final", "text": "", "words": [], "endpoint_reason": reason})
            return
        start = self.utterance_start
        end = min(len(self.buffer), self.last_voice + PAD_BYTES)
        pcm = bytes(self.buffer[start:end])
        self.utterance_start = None
        self.last_voice = None
        self.silence_bytes = 0
        if len(pcm) < int(0.2 * BYTES_PER_SECOND):
            await self.send({"type": "final", "text": "", "words": [], "endpoint_reason": reason})
            return
        started = time.monotonic()
        primary = asyncio.create_task(asyncio.to_thread(call_whisper, pcm))
        if GIGAAM_SHADOW_URL:
            # Keep at most one comparison in flight; a slow shadow must never
            # build a queue or hold up the primary transcript.
            if shadow_task is None or shadow_task.done():
                shadow_task = asyncio.create_task(
                    compare_shadow(pcm, primary, uuid.uuid4().hex, reason, started)
                )
            else:
                print("asr_shadow_skipped: previous comparison still running", flush=True)
        try:
            text, segments, elapsed = await primary
        except Exception as error:
            await self.send({"type": "error", "code": "whisper_failed", "message": str(error)[:300]})
            return
        offset = (self.base_bytes + start) / BYTES_PER_SECOND
        words = []
        for segment in segments:
            words.append(
                {
                    "start": offset + float(segment.get("start", 0.0)),
                    "end": offset + float(segment.get("end", 0.0)),
                }
            )
        if not words:
            words.append(
                {
                    "start": offset,
                    "end": offset + len(pcm) / BYTES_PER_SECOND,
                }
            )
        print(
            "final %s: %.2f s audio, %.2f s decode: %s"
            % (reason, len(pcm) / BYTES_PER_SECOND, elapsed, text),
            flush=True,
        )
        await self.send({"type": "final", "text": text, "words": words, "endpoint_reason": reason})
        # Drop the decoded utterance; bytes after the endpoint belong to the
        # next one and stay in the buffer.
        leftover = bytes(self.pending)
        self.base_bytes += len(self.buffer) - len(leftover)
        self.buffer = bytearray(leftover)
        self.pending = bytearray(leftover)

    async def audio(self, data: bytes):
        self.total_bytes += len(data)
        self.buffer.extend(data)
        self.pending.extend(data)
        while len(self.pending) >= FRAME_BYTES:
            frame = bytes(self.pending[:FRAME_BYTES])
            del self.pending[:FRAME_BYTES]
            speech = self.vad.is_speech(frame, SAMPLE_RATE)
            if speech:
                if self.utterance_start is None:
                    self.utterance_start = max(0, len(self.buffer) - FRAME_BYTES - PAD_BYTES)
                self.last_voice = len(self.buffer)
                self.silence_bytes = 0
            elif self.utterance_start is not None:
                self.silence_bytes += FRAME_BYTES
                if self.endpoint_mode == "auto" and self.silence_bytes >= self.min_silence_bytes:
                    await self.final("vad")

    async def run(self):
        await self.send({"type": "ready", "supported_rates": [SAMPLE_RATE]})
        try:
            async for message in self.ws:
                if isinstance(message, str):
                    try:
                        body = json.loads(message)
                    except Exception:
                        continue
                    kind = body.get("type")
                    if kind == "configure":
                        self.configure(body)
                    elif kind == "stop":
                        await self.final("stop")
                        break
                else:
                    await self.audio(message)
        except websockets.ConnectionClosed:
            pass
        finally:
            self.closed = True


async def handler(websocket):
    print("client connected", websocket.remote_address, flush=True)
    await Session(websocket).run()
    print("client disconnected", websocket.remote_address, flush=True)


async def main():
    async with websockets.serve(
        handler,
        "0.0.0.0",
        8099,
        max_size=1 << 20,
        ping_interval=20,
    ):
        print("whisper streaming gateway on 0.0.0.0:8099", flush=True)
        await asyncio.Future()


if __name__ == "__main__":
    asyncio.run(main())
