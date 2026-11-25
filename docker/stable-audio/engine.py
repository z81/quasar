"""The model stays in one worker; speech services keep their own environment."""

import os
import secrets
import subprocess
import time
from pathlib import Path


class Engine:
    def __init__(self, directory: Path):
        self.directory = directory
        self.model = None

    def load(self):
        import torch
        from stable_audio_3 import StableAudioModel

        torch.set_num_threads(2)
        # Radeon VII does not support CUDA Flash Attention. PyTorch's math
        # attention works on its ROCm backend without compiling GPU kernels.
        if torch.version.hip:
            torch.backends.cuda.enable_flash_sdp(False)
            torch.backends.cuda.enable_mem_efficient_sdp(False)
        self.model = StableAudioModel.from_pretrained(
            "small-music", device=os.environ.get("STABLE_AUDIO_DEVICE", "cuda")
        )

    def generate(self, prompt: str, duration: int):
        import numpy as np
        import soundfile as sf

        started = time.monotonic()
        name = secrets.token_hex(16)
        wav = self.directory / f"{name}.wav"
        mp3 = self.directory / f"{name}.mp3"
        audio = self.model.generate(prompt=prompt, duration=duration, steps=8, chunked_decode=True)
        samples = audio[0].float().cpu().transpose(0, 1).numpy()
        if not np.isfinite(samples).all() or np.max(np.abs(samples)) < 0.00001:
            raise RuntimeError("Модель вернула пустой или повреждённый звук")
        sf.write(wav, samples, 44100, subtype="PCM_16")
        try:
            subprocess.run(
                ["ffmpeg", "-nostdin", "-v", "error", "-y", "-i", str(wav),
                 "-af", f"afade=t=in:d=0.03,afade=t=out:st={duration - 0.05}:d=0.05",
                 "-c:a", "libmp3lame", "-b:a", "192k", "-ar", "44100", "-ac", "2",
                 "-write_xing", "0", "-id3v2_version", "0", "-map_metadata", "-1", str(mp3)],
                check=True, capture_output=True, timeout=90,
            )
        except BaseException:
            mp3.unlink(missing_ok=True)
            raise
        finally:
            wav.unlink(missing_ok=True)
        return mp3, round(time.monotonic() - started, 2)
