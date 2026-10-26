#!/bin/sh
# Downloads the voice into the data volume unless it is there already, then
# serves it on 0.0.0.0:5000. The download goes to a temporary directory first,
# so an interrupted one never leaves a broken model behind.
set -e
voice="${PIPER_VOICE:-ru_RU-irina-medium}"
data="${PIPER_DATA_DIR:-/data}"
if [ ! -s "$data/$voice.onnx" ] || [ ! -s "$data/$voice.onnx.json" ]; then
    echo "downloading the Piper voice $voice into $data"
    rm -rf "$data/.download"
    python3 -m piper.download_voices --data-dir "$data/.download" "$voice"
    mv "$data/.download/$voice.onnx" "$data/.download/$voice.onnx.json" "$data/"
    rm -rf "$data/.download"
fi
exec python3 -m piper.http_server -m "$voice" --data-dir "$data" --host 0.0.0.0 --port 5000
