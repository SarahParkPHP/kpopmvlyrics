#!/usr/bin/env python3
"""Transcribe audio with faster-whisper and emit word-level timestamps as JSON."""

from __future__ import annotations

import argparse
import json
import os
import sys
import wave
from dataclasses import dataclass
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
        help="CTranslate2 compute type (auto, int8, int8_float16, float16, float32)",
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


def is_large_model(model: str) -> bool:
    lowered = model.lower()
    return "large" in lowered or lowered == "turbo"


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


def resolve_compute_type(model: str, device: str, explicit: str) -> str:
    if explicit and explicit != "auto":
        return explicit
    if device == "cuda":
        return "int8_float16" if is_large_model(model) else "float16"
    return "float32" if is_large_model(model) else "int8"


def resolve_device(args: argparse.Namespace) -> str:
    if args.device != "auto":
        return args.device
    if cuda_runtime_available():
        return "cuda"
    return "cpu"


def cpu_threads() -> int:
    return max(1, min(os.cpu_count() or 4, 16))


def audio_duration_seconds(path: Path) -> float:
    with wave.open(str(path), "rb") as handle:
        rate = handle.getframerate()
        if rate <= 0:
            return 0.0
        return handle.getnframes() / float(rate)


def expected_min_words(duration_seconds: float) -> int:
    if duration_seconds <= 0:
        return 40
    # Music videos can be sparse, but full-length MVs should exceed this easily.
    return max(40, int(round((duration_seconds / 60.0) * 15)))


def is_cuda_runtime_error(error: BaseException) -> bool:
    message = str(error).lower()
    return any(
        token in message
        for token in ("libcublas", "libcurand", "libcudnn", "cuda", "cublas")
    )


@dataclass
class TranscriptionResult:
    segments: list
    info: object
    device: str
    compute_type: str
    language: str | None


def extract_words_and_segments(segments: list) -> tuple[list[dict], list[dict]]:
    words: list[dict] = []
    segment_rows: list[dict] = []
    for segment in segments:
        segment_words: list[dict] = []
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
    return words, segment_rows


def transcribe_audio(
    args: argparse.Namespace,
    device: str,
    compute_type: str,
    language: str | None,
) -> TranscriptionResult:
    from faster_whisper import WhisperModel

    model = WhisperModel(
        args.model,
        device=device,
        compute_type=compute_type,
        cpu_threads=cpu_threads(),
    )

    transcribe_kwargs = {
        "beam_size": 5,
        "word_timestamps": True,
        "vad_filter": resolve_vad_filter(args.vad_filter),
        # Music videos lose most of the track when Whisper conditions on prior text.
        "condition_on_previous_text": False,
        "no_speech_threshold": 0.5,
        "compression_ratio_threshold": 2.8,
    }
    if language:
        transcribe_kwargs["language"] = language

    segments, info = model.transcribe(str(args.audio), **transcribe_kwargs)
    return TranscriptionResult(
        segments=list(segments),
        info=info,
        device=device,
        compute_type=compute_type,
        language=language,
    )


def looks_truncated(word_count: int, duration_seconds: float, model: str) -> bool:
    if not is_large_model(model):
        return False
    return word_count < expected_min_words(duration_seconds)


def run_transcription(args: argparse.Namespace) -> TranscriptionResult:
    duration_seconds = audio_duration_seconds(args.audio)
    language = args.language

    device = resolve_device(args)
    compute_type = resolve_compute_type(args.model, device, args.compute_type)

    try:
        result = transcribe_audio(args, device, compute_type, language)
    except RuntimeError as error:
        if device == "cuda" and is_cuda_runtime_error(error):
            print(
                "CUDA libraries unavailable; retrying Whisper on CPU.",
                file=sys.stderr,
            )
            fallback_compute = resolve_compute_type(args.model, "cpu", "auto")
            result = transcribe_audio(args, "cpu", fallback_compute, language)
        else:
            raise

    words, _ = extract_words_and_segments(result.segments)
    if not looks_truncated(len(words), duration_seconds, args.model):
        return result

    print(
        f"Whisper produced only {len(words)} words for {duration_seconds:.1f}s audio; "
        "retrying large model on CPU float32.",
        file=sys.stderr,
    )
    retry = transcribe_audio(args, "cpu", "float32", language)
    retry_words, _ = extract_words_and_segments(retry.segments)
    if len(retry_words) > len(words):
        return retry

    if language:
        print(
            "Whisper retry still sparse; retrying once more with auto language detection.",
            file=sys.stderr,
        )
        auto_retry = transcribe_audio(args, "cpu", "float32", None)
        auto_words, _ = extract_words_and_segments(auto_retry.segments)
        if len(auto_words) > len(retry_words):
            return auto_retry

    return retry if len(retry_words) >= len(words) else result


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

    result = run_transcription(args)
    words, segment_rows = extract_words_and_segments(result.segments)

    payload = {
        "language": result.info.language,
        "language_probability": result.info.language_probability,
        "model": args.model,
        "device": result.device,
        "compute_type": result.compute_type,
        "words": words,
        "segments": segment_rows,
    }

    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(payload, ensure_ascii=False, indent=2), encoding="utf-8")
    print(
        json.dumps(
            {
                "words": len(words),
                "segments": len(segment_rows),
                "language": result.info.language,
                "device": result.device,
                "compute_type": result.compute_type,
            }
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
