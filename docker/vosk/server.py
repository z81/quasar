"""Speech synthesis by Vosk TTS (https://github.com/alphacep/vosk-tts, Apache-2.0)
for alice-server's TTS_PROVIDER=piper: it speaks the HTTP of Piper's server,
POST /synthesize {"text": "..."} answered with the WAV of the whole text, so
PIPER_URL can point here. Optional request fields: speaker (a name of the
model, e.g. female_1) or speaker_id, and length_scale (above 1 is slower).

The model (VOSK_MODEL) is downloaded into VOSK_DATA_DIR at the first start.
VOSK_SPEAKER is the voice of requests without one, VOSK_THREADS the CPU
threads of a synthesis. One synthesis runs at a time, so the chunk asked for
first is spoken first.
"""
import io
import json
import logging
import os
import shutil
import signal
import sys
import threading
import time
import urllib.request
import wave
import zipfile
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

import onnxruntime
import vosk_tts

MODELS_URL = "https://alphacephei.com/vosk/models"
MODEL = os.environ.get("VOSK_MODEL", "vosk-model-tts-ru-0.9-multi")
DATA = Path(os.environ.get("VOSK_DATA_DIR", "/data"))
THREADS = int(os.environ.get("VOSK_THREADS", "4"))
PORT = int(os.environ.get("PORT", "5000"))

# vosk-tts logs every synthesis through the root logger
logging.basicConfig(level=logging.WARNING, format="%(asctime)s %(levelname)s %(message)s")
log = logging.getLogger("vosk-tts")
log.setLevel(logging.INFO)


def download(model_dir):
    """The model's zip unpacked in DATA/.download, then moved into place, so
    an interrupted download leaves no half of a model behind"""
    staging = DATA / ".download"
    shutil.rmtree(staging, ignore_errors=True)
    staging.mkdir(parents=True)
    archive = staging / f"{MODEL}.zip"
    url = f"{MODELS_URL}/{MODEL}.zip"
    log.info("downloading the model from %s", url)
    with urllib.request.urlopen(url, timeout=60) as response, open(archive, "wb") as file:
        total = int(response.headers.get("Content-Length") or 0)
        done, shown = 0, 0
        while chunk := response.read(1 << 20):
            file.write(chunk)
            done += len(chunk)
            if total and done * 10 // total > shown:
                shown = done * 10 // total
                log.info("downloaded %d%% of %d MB", shown * 10, total >> 20)
    with zipfile.ZipFile(archive) as zipped:
        zipped.extractall(staging)
    shutil.rmtree(model_dir, ignore_errors=True)
    (staging / MODEL).rename(model_dir)
    shutil.rmtree(staging)


def load(model_dir):
    """The model with sessions of THREADS threads: vosk-tts takes every core,
    which is twice as slow where other services share them"""
    model = vosk_tts.Model(model_path=str(model_dir))
    options = onnxruntime.SessionOptions()
    options.intra_op_num_threads = THREADS
    providers = ["CPUExecutionProvider"]
    model.onnx = onnxruntime.InferenceSession(str(model_dir / "model.onnx"), options, providers=providers)
    if model.tokenizer is not None:
        bert = model_dir / "bert" / "model.onnx"
        model.bert_onnx = onnxruntime.InferenceSession(str(bert), options, providers=providers)
    return model


def speaker_id(value):
    """The number of a speaker of the model, given by its name or number"""
    names = model.config.get("speaker_id_map") or {}
    count = int(model.config.get("num_speakers") or 1)
    if isinstance(value, int) or str(value).strip().isdigit():
        number = int(value)
        if 0 <= number < count:
            return number
    elif str(value).strip() in names:
        return int(names[str(value).strip()])
    known = ", ".join(names) or f"0 to {count - 1}"
    raise ValueError(f"no speaker {value!r} in {MODEL}, it has {known}")


def wav(audio):
    """16-bit mono samples as a WAV file"""
    buffer = io.BytesIO()
    with wave.open(buffer, "wb") as file:
        file.setnchannels(1)
        file.setsampwidth(2)
        file.setframerate(RATE)
        file.writeframes(audio.tobytes())
    return buffer.getvalue()


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path != "/info":
            return self.reply(404, "text/plain", b"not found")
        names = model.config.get("speaker_id_map") or {}
        info = {"model": MODEL, "speakers": list(names), "speaker_id": SPEAKER, "threads": THREADS, "sample_rate": RATE}
        self.reply(200, "application/json", json.dumps(info).encode())

    def do_POST(self):
        if self.path != "/synthesize":
            return self.reply(404, "text/plain", b"not found")
        try:
            request = json.loads(self.rfile.read(int(self.headers.get("Content-Length") or 0)) or b"{}")
            text = str(request.get("text") or "").strip()
            if not text:
                raise ValueError("no text to synthesize")
            speaker = SPEAKER
            if request.get("speaker_id") is not None:
                speaker = speaker_id(request["speaker_id"])
            elif request.get("speaker"):
                speaker = speaker_id(request["speaker"])
            scale = float(request.get("length_scale") or 1)
            if not 0 < scale < 10:
                raise ValueError(f"length_scale must be a positive number, not {scale}")
        except (ValueError, TypeError, AttributeError) as error:
            return self.reply(400, "text/plain; charset=utf-8", str(error).encode())
        started = time.perf_counter()
        try:
            with lock:
                waited = time.perf_counter() - started
                audio = synth.synth_audio(text, speaker_id=speaker, speech_rate=1 / scale)
        except Exception as error:
            log.exception("synthesis of %r failed", text)
            return self.reply(500, "text/plain; charset=utf-8", f"synthesis failed: {error}".encode())
        took = time.perf_counter() - started
        log.info("%d chars, speaker %d: %.1f s of speech in %d ms (%d ms waiting)",
                 len(text), speaker, len(audio) / RATE, took * 1000, waited * 1000)
        self.reply(200, "audio/wav", wav(audio))

    def reply(self, status, content_type, body):
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass  # one line per synthesis is enough


model_dir = DATA / MODEL
if not (model_dir / "model.onnx").is_file():
    download(model_dir)
model = load(model_dir)
synth = vosk_tts.Synth(model)
RATE = int(model.config["audio"]["sample_rate"])
SPEAKER = speaker_id(os.environ.get("VOSK_SPEAKER", "female_2"))
lock = threading.Lock()

server = ThreadingHTTPServer(("0.0.0.0", PORT), Handler)
server.daemon_threads = True
# as PID 1 of a container the process ignores SIGTERM without a handler
signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
log.info("Vosk TTS on port %d: %s, speaker %d of %s, %d threads",
         PORT, MODEL, SPEAKER, ", ".join(model.config.get("speaker_id_map") or {}) or "one", THREADS)
server.serve_forever()
