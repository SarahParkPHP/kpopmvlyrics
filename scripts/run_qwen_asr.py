#!/usr/bin/env python3
"""Transcribe and align audio with official Qwen3-ASR (PyTorch + Hugging Face)."""

from __future__ import annotations

import argparse
import base64
import gc
import json
import mimetypes
import os
import re
import statistics
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from urllib import error as urlerror
from urllib import request

# Reduce CUDA memory fragmentation. Must be set before torch is imported anywhere.
os.environ.setdefault("PYTORCH_CUDA_ALLOC_CONF", "expandable_segments:True")

# `python scripts/run_qwen_asr.py` puts scripts/ on sys.path[0], which can shadow
# the installed `qwen_asr` package (especially if stale __pycache__ exists).
_SCRIPT_DIR = str(Path(__file__).resolve().parent)
if sys.path and Path(sys.path[0]).resolve() == Path(_SCRIPT_DIR).resolve():
    sys.path.pop(0)


def free_cuda_memory() -> None:
    """Release Python references and PyTorch's reserved-but-unallocated cache."""
    gc.collect()
    try:
        import torch

        if torch.cuda.is_available():
            torch.cuda.empty_cache()
            torch.cuda.synchronize()
    except ImportError:
        pass


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--audio", required=True, type=Path, help="Input audio file")
    parser.add_argument("--output", required=True, type=Path, help="Output JSON path")
    parser.add_argument(
        "--provider",
        default="local",
        choices=["local", "openai", "elevenlabs", "mistral", "gemini", "soniox", "alibaba"],
        help="ASR provider backend",
    )
    parser.add_argument(
        "--model",
        required=True,
        help="Provider model id",
    )
    parser.add_argument(
        "--aligner-model",
        default="Qwen/Qwen3-ForcedAligner-0.6B",
        help="Hugging Face forced aligner id, e.g. Qwen/Qwen3-ForcedAligner-0.6B",
    )
    parser.add_argument("--api-key", default=None, help="External provider API key")
    parser.add_argument("--base-url", default=None, help="External provider base URL override")
    parser.add_argument(
        "--language",
        default=None,
        help="ASR language hint code, e.g. ko or en",
    )
    parser.add_argument(
        "--align-language",
        default=None,
        help='Forced-aligner language name, e.g. Korean or English',
    )
    parser.add_argument(
        "--lyrics-text",
        default=None,
        help="Known lyric transcript for forced alignment (preferred over ASR text)",
    )
    parser.add_argument(
        "--lyrics-lines-file",
        default=None,
        type=Path,
        help="JSON file with [{index, text, char_start?, char_end?}, ...] for line timings",
    )
    parser.add_argument(
        "--device",
        default="auto",
        choices=["auto", "cpu", "cuda"],
        help="PyTorch device (auto prefers CUDA when available)",
    )
    return parser.parse_args()


ALIGN_LANGUAGE_BY_CODE = {
    "ko": "Korean",
    "en": "English",
    "ja": "Japanese",
    "zh": "Chinese",
    "yue": "Cantonese",
    "fr": "French",
    "de": "German",
    "it": "Italian",
    "pt": "Portuguese",
    "ru": "Russian",
    "es": "Spanish",
}


def resolve_align_language(code: str | None, explicit: str | None) -> str:
    if explicit and explicit.strip():
        return explicit.strip()
    if code:
        normalized = code.strip().lower()
        if normalized in ALIGN_LANGUAGE_BY_CODE:
            return ALIGN_LANGUAGE_BY_CODE[normalized]
        if normalized == "zh-cn":
            return "Chinese"
    return "English"


def resolve_device(requested: str) -> str:
    import torch

    normalized = requested.strip().lower()
    if normalized == "cpu":
        return "cpu"
    if torch.cuda.is_available():
        return "cuda"
    if normalized == "cuda":
        print("CUDA requested but torch.cuda.is_available() is false; using CPU.", file=sys.stderr)
    return "cpu"


def _env_float(name: str, default: float) -> float:
    raw = os.environ.get(name)
    if not raw:
        return default
    try:
        return max(0.0, float(raw))
    except ValueError:
        return default


def vram_budget_gib() -> tuple[float, float]:
    """Return (gpu_budget_gib, free_gib) for the current CUDA device.

    Budget = free VRAM - headroom (activations + CUDA workspace + other apps).
    """
    import torch

    if not torch.cuda.is_available():
        return 0.0, 0.0
    free_bytes, _total_bytes = torch.cuda.mem_get_info(0)
    free_gib = free_bytes / (1024**3)
    # Headroom covers ASR autoregressive KV cache, FA activations, and OS/other-app overhead.
    headroom_gib = _env_float("KPOPMVLYRICS_ASR_VRAM_HEADROOM_GIB", 1.0)
    budget = max(0.0, free_gib - headroom_gib)
    return budget, free_gib


def torch_load_kwargs(device: str, budget_share: float = 1.0) -> dict:
    """Build from_pretrained kwargs that fit the model in available VRAM.

    `budget_share` lets callers reserve a fraction of free VRAM for a sibling model
    (e.g. ASR + aligner loaded together: ASR=0.7, aligner=0.3).
    """
    import torch

    if device != "cuda":
        return {"dtype": torch.float32, "device_map": {"": "cpu"}}

    dtype = torch.bfloat16
    total_budget_gib, free_gib = vram_budget_gib()
    budget_gib = total_budget_gib * max(0.05, min(1.0, budget_share))
    cpu_cap_gib = _env_float("KPOPMVLYRICS_ASR_CPU_BUDGET_GIB", 64.0)

    if budget_gib < 0.5:
        print(
            f"VRAM tight (free={free_gib:.2f} GiB, share={budget_share:.2f} "
            f"→ budget={budget_gib:.2f} GiB); loading on CPU only.",
            file=sys.stderr,
        )
        return {"dtype": torch.float32, "device_map": {"": "cpu"}}

    max_memory = {0: f"{budget_gib:.2f}GiB", "cpu": f"{cpu_cap_gib:.0f}GiB"}
    print(
        f"VRAM budget: GPU={budget_gib:.2f} GiB (free {free_gib:.2f} GiB, "
        f"share={budget_share:.2f}), CPU cap {cpu_cap_gib:.0f} GiB. "
        "Using device_map=auto.",
        file=sys.stderr,
    )
    return {"dtype": dtype, "device_map": "auto", "max_memory": max_memory}


def ms(value: float) -> int:
    return int(round(float(value) * 1000))


