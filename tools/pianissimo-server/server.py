#!/usr/bin/env python3
# /// script
# requires-python = ">=3.10"
# dependencies = ["onnx-asr[cpu,hub]>=0.9", "onnxruntime!=1.24.1"]
# ///
"""Minimal OpenAI-compatible transcription server for Klang Pianissimo (Swedish).

Exposes `POST /v1/audio/transcriptions` (multipart: `file`, optional `response_format`)
backed by `KlangAI/pianissimo-sv-onnx` via onnx-asr. Long recordings are split at
pauses with Silero VAD, so the 20–30 s single-pass limit does not apply.

Run:  uv run server.py --port 8178            (first start downloads ~660 MB + VAD)
Env:  PIANISSIMO_MODEL (default KlangAI/pianissimo-sv-onnx), PIANISSIMO_QUANT (int8)
"""
from __future__ import annotations

import argparse
import json
import logging
import os
import sys
import tempfile
import time
from email.parser import BytesParser
from email.policy import HTTP
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import onnx_asr

LOG = logging.getLogger("pianissimo-server")
MODEL_NAME = os.environ.get("PIANISSIMO_MODEL", "KlangAI/pianissimo-sv-onnx")
QUANT = os.environ.get("PIANISSIMO_QUANT", "int8")


class Engine:
    """Loads the model once; `transcribe` is safe to call from several threads."""

    def __init__(self) -> None:
        t0 = time.time()
        base = onnx_asr.load_model(MODEL_NAME, quantization=QUANT)
        vad = onnx_asr.load_vad("silero")
        # Split at pauses; cap segments well under the model's local-attention window.
        self.segmented = base.with_vad(vad, max_speech_duration_s=20, min_silence_duration_ms=300)
        LOG.info("loaded %s (%s) in %.1fs", MODEL_NAME, QUANT, time.time() - t0)

    def transcribe(self, wav_path: str) -> dict:
        t0 = time.time()
        segments = []
        for seg in self.segmented.recognize(wav_path, channel="mean"):
            text = seg.text.strip()
            if text:
                segments.append({"id": len(segments), "start": float(seg.start), "end": float(seg.end), "text": text})
        text = " ".join(s["text"] for s in segments)
        LOG.info("transcribed %d segment(s), %d chars in %.2fs", len(segments), len(text), time.time() - t0)
        return {"task": "transcribe", "language": "sv", "duration": segments[-1]["end"] if segments else 0.0, "text": text, "segments": segments}


def parse_multipart(headers, body: bytes) -> dict[str, bytes | str]:
    """Return a dict of form fields; file parts come back as bytes, text parts as str."""
    content_type = headers.get("Content-Type", "")
    raw = b"Content-Type: " + content_type.encode() + b"\r\nMIME-Version: 1.0\r\n\r\n" + body
    msg = BytesParser(policy=HTTP).parsebytes(raw)
    fields: dict[str, bytes | str] = {}
    for part in msg.iter_parts():
        name = part.get_param("name", header="content-disposition")
        if not name:
            continue
        payload = part.get_payload(decode=True) or b""
        fields[name] = payload if part.get_filename() else payload.decode("utf-8", "replace")
    return fields


def make_handler(engine: Engine):
    class Handler(BaseHTTPRequestHandler):
        server_version = "pianissimo-server/0.1"

        def log_message(self, fmt, *args):  # route http.server noise through logging
            LOG.debug("%s " + fmt, self.address_string(), *args)

        def _json(self, code: int, payload: dict) -> None:
            data = json.dumps(payload, ensure_ascii=False).encode()
            self.send_response(code)
            self.send_header("Content-Type", "application/json; charset=utf-8")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        def do_GET(self):
            if self.path in ("/", "/health"):
                self._json(200, {"status": "ok", "model": MODEL_NAME, "quantization": QUANT})
            elif self.path == "/v1/models":
                self._json(200, {"object": "list", "data": [{"id": MODEL_NAME, "object": "model"}]})
            else:
                self._json(404, {"error": {"message": "not found"}})

        def do_POST(self):
            if self.path.rstrip("/") != "/v1/audio/transcriptions":
                return self._json(404, {"error": {"message": "not found"}})
            length = int(self.headers.get("Content-Length", 0))
            body = self.rfile.read(length)
            try:
                fields = parse_multipart(self.headers, body)
            except Exception as err:  # noqa: BLE001
                return self._json(400, {"error": {"message": f"bad multipart body: {err}"}})
            audio = fields.get("file")
            if not isinstance(audio, bytes) or not audio:
                return self._json(400, {"error": {"message": "multipart field `file` is required"}})
            fmt = str(fields.get("response_format", "json"))
            with tempfile.NamedTemporaryFile(suffix=".wav", delete=True) as tmp:
                tmp.write(audio)
                tmp.flush()
                try:
                    result = engine.transcribe(tmp.name)
                except Exception as err:  # noqa: BLE001
                    LOG.exception("transcription failed")
                    return self._json(500, {"error": {"message": f"transcription failed: {err}"}})
            if fmt == "text":
                data = result["text"].encode()
                self.send_response(200)
                self.send_header("Content-Type", "text/plain; charset=utf-8")
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)
            elif fmt == "verbose_json":
                self._json(200, result)
            else:
                self._json(200, {"text": result["text"]})

    return Handler


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--port", type=int, default=8178)
    ap.add_argument("--log-level", default=os.environ.get("PIANISSIMO_LOG", "INFO"))
    args = ap.parse_args()
    logging.basicConfig(level=args.log_level.upper(), format="%(asctime)s %(levelname)s %(name)s: %(message)s", stream=sys.stderr)
    engine = Engine()
    httpd = ThreadingHTTPServer((args.host, args.port), make_handler(engine))
    LOG.info("listening on http://%s:%d", args.host, args.port)
    try:
        httpd.serve_forever()
    except KeyboardInterrupt:
        pass
    return 0


if __name__ == "__main__":
    sys.exit(main())
