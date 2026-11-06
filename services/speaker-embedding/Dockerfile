FROM python:3.11-slim-bookworm
RUN apt-get update && apt-get install -y --no-install-recommends ffmpeg libsndfile1 && rm -rf /var/lib/apt/lists/*
# Official matching CPU wheels: https://pytorch.org/get-started/previous-versions/#v260
RUN pip install --no-cache-dir torch==2.6.0 torchaudio==2.6.0 --index-url https://download.pytorch.org/whl/cpu
COPY requirements.txt /opt/speaker/requirements.txt
RUN pip install --no-cache-dir -r /opt/speaker/requirements.txt
COPY download.py /opt/speaker/download.py
RUN python /opt/speaker/download.py && chmod -R a+rX /opt/model
COPY server.py /opt/speaker/server.py
ENV HF_HUB_OFFLINE=1 HF_HUB_DISABLE_TELEMETRY=1 OMP_NUM_THREADS=2 OPENBLAS_NUM_THREADS=1
USER 65534:65534
EXPOSE 8091
HEALTHCHECK --interval=30s --timeout=3s --start-period=30s CMD python -c "import urllib.request; urllib.request.urlopen('http://127.0.0.1:8091/health', timeout=2)"
CMD ["python", "/opt/speaker/server.py"]