def looks_like_metadata(text: str) -> bool:
    lower = text.lower()
    return (
        lower.startswith("english:")
        or lower.startswith("credits")
        or lower.startswith("disclaimer")
    )


@dataclass
class AlignedToken:
    text: str
    start: float
    end: float


@dataclass
class ExternalTranscript:
    language: str | None
    words: list[AlignedToken]
    segments: list[dict]
    raw_text: str
    alignment_source: str


def audio_mime_type(path: Path) -> str:
    return mimetypes.guess_type(path.name)[0] or "audio/mpeg"


def http_json(url: str, headers: dict[str, str], payload: bytes | dict, timeout: int = 600) -> dict:
    if isinstance(payload, dict):
        body = json.dumps(payload).encode("utf-8")
        headers = {**headers, "Content-Type": "application/json"}
    else:
        body = payload
    req = request.Request(url, data=body, headers=headers, method="POST")
    try:
        with request.urlopen(req, timeout=timeout) as resp:
            data = resp.read().decode("utf-8", errors="replace")
    except urlerror.HTTPError as err:
        detail = err.read().decode("utf-8", errors="replace")
        raise RuntimeError(f"{url} failed with HTTP {err.code}: {detail}") from err
    return json.loads(data) if data.strip() else {}


def http_get_json(url: str, headers: dict[str, str], timeout: int = 120) -> dict:
    req = request.Request(url, headers=headers, method="GET")
    try:
        with request.urlopen(req, timeout=timeout) as resp:
            data = resp.read().decode("utf-8", errors="replace")
    except urlerror.HTTPError as err:
        detail = err.read().decode("utf-8", errors="replace")
        raise RuntimeError(f"{url} failed with HTTP {err.code}: {detail}") from err
    return json.loads(data) if data.strip() else {}


def multipart_body(fields: dict[str, str], files: dict[str, Path]) -> tuple[bytes, str]:
    boundary = f"kpopmvlyrics-{os.getpid()}-{int(time.time() * 1000)}"
    chunks: list[bytes] = []
    for name, value in fields.items():
        chunks.extend(
            [
                f"--{boundary}\r\n".encode(),
                f'Content-Disposition: form-data; name="{name}"\r\n\r\n'.encode(),
                str(value).encode("utf-8"),
                b"\r\n",
            ]
        )
    for name, path in files.items():
        mime = audio_mime_type(path)
        chunks.extend(
            [
                f"--{boundary}\r\n".encode(),
                (
                    f'Content-Disposition: form-data; name="{name}"; '
                    f'filename="{path.name}"\r\n'
                ).encode(),
                f"Content-Type: {mime}\r\n\r\n".encode(),
                path.read_bytes(),
                b"\r\n",
            ]
        )
    chunks.append(f"--{boundary}--\r\n".encode())
    return b"".join(chunks), f"multipart/form-data; boundary={boundary}"


def provider_base_url(provider: str, override: str | None) -> str:
    if override and override.strip():
        return override.strip().rstrip("/")
    return {
        "openai": "https://api.openai.com/v1",
        "elevenlabs": "https://api.elevenlabs.io/v1",
        "mistral": "https://api.mistral.ai/v1",
        "gemini": "https://generativelanguage.googleapis.com/v1beta",
        "soniox": "https://api.soniox.com/v1",
        "alibaba": "https://dashscope-intl.aliyuncs.com/api/v1",
    }[provider]


def require_api_key(provider: str, api_key: str | None) -> str:
    if api_key and api_key.strip():
        return api_key.strip()
    env_key = {
        "openai": "OPENAI_API_KEY",
        "elevenlabs": "ELEVENLABS_API_KEY",
        "mistral": "MISTRAL_API_KEY",
        "gemini": "GEMINI_API_KEY",
        "soniox": "SONIOX_API_KEY",
        "alibaba": "DASHSCOPE_API_KEY",
    }[provider]
    value = os.environ.get(env_key)
    if value:
        return value
    raise RuntimeError(f"Missing API key for {provider}. Set it in the app settings.")


def numeric_time(value) -> float | None:
    if value is None:
        return None
    try:
        number = float(value)
    except (TypeError, ValueError):
        return None
    if number > 1000:
        return number / 1000.0
    return number


def dict_text(row: dict) -> str:
    for key in ("text", "word", "token", "content"):
        value = row.get(key)
        if isinstance(value, str) and value.strip():
            return value.strip()
    return ""


def dict_start_end(row: dict) -> tuple[float | None, float | None]:
    start = None
    end = None
    for key in ("start", "start_time", "start_sec", "begin_time", "start_ms", "begin_ms"):
        if key in row:
            start = numeric_time(row.get(key))
            break
    for key in ("end", "end_time", "end_sec", "finish_time", "end_ms", "finish_ms"):
        if key in row:
            end = numeric_time(row.get(key))
            break
    return start, end


def tokens_from_rows(rows: list[dict]) -> list[AlignedToken]:
    tokens = []
    for row in rows:
        if not isinstance(row, dict):
            continue
        if row.get("type") in {"spacing", "audio_event"}:
            continue
        text = dict_text(row)
        start, end = dict_start_end(row)
        if text and start is not None and end is not None and end >= start:
            tokens.append(AlignedToken(text=text, start=start, end=max(end, start + 0.02)))
    return tokens


def segments_from_rows(rows: list[dict]) -> list[dict]:
    segments = []
    for index, row in enumerate(rows):
        if not isinstance(row, dict):
            continue
        text = dict_text(row)
        start, end = dict_start_end(row)
        if text and start is not None and end is not None and end >= start:
            segments.append({"text": text, "start_ms": ms(start), "end_ms": ms(max(end, start + 0.2)), "index": index})
    return segments


def distribute_segment_words(segments: list[dict]) -> list[AlignedToken]:
    tokens: list[AlignedToken] = []
    for segment in segments:
        text = str(segment.get("text") or "").strip()
        if not text:
            continue
        words = re.findall(r"\S+", text)
        if not words:
            continue
        start = float(segment.get("start_ms", 0)) / 1000.0
        end = float(segment.get("end_ms", 0)) / 1000.0
        if end <= start:
            continue
        step = (end - start) / len(words)
        for index, word in enumerate(words):
            word_start = start + step * index
            tokens.append(AlignedToken(word, word_start, word_start + max(0.02, step * 0.85)))
    return tokens


