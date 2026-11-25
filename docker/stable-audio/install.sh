#!/bin/sh
set -eu

# Reuse a working ROCm PyTorch installation without changing any of its files.
# Usage: sh install.sh /opt/qwen-venv/bin/python
speech_python=${1:?Pass the Python executable of the existing ROCm environment}
source_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
audio_python=/opt/stable-audio-venv/bin/python
if [ ! -x "$audio_python" ]; then python3 -m venv /opt/stable-audio-venv; fi
speech_site=$("$speech_python" -c 'import sysconfig; print(sysconfig.get_path("purelib"))')
audio_site=$("$audio_python" -c 'import sysconfig; print(sysconfig.get_path("purelib"))')
printf '%s\n' "$speech_site" > "$audio_site/quasar-rocm.pth"
"$audio_python" -m pip install -r "$source_dir/requirements.txt"
"$audio_python" -m pip install --no-deps 'git+https://github.com/Stability-AI/stable-audio-3.git@790793b89ddceea612be9ce42ff6830d18f6fa57'
command -v ffmpeg >/dev/null
install -d /opt/quasar-stable-audio
install -m 644 "$source_dir/engine.py" "$source_dir/server.py" /opt/quasar-stable-audio/
install -m 644 "$source_dir/quasar-stable-audio.service" /etc/systemd/system/
# /etc/quasar-stable-audio.env supplies ALICE_API_URL and STREAM_URL (ending in
# /stream), and ALICE_API_TOKEN if Quasar requires it. Model access is granted
# separately through Hugging Face; this installer never accepts model terms.
# Set STABLE_AUDIO_MODEL=medium there to use Medium instead of small-music.
systemctl daemon-reload
