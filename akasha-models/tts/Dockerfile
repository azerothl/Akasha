# TTS service for Akasha: POST /tts {"text": "..."} → audio/wav
# Uses edge-tts (Microsoft) + pydub for WAV. ffmpeg required for pydub.
FROM python:3.11-slim

RUN apt-get update && apt-get install -y --no-install-recommends ffmpeg \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY requirements.txt .
RUN pip install --no-cache-dir -r requirements.txt
COPY server.py .

ENV TTS_HOST=0.0.0.0
ENV TTS_PORT=8765
EXPOSE 8765

CMD ["python", "server.py"]
