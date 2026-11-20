"""Local whisper detector: YAMNet over the station's OGG/Opus utterance.

The service only classifies; it never stores or returns the audio. The server
sends one utterance per request and keeps the last result for the panel.
"""

import csv
import json
import os
import subprocess
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import numpy as np
from tflite_runtime.interpreter import Interpreter

MODEL = os.path.join(os.path.dirname(__file__), "yamnet.tflite")
CLASS_MAP = os.path.join(os.path.dirname(__file__), "yamnet_class_map.csv")
SAMPLE_RATE = 16000
WINDOW = 15600  # 0.975 s, the model's input
HOP = WINDOW // 2
MAX_BODY = 8 * 1024 * 1024
THRESHOLD = float(os.environ.get("WHISPER_THRESHOLD", "0.45"))
FRAME = 640  # 40 ms at 16 kHz
HOP = 320


def load_class_index(name: str) -> int:
    with open(CLASS_MAP, newline="", encoding="utf-8") as handle:
        for row in csv.DictReader(handle):
            if row["display_name"].strip().lower() == name.lower():
                return int(row["index"])
    raise RuntimeError(f"{name} is missing from the YAMNet class map")


interpreter = Interpreter(model_path=MODEL, num_threads=max(1, min(4, os.cpu_count() or 1)))
interpreter.allocate_tensors()
input_details = interpreter.get_input_details()
output_details = interpreter.get_output_details()
whisper_index = load_class_index("Whispering")
lock = threading.Lock()


def decode(ogg: bytes) -> np.ndarray:
    process = subprocess.run(
        [
            "ffmpeg", "-hide_banner", "-loglevel", "error",
            "-i", "pipe:0", "-f", "s16le", "-ac", "1", "-ar", str(SAMPLE_RATE), "pipe:1",
        ],
        input=ogg,
        capture_output=True,
        timeout=12,
        check=False,
    )
    if process.returncode != 0:
        raise ValueError("ffmpeg could not decode the utterance")
    pcm = np.frombuffer(process.stdout, dtype="<i2").astype(np.float32) / 32768.0
    if pcm.size == 0:
        raise ValueError("the utterance has no audio")
    return pcm


def yamnet_score(pcm: np.ndarray) -> float:
    starts = list(range(0, max(1, pcm.size - WINDOW + 1), WINDOW // 2))
    if starts[-1] + WINDOW > pcm.size:
        starts.append(max(0, pcm.size - WINDOW))
        starts = sorted(set(starts))
    scores = []
    with lock:
        for start in starts:
            window = pcm[start:start + WINDOW]
            if window.size < WINDOW:
                window = np.pad(window, (0, WINDOW - window.size))
            window = window.astype(np.float32).reshape(input_details[0]["shape"])
            interpreter.set_tensor(input_details[0]["index"], window)
            interpreter.invoke()
            frame = interpreter.get_tensor(output_details[0]["index"])[0]
            scores.append(float(frame[whisper_index]))
    top = sorted(scores)[-min(3, len(scores)):]
    return float(np.mean(top))


def voice_features(pcm: np.ndarray) -> tuple[float, float, float, int]:
    """Pitch strength and spectral flatness over speech frames.

    Whispered speech is unvoiced: its autocorrelation has no strong pitch peak
    and its spectrum is flatter than a voiced vowel. This complements YAMNet,
    whose AudioSet "Whispering" class is conservative for intelligible speech.
    """
    pitches, flats, energies = [], [], []
    for start in range(0, max(1, pcm.size - FRAME + 1), HOP):
        window = pcm[start:start + FRAME]
        if window.size < FRAME:
            break
        window = window - window.mean()
        energy = float(np.sqrt(np.mean(window * window)))
        if energy < 0.003:
            continue
        spectrum = np.fft.rfft(window * np.hanning(FRAME), n=FRAME * 2)
        power = np.abs(spectrum) ** 2
        autoc = np.fft.irfft(power)[:FRAME]
        if autoc[0] <= 0:
            continue
        autoc = autoc / autoc[0]
        low, high = int(SAMPLE_RATE / 400), int(SAMPLE_RATE / 70)
        pitches.append(float(np.max(autoc[low:high])))
        positive = power[1:] + 1e-12
        flats.append(float(np.exp(np.mean(np.log(positive))) / np.mean(positive)))
        energies.append(energy)
    if not pitches:
        return 0.0, 0.0, 0.0, 0
    return (
        float(np.median(pitches)),
        float(np.median(flats)),
        float(np.median(energies)),
        len(pitches),
    )


def classify(ogg: bytes) -> dict:
    pcm = decode(ogg)
    yamnet = yamnet_score(pcm)
    pitch, flatness, energy, frames = voice_features(pcm)
    # 0.55 separates the voiced vowels of normal speech (>0.5) from the
    # unvoiced excitation of a whisper (usually <0.3); flatness is high for
    # breathy/whispered sounds and very low for voiced ones.
    voicing = max(0.0, min(1.0, (0.55 - pitch) / 0.35))
    flat = max(0.0, min(1.0, (flatness - 0.005) / 0.05))
    acoustic = (voicing + flat) / 2.0 if frames else 0.0
    score = max(yamnet, acoustic)
    return {
        "whisper": score >= THRESHOLD,
        "score": round(score, 4),
        "yamnet": round(yamnet, 4),
        "pitch": round(pitch, 4),
        "flatness": round(flatness, 5),
        "rms": round(energy, 4),
        "threshold": THRESHOLD,
        "frames": frames,
        "seconds": round(pcm.size / SAMPLE_RATE, 2),
    }


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):
        sys.stderr.write("%s %s\n" % (self.address_string(), fmt % args))

    def reply(self, status: int, value: dict):
        body = json.dumps(value, ensure_ascii=False).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if self.path == "/health":
            self.reply(200, {"ok": True, "model": "yamnet", "whisper_class": whisper_index, "threshold": THRESHOLD})
        else:
            self.reply(404, {"error": "not found"})

    def do_POST(self):
        if self.path != "/classify":
            self.reply(404, {"error": "not found"})
            return
        try:
            length = int(self.headers.get("Content-Length", "0"))
        except ValueError:
            self.reply(400, {"error": "bad content length"})
            return
        if length <= 0 or length > MAX_BODY:
            self.reply(413, {"error": "utterance must be between 1 byte and 8 MiB"})
            return
        try:
            result = classify(self.rfile.read(length))
        except subprocess.TimeoutExpired:
            self.reply(422, {"error": "decoding took too long"})
            return
        except Exception as error:  # noqa: BLE001 - a single bad request must not stop the service
            self.reply(422, {"error": str(error)})
            return
        self.reply(200, result)


if __name__ == "__main__":
    server = ThreadingHTTPServer(("0.0.0.0", 8080), Handler)
    print(f"whisper detector listening on :8080, whisper class {whisper_index}, threshold {THRESHOLD}", flush=True)
    server.serve_forever()
