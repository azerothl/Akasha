"""
TTS HTTP server for Akasha voice_router.
POST /tts with JSON {"text": "..."} → response body = raw WAV bytes (16 kHz mono PCM).
Uses edge-tts (MP3) then ffmpeg or PyAV for WAV.
"""
import io
import os
import shutil
import subprocess
import tempfile
import wave
from contextlib import asynccontextmanager
from pathlib import Path

from fastapi import FastAPI, HTTPException
from fastapi.responses import Response
from pydantic import BaseModel
import edge_tts
import uvicorn

HOST = os.environ.get("TTS_HOST", "0.0.0.0")
PORT = int(os.environ.get("TTS_PORT", "8765"))
VOICE = os.environ.get("TTS_VOICE", "fr-FR-DeniseNeural")
# Soft headroom for small speakers / amp (0.0–1.0). Applied after decode.
TTS_GAIN = float(os.environ.get("TTS_GAIN", "0.85"))


class TTSRequest(BaseModel):
    text: str


@asynccontextmanager
async def lifespan(app: FastAPI):
    yield


app = FastAPI(title="Akasha TTS", lifespan=lifespan)


def _find_ffmpeg() -> str | None:
    env = os.environ.get("FFMPEG_PATH")
    if env and Path(env).is_file():
        return env
    which = shutil.which("ffmpeg")
    if which:
        return which
    for c in (
        r"C:\Program Files\Tracktion\Waveform 13\ffmpeg.exe",
        r"C:\ffmpeg\bin\ffmpeg.exe",
    ):
        if Path(c).is_file():
            return c
    return None


def _apply_gain_wav(wav_bytes: bytes, gain: float) -> bytes:
    if gain >= 0.999:
        return wav_bytes
    bio = io.BytesIO(wav_bytes)
    with wave.open(bio, "rb") as w:
        params = w.getparams()
        raw = w.readframes(w.getnframes())
    import array

    samples = array.array("h")
    samples.frombytes(raw)
    g = max(0.05, min(gain, 1.0))
    for i, s in enumerate(samples):
        v = int(s * g)
        if v > 32767:
            v = 32767
        elif v < -32768:
            v = -32768
        samples[i] = v
    out = io.BytesIO()
    with wave.open(out, "wb") as w:
        w.setparams(params)
        w.writeframes(samples.tobytes())
    return out.getvalue()


def _mp3_to_wav_ffmpeg(mp3_path: str, ffmpeg: str) -> bytes:
    fd, wav_path = tempfile.mkstemp(suffix=".wav")
    os.close(fd)
    try:
        cmd = [
            ffmpeg,
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
            mp3_path,
            "-ac",
            "1",
            "-ar",
            "16000",
            "-sample_fmt",
            "s16",
            wav_path,
        ]
        subprocess.run(cmd, check=True, capture_output=True)
        with open(wav_path, "rb") as f:
            return f.read()
    finally:
        try:
            os.unlink(wav_path)
        except OSError:
            pass


def _mp3_to_wav_pyav(mp3_path: str) -> bytes:
    """Fallback decode: float planar → mono s16 @ 16 kHz via AudioFifo."""
    import av
    import numpy as np

    container = av.open(mp3_path)
    stream = container.streams.audio[0]
    resampler = av.audio.resampler.AudioResampler(
        format="s16", layout="mono", rate=16000
    )
    fifo = av.AudioFifo()
    for frame in container.decode(stream):
        for out in resampler.resample(frame):
            fifo.write(out)
    for out in resampler.resample(None):
        fifo.write(out)
    container.close()

    chunks: list[bytes] = []
    while fifo.samples >= 1024 or (fifo.samples > 0 and not chunks):
        frame = fifo.read(min(fifo.samples, 4096))
        if frame is None:
            break
        # Prefer plane bytes for s16 mono (avoids ndarray layout ambiguity)
        if frame.planes:
            chunks.append(bytes(frame.planes[0])[: frame.samples * 2])
        else:
            arr = frame.to_ndarray()
            if arr.ndim == 2:
                arr = arr.reshape(-1)
            chunks.append(np.ascontiguousarray(arr, dtype=np.int16).tobytes())

    pcm = b"".join(chunks)
    buf = io.BytesIO()
    with wave.open(buf, "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(16000)
        w.writeframes(pcm)
    return buf.getvalue()


def _mp3_to_wav_bytes(mp3_path: str) -> bytes:
    ffmpeg = _find_ffmpeg()
    if ffmpeg:
        return _mp3_to_wav_ffmpeg(mp3_path, ffmpeg)
    return _mp3_to_wav_pyav(mp3_path)


@app.post("/tts", response_class=Response)
async def tts(request: TTSRequest):
    text = (request.text or "").strip()
    if not text:
        raise HTTPException(status_code=400, detail="text is required")
    tmp_mp3 = None
    try:
        communicate = edge_tts.Communicate(text, VOICE)
        fd, tmp_mp3 = tempfile.mkstemp(suffix=".mp3")
        os.close(fd)
        await communicate.save(tmp_mp3)
        wav_bytes = _mp3_to_wav_bytes(tmp_mp3)
        wav_bytes = _apply_gain_wav(wav_bytes, TTS_GAIN)
        return Response(content=wav_bytes, media_type="audio/wav")
    except Exception as e:
        raise HTTPException(status_code=500, detail=str(e))
    finally:
        if tmp_mp3 and os.path.exists(tmp_mp3):
            try:
                os.unlink(tmp_mp3)
            except OSError:
                pass


@app.get("/health")
async def health():
    return {
        "status": "ok",
        "ffmpeg": bool(_find_ffmpeg()),
        "gain": TTS_GAIN,
    }


if __name__ == "__main__":
    uvicorn.run(app, host=HOST, port=PORT)
