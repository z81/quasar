FROM python:3.11-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ffmpeg curl \
    && rm -rf /var/lib/apt/lists/*

RUN pip install --no-cache-dir numpy==1.26.4 tflite-runtime==2.14.0

WORKDIR /app
RUN curl -fsSL -o yamnet.tflite https://storage.googleapis.com/audioset/yamnet.tflite \
    && curl -fsSL -o yamnet_class_map.csv https://raw.githubusercontent.com/tensorflow/models/master/research/audioset/yamnet/yamnet_class_map.csv

COPY app.py .

EXPOSE 8080
CMD ["python", "app.py"]
