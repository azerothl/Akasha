"""
STT HTTP server for Akasha voice_router.
POST /stt with body = raw audio (audio/wav or other), Content-Type: audio/wav.
Response: JSON {"text": "..."}.
Uses faster-whisper (lightweight Whisper).
"""
import io
import os
import tempfile
from contextlib import asynccontextmanager

from fastapi import FastAPI, HTTPException, Request
from fastapi.responses import JSONResponse

HOST = os.environ.get("STT_HOST", "0.0.0.0")
PORT = int(os.environ.get("STT_PORT", "8766"))
# Model: tiny, base, small, medium, large-v2, etc.
WHISPER_MODEL = os.environ.get("WHISPER_MODEL", "base")


@asynccontextmanager
async def lifespan(app: FastAPI):
    # Load model at startup
    from faster_whisper import WhisperModel
    app.state.model = WhisperModel(WHISPER_MODEL, device="cpu", compute_type="int8")
    yield
    # cleanup


app = FastAPI(title="Akasha STT", lifespan=lifespan)


@app.post("/stt")
async def stt(request: Request):
    body = await request.body()
    if not body:
        return JSONResponse(content={"text": ""}, status_code=200)
    try:
        with tempfile.NamedTemporaryFile(suffix=".wav", delete=False) as f:
            f.write(body)
            tmp_path = f.name
        try:
            model = request.app.state.model
            segments, info = model.transcribe(tmp_path, language=None, beam_size=1)
            text = " ".join(s.text for s in segments).strip()
            return {"text": text}
        finally:
            if os.path.exists(tmp_path):
                os.unlink(tmp_path)
    except Exception as e:
        raise HTTPException(status_code=500, detail=str(e))


@app.get("/health")
async def health():
    return {"status": "ok"}


if __name__ == "__main__":
    import uvicorn
    uvicorn.run(app, host=HOST, port=PORT)
