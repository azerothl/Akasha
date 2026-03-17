# STT service for Akasha: POST /stt (body = audio/wav) → JSON {"text": "..."}
# Uses faster-whisper (CPU, int8). Set WHISPER_MODEL=tiny for faster startup.
FROM python:3.11-slim

WORKDIR /app
COPY requirements.txt .
RUN pip install --no-cache-dir -r requirements.txt
COPY server.py .

ENV STT_HOST=0.0.0.0
ENV STT_PORT=8766
ENV WHISPER_MODEL=base
EXPOSE 8766

CMD ["python", "server.py"]
