"""Persistent GigaAM Vulkan worker; accepts mono PCM16 LE at 16 kHz."""

import argparse
import json
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

import numpy as np
import transcribe_cpp


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def reply(self, status, body):
        data = json.dumps(body, ensure_ascii=False).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        if self.path != "/health":
            self.reply(404, {"error": "not_found"})
            return
        self.reply(200, {
            "ready": True,
            "model": self.server.model_name,
            "backend": self.server.model.device.kind,
            "device": self.server.model.device.description,
            "busy": self.server.inference_lock.locked(),
        })

    def do_POST(self):
        if self.path != "/inference":
            self.reply(404, {"error": "not_found"})
            return
        try:
            length = int(self.headers.get("Content-Length", "0"))
            if length <= 0 or length % 2:
                raise ValueError("expected PCM16 LE samples")
        except ValueError as error:
            self.reply(400, {"error": str(error)})
            return
        if not self.server.inference_lock.acquire(blocking=False):
            self.reply(503, {"error": "busy"})
            return
        try:
            raw = self.rfile.read(length)
            if len(raw) != length:
                self.reply(400, {"error": "incomplete_audio"})
                return
            pcm = np.frombuffer(raw, dtype="<i2").astype(np.float32) / 32768.0
            started = time.monotonic()
            result = self.server.session.run(pcm, language="ru", timestamps="none")
            elapsed = time.monotonic() - started
            self.reply(200, {
                "text": result.text.strip(),
                "seconds": round(elapsed, 4),
                "audio_seconds": len(pcm) / 16000,
            })
        except (BrokenPipeError, ConnectionResetError):
            pass
        except Exception as error:
            print("gigaam_failed: " + str(error), flush=True)
            self.reply(500, {"error": "recognition_failed"})
        finally:
            self.server.inference_lock.release()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--model", required=True)
    parser.add_argument("--port", type=int, default=8097)
    parser.add_argument("--threads", type=int, default=2)
    args = parser.parse_args()
    with transcribe_cpp.Model(args.model, backend="vulkan") as model:
        with model.session(n_threads=args.threads) as session:
            with ThreadingHTTPServer(("127.0.0.1", args.port), Handler) as server:
                server.model = model
                server.model_name = Path(args.model).stem
                server.session = session
                server.inference_lock = threading.Lock()
                print(f"GigaAM ready on 127.0.0.1:{args.port}: {model.device.description}", flush=True)
                server.serve_forever()


if __name__ == "__main__":
    main()
