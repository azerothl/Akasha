"""
TTS HTTP server for Akasha voice_router.
POST /tts with JSON {"text": "..."} → response body = raw WAV bytes.
Uses edge-tts (Microsoft Edge TTS, free) + pydub for WAV output.
"""
import io
import tempfile
import os
from contextlib import asynccontextmanager

from fastapi import FastAPI, HTTPException
from fastapi.responses import Response
from pydantic import BaseModel
import edge_tts
import uvicorn

# Optional: pydub for mp3 → wav (edge_tts outputs mp3)
try:
    from pydub import AudioSegment
    HAS_PYDUB = True
except ImportError:
    HAS_PYDUB = False

HOST = os.environ.get("TTS_HOST", "0.0.0.0")
PORT = int(os.environ.get("TTS_PORT", "8765"))
# Edge TTS voice (e.g. fr-FR-DeniseNeural for French, en-US-JennyNeural for English)
VOICE = os.environ.get("TTS_VOICE", "fr-FR-DeniseNeural")


class TTSRequest(BaseModel):
    text: str


@asynccontextmanager
async def lifespan(app: FastAPI):
    yield
    # cleanup if needed


app = FastAPI(title="Akasha TTS", lifespan=lifespan)


@app.post("/tts", response_class=Response)
async def tts(request: TTSRequest):
    text = (request.text or "").strip()
    if not text:
        raise HTTPException(status_code=400, detail="text is required")
    try:
        communicate = edge_tts.Communicate(text, VOICE)
        with tempfile.NamedTemporaryFile(suffix=".mp3", delete=False) as f:
            tmp_mp3 = f.name
        try:
            await communicate.save(tmp_mp3)
            if HAS_PYDUB:
                audio = AudioSegment.from_mp3(tmp_mp3)
                buf = io.BytesIO()
                audio.export(buf, format="wav")
                buf.seek(0)
                return Response(
                    content=buf.read(),
                    media_type="audio/wav",
                )
            # Fallback: return mp3 (daemon expects WAV; may still work for some clients)
            with open(tmp_mp3, "rb") as f:
                return Response(
                    content=f.read(),
                    media_type="audio/mpeg",
                )
        finally:
            if os.path.exists(tmp_mp3):
                os.unlink(tmp_mp3)
    except Exception as e:
        raise HTTPException(status_code=500, detail=str(e))


@app.get("/health")
async def health():
    return {"status": "ok"}


if __name__ == "__main__":
    uvicorn.run(app, host=HOST, port=PORT)