def recursive_collect_timed_rows(obj) -> tuple[list[dict], list[dict]]:
    words: list[dict] = []
    segments: list[dict] = []
    if isinstance(obj, dict):
        text = dict_text(obj)
        start, end = dict_start_end(obj)
        if text and start is not None and end is not None:
            if "words" in obj or "tokens" in obj or "segment" in str(obj.get("type", "")).lower():
                segments.append(obj)
            else:
                words.append(obj)
        for value in obj.values():
            child_words, child_segments = recursive_collect_timed_rows(value)
            words.extend(child_words)
            segments.extend(child_segments)
    elif isinstance(obj, list):
        if obj and all(isinstance(item, dict) for item in obj):
            direct_tokens = tokens_from_rows(obj)
            direct_segments = segments_from_rows(obj)
            if direct_tokens:
                words.extend(obj)
            elif direct_segments:
                segments.extend(obj)
        for value in obj:
            child_words, child_segments = recursive_collect_timed_rows(value)
            words.extend(child_words)
            segments.extend(child_segments)
    return words, segments


def text_from_response(obj) -> str:
    if isinstance(obj, dict):
        for key in ("text", "transcript"):
            value = obj.get(key)
            if isinstance(value, str) and value.strip():
                return value.strip()
        try:
            parts = obj["candidates"][0]["content"]["parts"]
            for part in parts:
                if isinstance(part, dict) and isinstance(part.get("text"), str):
                    return part["text"].strip()
        except (KeyError, IndexError, TypeError):
            pass
    return ""


def parse_json_text(text: str) -> dict:
    cleaned = text.strip()
    fence = re.search(r"```(?:json)?\s*(.*?)```", cleaned, re.DOTALL)
    if fence:
        cleaned = fence.group(1).strip()
    return json.loads(cleaned)


def external_transcript_from_response(provider: str, model: str, response_obj: dict, source: str) -> ExternalTranscript:
    word_rows, segment_rows = recursive_collect_timed_rows(response_obj)
    words = tokens_from_rows(word_rows)
    segments = segments_from_rows(segment_rows)
    if not segments:
        for key in ("segments", "chunks"):
            value = response_obj.get(key)
            if isinstance(value, list):
                segments = segments_from_rows(value)
                if segments:
                    break
    if not words:
        for key in ("words", "tokens"):
            value = response_obj.get(key)
            if isinstance(value, list):
                words = tokens_from_rows(value)
                if words:
                    break
    if not words and segments:
        words = distribute_segment_words(segments)
    language = response_obj.get("language") or response_obj.get("language_code")
    return ExternalTranscript(
        language=language if isinstance(language, str) else None,
        words=words,
        segments=segments,
        raw_text=text_from_response(response_obj),
        alignment_source=source or f"{provider}:{model}",
    )


def transcribe_openai(args: argparse.Namespace) -> ExternalTranscript:
    api_key = require_api_key("openai", args.api_key)
    base = provider_base_url("openai", args.base_url)
    fields = {
        "model": args.model,
        "response_format": "verbose_json",
        "timestamp_granularities[]": "word",
    }
    if args.language:
        fields["language"] = args.language
    if args.lyrics_text:
        fields["prompt"] = args.lyrics_text[:2000]
    body, content_type = multipart_body(fields, {"file": args.audio})
    response_obj = http_json(
        f"{base}/audio/transcriptions",
        {"Authorization": f"Bearer {api_key}", "Content-Type": content_type},
        body,
    )
    return external_transcript_from_response("openai", args.model, response_obj, "openai")


def transcribe_elevenlabs(args: argparse.Namespace) -> ExternalTranscript:
    api_key = require_api_key("elevenlabs", args.api_key)
    base = provider_base_url("elevenlabs", args.base_url)
    fields = {"model_id": args.model}
    if args.language:
        fields["language_code"] = args.language
    body, content_type = multipart_body(fields, {"file": args.audio})
    response_obj = http_json(
        f"{base}/speech-to-text",
        {"xi-api-key": api_key, "Content-Type": content_type},
        body,
    )
    return external_transcript_from_response("elevenlabs", args.model, response_obj, "elevenlabs")


def transcribe_mistral(args: argparse.Namespace) -> ExternalTranscript:
    api_key = require_api_key("mistral", args.api_key)
    base = provider_base_url("mistral", args.base_url)
    fields = {"model": args.model, "timestamp_granularities[]": "segment"}
    body, content_type = multipart_body(fields, {"file": args.audio})
    response_obj = http_json(
        f"{base}/audio/transcriptions",
        {"Authorization": f"Bearer {api_key}", "Content-Type": content_type},
        body,
    )
    return external_transcript_from_response("mistral", args.model, response_obj, "mistral")


def transcribe_gemini(args: argparse.Namespace) -> ExternalTranscript:
    api_key = require_api_key("gemini", args.api_key)
    base = provider_base_url("gemini", args.base_url)
    audio_data = base64.b64encode(args.audio.read_bytes()).decode("ascii")
    prompt = (
        "Transcribe the speech and singing in this audio. Return JSON only with "
        '{"language":"<bcp47>","segments":[{"start_ms":0,"end_ms":0,"text":"..."}]}. '
        "Use millisecond timestamps. Do not include markdown."
    )
    if args.lyrics_text:
        prompt += "\nKnown lyrics/context:\n" + args.lyrics_text[:8000]
    payload = {
        "contents": [
            {
                "parts": [
                    {"text": prompt},
                    {"inline_data": {"mime_type": audio_mime_type(args.audio), "data": audio_data}},
                ]
            }
        ],
        "generationConfig": {"response_mime_type": "application/json"},
    }
    response_obj = http_json(
        f"{base}/models/{args.model}:generateContent?key={api_key}",
        {},
        payload,
    )
    text = text_from_response(response_obj)
    parsed = parse_json_text(text) if text else response_obj
    return external_transcript_from_response("gemini", args.model, parsed, "gemini")


def transcribe_soniox(args: argparse.Namespace) -> ExternalTranscript:
    api_key = require_api_key("soniox", args.api_key)
    base = provider_base_url("soniox", args.base_url)
    body, content_type = multipart_body({}, {"file": args.audio})
    file_obj = http_json(
        f"{base}/files",
        {"Authorization": f"Bearer {api_key}", "Content-Type": content_type},
        body,
    )
    file_id = file_obj.get("id") or file_obj.get("file_id")
    if not file_id:
        raise RuntimeError(f"Soniox upload response did not include a file id: {file_obj}")
    payload = {
        "model": args.model,
        "file_id": file_id,
        "enable_language_identification": True,
    }
    if args.language:
        payload["language_hints"] = [args.language]
    job = http_json(f"{base}/transcriptions", {"Authorization": f"Bearer {api_key}"}, payload)
    transcription_id = job.get("id") or job.get("transcription_id")
    if not transcription_id:
        raise RuntimeError(f"Soniox transcription response did not include an id: {job}")
    status_obj = job
    deadline = time.time() + 900
    while time.time() < deadline:
        status = str(status_obj.get("status", "")).lower()
        if status in {"completed", "complete", "succeeded", "success"}:
            break
        if status in {"failed", "error"}:
            raise RuntimeError(status_obj.get("error_message") or f"Soniox transcription failed: {status_obj}")
        time.sleep(2.0)
        status_obj = http_get_json(f"{base}/transcriptions/{transcription_id}", {"Authorization": f"Bearer {api_key}"})
    else:
        raise RuntimeError("Soniox transcription timed out")
    transcript = http_get_json(
        f"{base}/transcriptions/{transcription_id}/transcript",
        {"Authorization": f"Bearer {api_key}"},
    )
    return external_transcript_from_response("soniox", args.model, transcript, "soniox")


