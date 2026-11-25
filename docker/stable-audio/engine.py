"""The model stays in one worker; speech services keep their own environment."""

import json
import os
import secrets
import subprocess
import time
from pathlib import Path


class Engine:
    def __init__(self, directory: Path):
        self.directory = directory
        self.model_name = os.environ.get("STABLE_AUDIO_MODEL", "small-music")
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
            if torch.cuda.get_device_properties(0).gcnArchName.split(":")[0] == "gfx906":
                from stable_audio_3.models import transformer

                # SAME-L already has a chunked SDPA implementation for GPUs
                # without Flash Attention; avoid its Triton compilation path.
                transformer.flex_attention_available = False
        device = os.environ.get("STABLE_AUDIO_DEVICE", "cuda")
        if self.model_name == "medium" and device.startswith("cuda"):
            self.model = self._load_medium(device)
        else:
            self.model = StableAudioModel.from_pretrained(self.model_name, device=device)

    def _load_medium(self, device):
        import torch
        from safetensors import safe_open
        from stable_audio_3 import StableAudioModel
        from stable_audio_3.factory import create_diffusion_cond_from_config
        from stable_audio_3.loading_utils import remap_state_dict_keys
        from stable_audio_3.model_configs import all_models

        config_path, checkpoint_path = all_models[self.model_name].resolve()
        with open(config_path) as source:
            config = json.load(source)
        # Construct FP16 parameters directly on the GPU: the VM has 4 GB RAM,
        # while Medium's FP32 checkpoint alone is over 9 GB.
        previous_dtype = torch.get_default_dtype()
        try:
            torch.set_default_dtype(torch.float16)
            with torch.device(device):
                model = create_diffusion_cond_from_config(config)
        finally:
            torch.set_default_dtype(previous_dtype)

        # pread avoids reserving the entire file as a writable mmap. Keep
        # only one CPU tensor at a time and retain strict checkpoint checks.
        targets = model.state_dict()
        with safe_open(checkpoint_path, framework="pt", device="cpu", backend="pread") as checkpoint:
            names = remap_state_dict_keys({key: key for key in checkpoint.keys()}, targets)
            if names.keys() != targets.keys():
                raise RuntimeError("Medium checkpoint keys do not match the model")
            with torch.no_grad():
                for name, source_name in names.items():
                    tensor = checkpoint.get_tensor(source_name)
                    if tensor.shape != targets[name].shape:
                        raise RuntimeError(f"Medium checkpoint shape mismatch: {name}")
                    targets[name].copy_(tensor)
                    del tensor
        model.half().eval().requires_grad_(False)
        model.use_lora = False
        model.lora_names = []
        return StableAudioModel(model, config, device, True)

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
