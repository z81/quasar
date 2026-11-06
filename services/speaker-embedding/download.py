"""Build-time model download: no weights or network needed at runtime."""
from huggingface_hub import snapshot_download

snapshot_download(
    repo_id="speechbrain/spkrec-ecapa-voxceleb",
    revision="0f99f2d0ebe89ac095bcc5903c4dd8f72b367286",
    local_dir="/opt/model",
    allow_patterns=["hyperparams.yaml", "embedding_model.ckpt", "mean_var_norm_emb.ckpt", "classifier.ckpt", "label_encoder.txt"],
)