def transcribe_alibaba(args: argparse.Namespace) -> ExternalTranscript:
    api_key = require_api_key("alibaba", args.api_key)
    base = provider_base_url("alibaba", args.base_url)
    audio_data = base64.b64encode(args.audio.read_bytes()).decode("ascii")
    messages = []
    if args.lyrics_text:
        messages.append({"role": "system", "content": [{"text": args.lyrics_text[:10000]}]})
    messages.append(
        {
            "role": "user",
            "content": [{"audio": f"data:{audio_mime_type(args.audio)};base64,{audio_data}"}],
        }
    )
    asr_options = {"enable_itn": False}
    if args.language:
        asr_options["language"] = args.language
    payload = {
        "model": args.model,
        "input": {"messages": messages},
        "parameters": {"asr_options": asr_options},
    }
    response_obj = http_json(
        f"{base}/services/aigc/multimodal-generation/generation",
        {"Authorization": f"Bearer {api_key}"},
        payload,
    )
    return external_transcript_from_response("alibaba", args.model, response_obj, "alibaba-qwen-asr")


def transcribe_external(args: argparse.Namespace) -> ExternalTranscript:
    return {
        "openai": transcribe_openai,
        "elevenlabs": transcribe_elevenlabs,
        "mistral": transcribe_mistral,
        "gemini": transcribe_gemini,
        "soniox": transcribe_soniox,
        "alibaba": transcribe_alibaba,
    }[args.provider](args)


_memory_hooks_installed = False


def _install_memory_clearing_hooks(Qwen3ASRModel, Qwen3ForcedAligner) -> None:
    """Hook into qwen-asr to release CUDA memory between ASR and FA phases.

    The Qwen3-ASR model's `transcribe()` calls `_infer_asr` (autoregressive
    generation) then immediately invokes the forced aligner on each chunk's
    transcript. With both models GPU-resident, the FA's forward pass needs
    ~400 MiB of activations - but PyTorch's reserved-but-unallocated cache
    holds onto similar amounts from the ASR pass, causing OOM on 6 GiB GPUs.
    Clearing cache between phases fixes this.
    """
    global _memory_hooks_installed
    if _memory_hooks_installed:
        return

    original_infer_asr = Qwen3ASRModel._infer_asr

    def patched_infer_asr(self, *args, **kwargs):
        result = original_infer_asr(self, *args, **kwargs)
        free_cuda_memory()
        return result

    Qwen3ASRModel._infer_asr = patched_infer_asr

    original_align = Qwen3ForcedAligner.align

    def patched_align(self, *args, **kwargs):
        free_cuda_memory()
        return original_align(self, *args, **kwargs)

    Qwen3ForcedAligner.align = patched_align

    _memory_hooks_installed = True


def import_qwen_asr():
    try:
        from qwen_asr import Qwen3ASRModel, Qwen3ForcedAligner
    except ImportError:
        from qwen_asr.inference.qwen3_asr_model import Qwen3ASRModel  # type: ignore
        from qwen_asr.inference.qwen3_forced_aligner import Qwen3ForcedAligner  # type: ignore
    _install_memory_clearing_hooks(Qwen3ASRModel, Qwen3ForcedAligner)
    return Qwen3ASRModel, Qwen3ForcedAligner


def anchor_words_in_text(align_text: str, aligned: list[AlignedToken]) -> list[tuple[int, int, int]]:
    anchors: list[tuple[int, int, int]] = []
    pos = 0
    for index, word in enumerate(aligned):
        token = word.text.strip()
        if not token:
            continue
        found = align_text.find(token, pos)
        if found < 0:
            found = pos
        end = found + len(token)
        anchors.append((index, found, end))
        pos = end
    return anchors


