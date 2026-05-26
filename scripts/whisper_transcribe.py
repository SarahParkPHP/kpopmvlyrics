#!/usr/bin/env python3
"""Transcribe audio with faster-whisper and emit word-level timestamps as JSON."""

from __future__ import annotations

import argparse
import json
import os
import sys
from pathlib import Path


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--audio", required=True, type=Path, help="Input audio file")
    parser.add_argument("--output", required=True, type=Path, help="Output JSON path")
    parser.add_argument("--language", default=None, help="Language code, e.g. en or ko")
    parser.add_argument(
        "--model",
        default="small",
        help="Whisper model size (tiny, base, small, medium, large-v3, distil-large-v3)",
    )
    parser.add_argument(
        "--device",
        default="auto",
        choices=["auto", "cpu", "cuda"],
        help="Inference device",
    )
    parser.add_argument(
        "--compute-type",
        default="auto",
        help="CTranslate2 compute type (auto, int8, float16, float32)",
    )
    parser.add_argument(
        "--vad-filter",
        action=argparse.BooleanOptionalAction,
        default=None,
        help="Use Silero VAD before transcription (default: off for music videos)",
    )
    return parser.parse_args()


def resolve_vad_filter(explicit: bool | None) -> bool:
    if explicit is not None:
        return explicit
    env = os.environ.get("KPOPMVLYRICS_WHISPER_VAD", "0").strip().lower()
    return env in {"1", "true", "yes", "on"}


def cuda_runtime_available() -> bool:
    try:
        import ctypes

        import ctranslate2

        if ctranslate2.get_cuda_device_count() == 0:
            return False
        # ctranslate2 wheels are built against CUDA 12.x.
        ctypes.CDLL("libcublas.so.12")
        return True
    except Exception:
        pass
    return False


def resolve_device(device: str) -> tuple[str, str]:
    if device != "auto":
        compute = "float16" if device == "cuda" else "int8"
        return device, compute
    if cuda_runtime_available():
        return "cuda", "float16"
    return "cpu", "int8"


def is_cuda_runtime_error(error: BaseException) -> bool:
    message = str(error).lower()
    return any(
        token in message
        for token in ("libcublas", "libcurand", "libcudnn", "cuda", "cublas")
    )


def transcribe_audio(
    args: argparse.Namespace,
    device: str,
    compute_type: str,
) -> tuple[list, object, str, str]:
    from faster_whisper import WhisperModel

    model = WhisperModel(args.model, device=device, compute_type=compute_type)

    transcribe_kwargs = {
        "beam_size": 5,
        "word_timestamps": True,
        "vad_filter": resolve_vad_filter(args.vad_filter),
    }
    if args.language:
        transcribe_kwargs["language"] = args.language

    segments, info = model.transcribe(str(args.audio), **transcribe_kwargs)
    return list(segments), info, device, compute_type


def main() -> int:
    args = parse_args()
    if not args.audio.exists():
        print(f"Audio file not found: {args.audio}", file=sys.stderr)
        return 1

    try:
        from faster_whisper import WhisperModel  # noqa: F401
    except ImportError:
        print(
            "faster-whisper is not installed. Run ./scripts/setup-whisper.sh from the project root.",
            file=sys.stderr,
        )
        return 1

    device, default_compute = resolve_device(args.device)
    compute_type = args.compute_type
    if compute_type == "auto":
        compute_type = default_compute

    try:
        segments, info, device, compute_type = transcribe_audio(args, device, compute_type)
    except RuntimeError as error:
        if device == "cuda" and is_cuda_runtime_error(error):
            print(
                "CUDA libraries unavailable; retrying Whisper on CPU.",
                file=sys.stderr,
            )
            segments, info, device, compute_type = transcribe_audio(args, "cpu", "int8")
        else:
            raise

    words = []
    segment_rows = []
    for segment in segments:
        segment_words = []
        if segment.words:
            for word in segment.words:
                if not word.word or not word.word.strip():
                    continue
                entry = {
                    "text": word.word.strip(),
                    "start_ms": int(round(word.start * 1000)),
                    "end_ms": int(round(word.end * 1000)),
                }
                words.append(entry)
                segment_words.append(entry)
        segment_rows.append(
            {
                "text": segment.text.strip(),
                "start_ms": int(round(segment.start * 1000)),
                "end_ms": int(round(segment.end * 1000)),
                "words": segment_words,
            }
        )

    payload = {
        "language": info.language,
        "language_probability": info.language_probability,
        "model": args.model,
        "device": device,
        "words": words,
        "segments": segment_rows,
    }

    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(payload, ensure_ascii=False, indent=2), encoding="utf-8")
    print(json.dumps({"words": len(words), "segments": len(segment_rows), "language": info.language}))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
