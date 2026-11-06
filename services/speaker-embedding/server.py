"""Private CPU speaker encoder. Audio lives only in request memory, never files.

Personalization only: no liveness/spoof detection and no authentication guarantee.
Model: https://huggingface.co/speechbrain/spkrec-ecapa-voxceleb
API: https://github.com/speechbrain/speechbrain/blob/v1.0.3/speechbrain/inference/speaker.py
"""
import json
import logging
import os
import socket
import subprocess
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import numpy as np
import torch
import webrtcvad
from speechbrain.inference.speaker import SpeakerRecognition

MODEL = "speechbrain-ecapa-0f99f2d0"
MAX_BYTES = 8 * 1024 * 1024
LOCK = threading.Lock()
logging.getLogger("speechbrain").setLevel(logging.ERROR)
torch.set_num_threads(2)
torch.set_num_interop_threads(1)
# All weights are downloaded at image-build time from a pinned revision.
ENCODER = SpeakerRecognition.from_hparams(
    source="/opt/model", savedir="/tmp/encoder", run_opts={"device": "cpu"},
    overrides={"pretrained_path": "/opt/model"},
)


def embedding(audio):
    # A fixed input format disallows playlists/URLs; no network protocols.
    decoded = subprocess.run(
        ["ffmpeg", "-nostdin", "-v", "error", "-protocol_whitelist", "pipe",
         "-f", "ogg", "-i", "pipe:0", "-t", "30", "-ac", "1", "-ar", "16000",
         "-f", "s16le", "pipe:1"],
        input=audio, capture_output=True, timeout=5, check=True,
    ).stdout
    # VAD evaluates real 30 ms speech frames, so silence cannot enroll a voice.
    vad = webrtcvad.Vad(2)
    frames = [decoded[i:i + 960] for i in range(0, len(decoded) - 959, 960)]
    speech = [frame for frame in frames if vad.is_speech(frame, 16000)]
    speech_seconds = len(speech) * 0.03
    if speech_seconds < 2.0:
        raise ValueError("insufficient_speech")
    signal = np.frombuffer(b"".join(speech), dtype="<i2").astype(np.float32) / 32768.0
    with torch.inference_mode():
        vector = ENCODER.encode_batch(torch.from_numpy(signal).unsqueeze(0)).flatten()
        vector = torch.nn.functional.normalize(vector, dim=0)
    if vector.numel() != 192 or not torch.isfinite(vector).all():
        raise ValueError("invalid_embedding")
    return {"model": MODEL, "embedding": vector.tolist(), "speech_seconds": speech_seconds}


class Handler(BaseHTTPRequestHandler):
    # Neither URLs, bodies, utterances, embeddings nor names go into logs.
    def log_message(self, *_args):
        pass

    def setup(self):
        super().setup()
        self.connection.settimeout(8)

    def respond(self, status, body):
        payload = json.dumps(body, allow_nan=False).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(payload)

    def do_GET(self):
        if self.path == "/health":
            self.respond(200, {"status": "ready", "model": MODEL})
        else:
            self.respond(404, {"error": "not_found"})

    def do_POST(self):
        if self.path != "/embedding":
            self.respond(404, {"error": "not_found"})
            return
        try:
            size = int(self.headers.get("Content-Length", "0"))
        except ValueError:
            size = 0
        if not 0 < size <= MAX_BYTES or self.headers.get("Content-Type") != "audio/ogg":
            self.respond(400, {"error": "invalid_audio"})
            return
        # Reject busy work instead of queuing voice recordings in memory.
        if not LOCK.acquire(blocking=False):
            self.respond(503, {"error": "busy"})
            return
        try:
            audio = self.rfile.read(size)
            if len(audio) != size:
                raise ValueError("incomplete_audio")
            result = embedding(audio)
            self.respond(200, result)
        except (ValueError, subprocess.SubprocessError):
            self.respond(422, {"error": "unusable_speech"})
        except (socket.timeout, ConnectionError):
            pass
        except Exception:
            self.respond(500, {"error": "inference_failed"})
        finally:
            LOCK.release()


if __name__ == "__main__":
    ThreadingHTTPServer(("0.0.0.0", 8091), Handler).serve_forever()