def robust_chunk_timing(chunk: list[AlignedToken]) -> tuple[int | None, int | None]:
    if not chunk:
        return None, None
    if len(chunk) == 1:
        word = chunk[0]
        start_ms = ms(word.start)
        end_ms = ms(max(word.end, word.start))
        return start_ms, max(end_ms, start_ms + 200)

    keep: list[AlignedToken] = []
    for index, word in enumerate(chunk):
        local_times = [word.start, word.end]
        if index > 0:
            local_times.extend([chunk[index - 1].start, chunk[index - 1].end])
        if index + 1 < len(chunk):
            local_times.extend([chunk[index + 1].start, chunk[index + 1].end])
        local_median = statistics.median(local_times)
        if abs(word.start - local_median) <= 20.0 and abs(word.end - local_median) <= 20.0:
            keep.append(word)

    if len(keep) < max(1, len(chunk) // 3):
        middle = chunk[1:-1] if len(chunk) > 2 else chunk
        anchor = statistics.median([word.start for word in middle] + [word.end for word in middle])
        keep = [
            word
            for word in chunk
            if abs(word.start - anchor) <= 20.0 and abs(word.end - anchor) <= 20.0
        ]

    if not keep:
        keep = [chunk[len(chunk) // 2]]

    start_ms = ms(min(word.start for word in keep))
    end_ms = ms(max(word.end for word in keep))
    if end_ms < start_ms:
        end_ms = start_ms + 200
    return start_ms, max(end_ms, start_ms + 200)


def map_aligned_words_to_lines(
    line_specs: list[dict],
    align_text: str,
    aligned: list[AlignedToken],
) -> list[dict]:
    anchors = anchor_words_in_text(align_text, aligned)
    line_timings: list[dict] = []

    for spec in line_specs:
        lyric_index = int(spec["index"])
        text = str(spec.get("text", "")).strip()
        if not text or looks_like_metadata(text):
            line_timings.append({"lyric_index": lyric_index, "start_ms": None, "end_ms": None})
            continue

        if "char_start" in spec and "char_end" in spec:
            char_start = int(spec["char_start"])
            char_end = int(spec["char_end"])
        else:
            char_start = align_text.find(text)
            if char_start < 0:
                line_timings.append({"lyric_index": lyric_index, "start_ms": None, "end_ms": None})
                continue
            char_end = char_start + len(text)

        chunk = [
            aligned[word_index]
            for word_index, start, end in anchors
            if start < char_end and end > char_start
        ]
        if not chunk:
            line_timings.append({"lyric_index": lyric_index, "start_ms": None, "end_ms": None})
            continue

        start_ms, end_ms = robust_chunk_timing(chunk)
        if start_ms is None or end_ms is None:
            line_timings.append({"lyric_index": lyric_index, "start_ms": None, "end_ms": None})
            continue

        line_timings.append(
            {"lyric_index": lyric_index, "start_ms": start_ms, "end_ms": end_ms}
        )

    synced = [row for row in line_timings if row.get("start_ms") is not None]
    synced.sort(key=lambda row: row["start_ms"])
    for index, row in enumerate(synced[:-1]):
        next_start = synced[index + 1]["start_ms"]
        if row["end_ms"] >= next_start:
            row["end_ms"] = max(row["start_ms"] + 200, next_start - 1)

    return line_timings


def line_timings_quality(line_timings: list[dict], lyric_count: int) -> bool:
    synced = [row for row in line_timings if row.get("start_ms") is not None]
    if len(synced) < max(4, lyric_count // 4):
        return False

    distinct_starts = len({row["start_ms"] for row in synced})
    if distinct_starts < max(4, len(synced) // 4):
        return False

    first = min(synced, key=lambda row: row["lyric_index"])
    if first["end_ms"] - first["start_ms"] > 25_000:
        return False

    max_span = max(row["end_ms"] - row["start_ms"] for row in synced)
    return max_span <= 45_000


def map_lines_sequential(line_specs: list[dict], aligned: list[AlignedToken]) -> list[dict]:
    """Map lines by consuming aligned tokens in order (matches official FA token stream)."""
    word_index = 0
    line_timings: list[dict] = []

    for spec in line_specs:
        lyric_index = int(spec["index"])
        text = str(spec.get("text", "")).strip()
        if not text or looks_like_metadata(text):
            line_timings.append({"lyric_index": lyric_index, "start_ms": None, "end_ms": None})
            continue

        line_norm = "".join(ch for ch in text if not ch.isspace())
        chunk: list[AlignedToken] = []
        consumed = 0
        while word_index < len(aligned) and consumed < len(line_norm):
            word = aligned[word_index]
            word_norm = "".join(ch for ch in word.text if not ch.isspace())
            if not word_norm:
                word_index += 1
                continue
            chunk.append(word)
            consumed += len(word_norm)
            word_index += 1
            if consumed >= len(line_norm):
                break

        if not chunk:
            line_timings.append({"lyric_index": lyric_index, "start_ms": None, "end_ms": None})
            continue

        start_ms = ms(min(word.start for word in chunk))
        end_ms = ms(max(word.end for word in chunk))
        line_timings.append(
            {
                "lyric_index": lyric_index,
                "start_ms": start_ms,
                "end_ms": max(end_ms, start_ms + 200),
            }
        )

    synced = [row for row in line_timings if row.get("start_ms") is not None]
    synced.sort(key=lambda row: row["start_ms"])
    for index, row in enumerate(synced[:-1]):
        next_start = synced[index + 1]["start_ms"]
        if row["end_ms"] >= next_start:
            row["end_ms"] = max(row["start_ms"] + 200, next_start - 1)

    return line_timings


def choose_line_timings(
    line_specs: list[dict],
    align_text: str,
    aligned: list[AlignedToken],
) -> list[dict]:
    if not line_specs:
        return []

    sequential = map_lines_sequential(line_specs, aligned)
    if line_timings_quality(sequential, len(line_specs)):
        return sequential

    precise = map_aligned_words_to_lines(line_specs, align_text, aligned)
    if line_timings_quality(precise, len(line_specs)):
        return precise

    return sequential if any(row.get("start_ms") is not None for row in sequential) else []


def repair_zero_word_timestamps(tokens: list[AlignedToken]) -> list[AlignedToken]:
    """Repair or drop tokens with broken timestamps from FA failures.

    Strategy:
      - **Drop** a large leading run of near-zero-timestamp tokens followed
        by a big gap. These represent ASR-recognised words the FA couldn't
        time at all - inventing timestamps for them propagates wrong timing
        through the whole song. Better to leave those lyric lines unsynced
        in the script's output; Rust's caption baseline merge then fills
        them in at approximately-right times.
      - **Redistribute** a small leading cluster (<=4 tokens) backward
        from the first real anchor, since this is usually just the FA
        being uncertain about the first word or two.

    Returns the cleaned tokens list (may be shorter than input).
    """
    if not tokens:
        return tokens

    # Find the first token with a credible START timestamp. End-only signals are
    # unreliable on this FA output (the FA can emit token with start=0, end=0.04
    # right before the real run starts).
    anchor_index = next(
        (i for i, t in enumerate(tokens) if t.start > 0.05),
        None,
    )

    leading_drop_threshold = 5
    if anchor_index is not None and anchor_index >= leading_drop_threshold:
        anchor_time = tokens[anchor_index].start
        # A jump >= 5s after a long zero run means the FA failed completely;
        # drop the leading zeros so Rust falls back to caption timings.
        if anchor_time >= 5.0:
            print(
                f"dropping {anchor_index} leading zero-timestamp tokens "
                f"(FA failed; anchor at {anchor_time:.2f}s)",
                file=sys.stderr,
            )
            tokens = tokens[anchor_index:]
            anchor_index = 0

    # Small leading cluster: redistribute backward from the anchor.
    if anchor_index is not None and 0 < anchor_index < leading_drop_threshold:
        anchor = tokens[anchor_index].start
        if anchor > 0:
            step = min(0.25, anchor / max(anchor_index + 1, 1))
            for i in range(anchor_index):
                start = max(0.0, anchor - (anchor_index - i) * step)
                tokens[i].start = start
                tokens[i].end = min(anchor, start + max(step, 0.08))

    # Forward-fill: any remaining exact zeros in the middle inherit the previous start.
    last = 0.0
    for token in tokens:
        if token.start > 0.05:
            last = token.start
        elif last > 0:
            token.start = last
            token.end = max(token.end, last + 0.05)

    return tokens


def dedupe_clustered_line_timings(line_timings: list[dict], max_share: int = 3) -> None:
    """Drop line rows when too many lines share the same start (chunk boundary artifact)."""
    from collections import Counter

    synced = [row for row in line_timings if row.get("start_ms") is not None]
    counts = Counter(row["start_ms"] for row in synced)
    for row in line_timings:
        start = row.get("start_ms")
        if start is None:
            continue
        if counts[start] > max_share:
            row["start_ms"] = None
            row["end_ms"] = None


def filter_hallucinated_tokens(
    tokens: list[AlignedToken],
    lyrics_text: str,
) -> list[AlignedToken]:
    """Drop tokens whose text doesn't appear in the known lyrics.

    The Qwen ASR sometimes emits English filler ("puncture", "bitchy") during
    instrumental tails when biased by mixed-language lyrics. These hallucinated
    tokens have plausible timestamps but their words aren't in the song, so
    fuzzy DP alignment of CCL lines to them is misleading. Keep only tokens
    that overlap (case-insensitive substring or trigram) with the lyrics.
    """
    if not lyrics_text or not tokens:
        return tokens
    norm_lyrics = "".join(c.lower() for c in lyrics_text if not c.isspace())
    if not norm_lyrics:
        return tokens
    trigrams = {norm_lyrics[i : i + 3] for i in range(len(norm_lyrics) - 2)}

    kept: list[AlignedToken] = []
    dropped = 0
    for token in tokens:
        norm = "".join(c.lower() for c in token.text if not c.isspace())
        if not norm:
            kept.append(token)
            continue
        if norm in norm_lyrics:
            kept.append(token)
            continue
        if len(norm) >= 3 and any(norm[i : i + 3] in trigrams for i in range(len(norm) - 2)):
            kept.append(token)
            continue
        if len(norm) <= 2 and norm in norm_lyrics:
            kept.append(token)
            continue
        dropped += 1
    if dropped:
        print(
            f"filtered {dropped} hallucinated tokens not present in lyrics",
            file=sys.stderr,
        )
    return kept


def forced_items_to_tokens(items) -> list[AlignedToken]:
    tokens: list[AlignedToken] = []
    for item in items:
        text = str(getattr(item, "text", "") or "").strip()
        if not text:
            continue
        tokens.append(
            AlignedToken(
                text=text,
                start=float(getattr(item, "start_time", 0.0)),
                end=float(getattr(item, "end_time", getattr(item, "start_time", 0.0))),
            )
        )
    return tokens


def line_align_language(text: str, fallback: str) -> str:
    hangul = sum(1 for ch in text if "\uac00" <= ch <= "\ud7af")
    kana = sum(1 for ch in text if "\u3040" <= ch <= "\u30ff")
    han = sum(1 for ch in text if "\u4e00" <= ch <= "\u9fff" or "\u3400" <= ch <= "\u4dbf")
    latin = sum(1 for ch in text if ch.isascii() and ch.isalpha())
    counts = [
        (hangul, "Korean"),
        (kana, "Japanese"),
        (han, "Chinese"),
        (latin, "English"),
    ]
    counts.sort(key=lambda pair: pair[0], reverse=True)
    top_count, top_lang = counts[0]
    if top_count == 0:
        return fallback
    return top_lang


def split_line_specs(line_specs: list[dict], chunk_count: int) -> list[list[dict]]:
    if chunk_count <= 1 or not line_specs:
        return [line_specs]
    groups: list[list[dict]] = [[] for _ in range(chunk_count)]
    for index, spec in enumerate(line_specs):
        groups[min(index * chunk_count // len(line_specs), chunk_count - 1)].append(spec)
    return [group for group in groups if group]


def allocate_lines_to_chunks(
    line_specs: list[dict],
    audio_chunks: list,
) -> list[list[dict]]:
    """Assign each lyric line to the audio chunk whose window contains its hint_start_ms.

    Falls back to count-proportional split for lines without hints, or when no hints exist.
    `audio_chunks` is the list returned by split_audio_into_chunks: [(wav, offset_sec), ...].
    """
    if not audio_chunks:
        return []
    if len(audio_chunks) == 1:
        return [list(line_specs)]

    has_any_hint = any(spec.get("hint_start_ms") is not None for spec in line_specs)
    if not has_any_hint:
        return split_line_specs(line_specs, len(audio_chunks))

    chunk_bounds: list[tuple[float, float]] = []
    for idx, (chunk_wav, offset_sec) in enumerate(audio_chunks):
        from qwen_asr.inference.utils import SAMPLE_RATE
        duration = len(chunk_wav) / float(SAMPLE_RATE)
        start = offset_sec
        end = offset_sec + duration if idx < len(audio_chunks) - 1 else float("inf")
        chunk_bounds.append((start, end))

    groups: list[list[dict]] = [[] for _ in audio_chunks]
    last_chunk_idx = 0
    for spec in line_specs:
        hint = spec.get("hint_start_ms")
        if hint is None:
            groups[last_chunk_idx].append(spec)
            continue
        hint_sec = hint / 1000.0
        target = last_chunk_idx
        for idx, (start, end) in enumerate(chunk_bounds):
            if start <= hint_sec < end:
                target = idx
                break
        last_chunk_idx = target
        groups[target].append(spec)
    return groups


def chunk_output_is_degenerate(tokens: list[AlignedToken], chunk_duration_sec: float) -> bool:
    """Return True if a chunk's forced-alignment output is unreliable garbage.

    Qwen ForcedAligner emits all-zero timestamps or collapsed clusters when the input
    lyrics don't match the chunk audio, or when the input exceeds the model's
    reliable window. Reject those.
    """
    if not tokens:
        return True
    zero_count = sum(1 for t in tokens if t.start == 0 and t.end == 0)
    if zero_count > len(tokens) * 0.5:
        return True
    distinct_starts = len({round(t.start, 1) for t in tokens})
    if distinct_starts < max(3, len(tokens) // 4):
        return True
    spread = max(t.end for t in tokens) - min(t.start for t in tokens)
    if spread > chunk_duration_sec * 2.5:
        return True
    return False


def offset_line_timings(line_timings: list[dict], offset_sec: float) -> None:
    offset_ms = ms(offset_sec)
    if offset_ms == 0:
        return
    for row in line_timings:
        if row.get("start_ms") is not None:
            row["start_ms"] += offset_ms
        if row.get("end_ms") is not None:
            row["end_ms"] += offset_ms


def load_forced_aligner(aligner_model: str, device: str):
    _, Qwen3ForcedAligner = import_qwen_asr()
    load_kwargs = torch_load_kwargs(device)
    return Qwen3ForcedAligner.from_pretrained(aligner_model, **load_kwargs)


def align_lyrics_chunked(
    aligner,
    audio_path: Path,
    line_specs: list[dict],
    align_language: str,
) -> tuple[list[AlignedToken], list[dict]]:
    from qwen_asr.inference.utils import (
        SAMPLE_RATE,
        normalize_audios,
        split_audio_into_chunks,
    )

    chunk_target_sec = _env_float("KPOPMVLYRICS_ASR_FA_CHUNK_SEC", 30.0)
    wav = normalize_audios(str(audio_path))[0]
    audio_duration_sec = len(wav) / float(SAMPLE_RATE)
    max_ms = ms(audio_duration_sec) + 500
    audio_chunks = split_audio_into_chunks(
        wav,
        SAMPLE_RATE,
        chunk_target_sec,
    )
    line_groups = allocate_lines_to_chunks(line_specs, audio_chunks)

    all_tokens: list[AlignedToken] = []
    all_line_timings: list[dict] = []
    accepted_chunks = 0

    for chunk_index, ((chunk_wav, offset_sec), group) in enumerate(
        zip(audio_chunks, line_groups)
    ):
        if not group:
            continue
        chunk_text = " ".join(str(spec.get("text", "")).strip() for spec in group if spec.get("text"))
        if not chunk_text.strip():
            continue
        chunk_duration_sec = len(chunk_wav) / float(SAMPLE_RATE)
        print(
            f"  chunk {chunk_index + 1}/{len(audio_chunks)}: "
            f"{len(group)} lines, offset={offset_sec:.1f}s, duration={chunk_duration_sec:.1f}s",
            file=sys.stderr,
        )
        chunk_language = line_align_language(chunk_text, align_language)
        results = aligner.align(
            audio=(chunk_wav, SAMPLE_RATE),
            text=chunk_text,
            language=chunk_language,
        )
        if not results:
            print(f"    skip: aligner returned no results", file=sys.stderr)
            continue
        tokens = forced_items_to_tokens(results[0])
        if chunk_output_is_degenerate(tokens, chunk_duration_sec):
            print(
                f"    skip: degenerate FA output "
                f"(tokens={len(tokens)}, "
                f"zeros={sum(1 for t in tokens if t.start == 0 and t.end == 0)}, "
                f"distinct={len({round(t.start, 1) for t in tokens})})",
                file=sys.stderr,
            )
            continue
        tokens = repair_zero_word_timestamps(tokens)
        chunk_timings = choose_line_timings(group, chunk_text, tokens)
        offset_line_timings(chunk_timings, offset_sec)
        for token in tokens:
            token.start += offset_sec
            token.end += offset_sec
        for row in chunk_timings:
            if row.get("start_ms") is not None and row["start_ms"] > max_ms:
                row["start_ms"] = None
                row["end_ms"] = None
        all_tokens.extend(tokens)
        all_line_timings.extend(chunk_timings)
        accepted_chunks += 1

        import torch

        if torch.cuda.is_available():
            torch.cuda.empty_cache()

    print(
        f"  accepted {accepted_chunks}/{len(audio_chunks)} chunks, "
        f"{sum(1 for r in all_line_timings if r.get('start_ms') is not None)} of "
        f"{len(line_specs)} lines aligned",
        file=sys.stderr,
    )

    dedupe_clustered_line_timings(all_line_timings)

    synced = [row for row in all_line_timings if row.get("start_ms") is not None]
    synced.sort(key=lambda row: row["start_ms"])
    for index, row in enumerate(synced[:-1]):
        next_start = synced[index + 1]["start_ms"]
        if row["end_ms"] >= next_start:
            row["end_ms"] = max(row["start_ms"] + 200, next_start - 1)

    return all_tokens, all_line_timings


def align_with_forced_aligner(
    aligner,
    audio_path: Path,
    align_text: str,
    align_language: str,
) -> list[AlignedToken]:
    results = aligner.align(
        audio=str(audio_path),
        text=align_text,
        language=line_align_language(align_text, align_language),
    )
    if not results:
        return []
    return forced_items_to_tokens(results[0])


_SMALL_FALLBACK_MODEL = "Qwen/Qwen3-ASR-0.6B"


def _is_cuda_oom(err: Exception) -> bool:
    if "out of memory" in str(err).lower():
        return True
    try:
        import torch

        return isinstance(err, torch.cuda.OutOfMemoryError)
    except (ImportError, AttributeError):
        return False


def _release_model(model) -> None:
    """Drop a loaded model and force GPU memory back to the pool."""
    try:
        del model
    except Exception:
        pass
    free_cuda_memory()


def transcribe_with_asr_model(
    model_id: str,
    aligner_model: str,
    audio_path: Path,
    language: str | None,
    device: str,
    context: str = "",
):
    """Run Qwen3-ASR transcription with optional lyrics context for biasing.

    Passing the known CCL/Genius lyrics as `context` makes the model recognize
    those lyrics in the audio rather than free-form transcribing — output text
    closely mirrors the input lyrics, and the FA pass produces accurate
    word-level timestamps spread across the song's actual sung timeline.

    If the chosen model OOMs on this GPU, falls back to Qwen3-ASR-0.6B and
    retries automatically.
    """
    Qwen3ASRModel, _Qwen3ForcedAligner = import_qwen_asr()
    language_arg = [language] if language else None
    attempted_models: list[str] = []

    def _try_with(target_model_id: str):
        attempted_models.append(target_model_id)
        # ASR (1.7B/0.6B) is larger than the aligner (0.6B); split the VRAM budget ~7:3.
        asr_kwargs = torch_load_kwargs(device, budget_share=0.7)
        aligner_kwargs = torch_load_kwargs(device, budget_share=0.3)
        model = Qwen3ASRModel.from_pretrained(
            target_model_id,
            forced_aligner=aligner_model,
            forced_aligner_kwargs=aligner_kwargs,
            max_inference_batch_size=1,
            max_new_tokens=512,
            **asr_kwargs,
        )
        print(
            f"ASR transcribe ({target_model_id}): "
            f"context_chars={len(context)}, language={language_arg}",
            file=sys.stderr,
        )
        try:
            results = model.transcribe(
                audio=str(audio_path),
                context=context,
                language=language_arg,
                return_time_stamps=True,
            )
            return results[0] if results else None
        finally:
            _release_model(model)

    try:
        return _try_with(model_id)
    except Exception as err:
        if not _is_cuda_oom(err):
            raise
        if model_id == _SMALL_FALLBACK_MODEL:
            raise
        free_cuda_memory()
        print(
            f"ASR OOM on {model_id}; retrying with {_SMALL_FALLBACK_MODEL}",
            file=sys.stderr,
        )
        return _try_with(_SMALL_FALLBACK_MODEL)


def main() -> int:
    args = parse_args()
    if not args.audio.exists():
        print(f"Audio file not found: {args.audio}", file=sys.stderr)
        return 1

    language_code = args.language.strip() if args.language else None
    align_language = resolve_align_language(language_code, args.align_language)
    align_text = args.lyrics_text.strip() if args.lyrics_text else ""

    if args.provider != "local":
        try:
            transcript = transcribe_external(args)
        except Exception as err:
            import traceback

            print(f"{args.provider} ASR failed: {err}", file=sys.stderr)
            traceback.print_exc()
            return 1

        words_out = [
            {
                "text": token.text,
                "start_ms": ms(token.start),
                "end_ms": ms(token.end),
            }
            for token in transcript.words
            if token.text.strip()
        ]
        payload = {
            "language": transcript.language or language_code,
            "model": args.model,
            "backend": transcript.alignment_source,
            "device": "api",
            "align_language": align_language,
            "words": words_out,
            "segments": transcript.segments,
            "alignment_source": transcript.alignment_source if words_out else "none",
            "align_word_count": len(words_out),
            "line_timings": [],
            "text": transcript.raw_text,
        }
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(payload, ensure_ascii=False, indent=2), encoding="utf-8")
        print(
            json.dumps(
                {
                    "backend": payload["backend"],
                    "model": args.model,
                    "words": len(words_out),
                    "segments": len(transcript.segments),
                    "language": payload["language"],
                },
                ensure_ascii=False,
            ),
            file=sys.stderr,
        )
        return 0

    try:
        import_qwen_asr()
    except ImportError as err:
        print(
            f"qwen-asr import failed ({err}). Run ./scripts/setup-asr.sh from the project root.",
            file=sys.stderr,
        )
        return 1

    runtime_device = resolve_device(args.device)

    line_specs: list[dict] = []
    if args.lyrics_lines_file and args.lyrics_lines_file.exists():
        parsed = json.loads(args.lyrics_lines_file.read_text(encoding="utf-8"))
        if isinstance(parsed, list):
            line_specs = parsed

    segments_out: list[dict] = []
    detected_language = language_code
    aligned_tokens: list[AlignedToken] = []
    line_timings: list[dict] = []

    alignment_source = "none"
    try:
        if align_text:
            # Primary path: ASR with the known lyrics as `context` bias.
            # The model transcribes what it actually hears (biased toward the lyrics)
            # and the FA pass times the transcript - giving accurate word-level
            # timestamps spread across the song's true sung timeline. The CCL→ASR
            # word mapping is handled in Rust via fuzzy DP (align_lyrics_to_timed_words),
            # which tolerates the small text differences from ASR mis-recognitions.
            print(
                f"Transcribing with ASR + lyrics context ({len(align_text)} chars)...",
                file=sys.stderr,
            )
            result = transcribe_with_asr_model(
                args.model,
                args.aligner_model,
                args.audio,
                align_language,
                runtime_device,
                context=align_text,
            )
            if result is not None:
                detected_language = getattr(result, "language", None) or language_code
                # We intentionally do NOT push a "full-text" segment into segments_out:
                # the Rust side would then build a single caption_line of all the ASR text
                # at 0-200ms, overwriting the multi-line YouTube captions. Letting
                # segments stay empty makes asr_caption_lines fall through to word-chunked
                # captions, which carry real timing.
                stamps = getattr(result, "time_stamps", None)
                if stamps is not None:
                    aligned_tokens = forced_items_to_tokens(stamps)
                    aligned_tokens = filter_hallucinated_tokens(aligned_tokens, align_text)
                    aligned_tokens = repair_zero_word_timestamps(aligned_tokens)
                if aligned_tokens:
                    alignment_source = "asr-context"
                    print(
                        f"ASR+context produced {len(aligned_tokens)} timestamped tokens",
                        file=sys.stderr,
                    )

            # Fallback to chunked forced alignment if ASR-with-context yielded nothing
            # (e.g. the model OOM'd or the audio was unrecognisable).
            if not aligned_tokens and line_specs:
                print(
                    "ASR+context produced no tokens; falling back to chunked FA",
                    file=sys.stderr,
                )
                aligner = load_forced_aligner(args.aligner_model, runtime_device)
                aligned_tokens, line_timings = align_lyrics_chunked(
                    aligner,
                    args.audio,
                    line_specs,
                    align_language,
                )
                if aligned_tokens:
                    alignment_source = "lyrics-chunked"
        else:
            # No known lyrics — pure ASR transcription.
            result = transcribe_with_asr_model(
                args.model,
                args.aligner_model,
                args.audio,
                align_language,
                runtime_device,
            )
            if result is not None:
                detected_language = getattr(result, "language", None) or language_code
                # We intentionally do NOT push a "full-text" segment into segments_out:
                # the Rust side would then build a single caption_line of all the ASR text
                # at 0-200ms, overwriting the multi-line YouTube captions. Letting
                # segments stay empty makes asr_caption_lines fall through to word-chunked
                # captions, which carry real timing.
                stamps = getattr(result, "time_stamps", None)
                if stamps is not None:
                    aligned_tokens = forced_items_to_tokens(stamps)
                    aligned_tokens = repair_zero_word_timestamps(aligned_tokens)
                if aligned_tokens:
                    alignment_source = "asr"
    except Exception as err:
        import traceback

        print(f"Qwen3 ASR alignment failed: {err}", file=sys.stderr)
        traceback.print_exc()
        return 1

    words_out = [
        {
            "text": token.text,
            "start_ms": ms(token.start),
            "end_ms": ms(token.end),
        }
        for token in aligned_tokens
        if token.text.strip()
    ]

    line_timings = sorted(line_timings, key=lambda row: row.get("lyric_index", 0))

    payload = {
        "language": detected_language,
        "model": args.model,
        "backend": "qwen-asr",
        "device": runtime_device,
        "align_language": align_language,
        "words": words_out,
        "segments": segments_out,
        "alignment_source": alignment_source if words_out else "none",
        "align_word_count": len(words_out),
        "line_timings": line_timings,
    }

    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(payload, ensure_ascii=False, indent=2), encoding="utf-8")
    print(
        json.dumps(
            {
                "words": len(words_out),
                "segments": len(segments_out),
                "language": detected_language,
                "alignment_source": payload["alignment_source"],
                "device": runtime_device,
            }
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
