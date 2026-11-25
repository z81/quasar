#!/bin/sh
set -eu

# GPU VM bootstrap. Reuses the existing speech Python environment; no pip changes.
source_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
asr_root=/opt/quasar-asr
runtime_revision=cfa237c2551a4b2d3a56ff8fe5461e3c67c51772
headers_revision=d277a0a3cda9c682708c93279f0c72cd6b0ae3f3
model_revision=3efd5b39882ceadca5570a8fb9fa1a2c801d6d6d
model_name=gigaam-v3-e2e-rnnt-F32.gguf
model_sha256=0de6fa626b8a3429dc80e4b34c4b03c0e85b02e2eca4c654efbeb27a8a0a4c5b

/opt/qwen-venv/bin/python -c 'import numpy, requests, webrtcvad, websockets'
apt-get install -y --no-install-recommends build-essential cmake git \
    libvulkan-dev glslc libopenblas-dev curl ca-certificates
install -d "$asr_root/models"

checkout() {
    repository=$1
    destination=$2
    revision=$3
    if [ ! -d "$destination/.git" ]; then
        git init "$destination"
        git -C "$destination" remote add origin "$repository"
    fi
    git -C "$destination" fetch --depth 1 origin "$revision"
    git -C "$destination" checkout --detach "$revision"
}

checkout https://github.com/handy-computer/transcribe.cpp.git \
    "$asr_root/transcribe.cpp" "$runtime_revision"
checkout https://github.com/KhronosGroup/Vulkan-Headers.git \
    "$asr_root/vulkan-headers" "$headers_revision"
cmake -S "$asr_root/transcribe.cpp" -B "$asr_root/transcribe.cpp/build" \
    -DCMAKE_BUILD_TYPE=Release -DTRANSCRIBE_VULKAN=ON \
    -DTRANSCRIBE_BUILD_SHARED=ON -DTRANSCRIBE_BUILD_TESTS=OFF \
    -DTRANSCRIBE_BUILD_EXAMPLES=OFF \
    -DVulkan_INCLUDE_DIR="$asr_root/vulkan-headers/include"
cmake --build "$asr_root/transcribe.cpp/build" --parallel 2

model_path="$asr_root/models/$model_name"
if [ ! -f "$model_path" ]; then
    curl --fail --location --retry 3 \
        "https://huggingface.co/handy-computer/gigaam-v3-e2e-rnnt-gguf/resolve/$model_revision/$model_name" \
        --output "$model_path.partial"
    printf '%s  %s\n' "$model_sha256" "$model_path.partial" | sha256sum --check
    mv "$model_path.partial" "$model_path"
else
    printf '%s  %s\n' "$model_sha256" "$model_path" | sha256sum --check
fi

backup_dir="/root/quasar-backups/parallel-asr-$(date -u +%Y%m%dT%H%M%SZ)"
install -d -m 700 "$backup_dir"
for file in /root/whisper_gigastt.py /opt/quasar-asr/gateway.py /opt/quasar-asr/gigaam_server.py \
    /etc/systemd/system/whisper-gigastt.service /etc/systemd/system/gigaam-stt.service \
    /etc/systemd/system/gigaam-rnnt-stt.service; do
    if [ -f "$file" ]; then cp -p "$file" "$backup_dir/"; fi
done
install -m 644 "$source_dir/gigaam_server.py" "$asr_root/"
install -m 644 "$source_dir/gateway.py" /root/whisper_gigastt.py
install -m 644 "$source_dir/gateway.py" "$asr_root/gateway.py"
install -m 644 "$source_dir/gigaam-stt.service" "$source_dir/whisper-gigastt.service" /etc/systemd/system/
systemctl daemon-reload
systemctl enable gigaam-stt whisper-gigastt
# Start the E2E worker and check /health before restarting the live gateway.
# Disable obsolete whisper-stt and gigaam-rnnt-stt once the gateway is verified.
