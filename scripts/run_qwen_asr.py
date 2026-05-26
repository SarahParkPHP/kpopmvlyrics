#!/usr/bin/env python3
"""Transcribe and align audio with official Qwen3-ASR (PyTorch + Hugging Face)."""

from __future__ import annotations

import argparse
import json
import statistics
import sys
from dataclasses import dataclass
from pathlib import Path

# `python scripts/run_qwen_asr.py` puts scripts/ on sys.path[0], which can shadow
# the installed `qwen_asr` package (especially if stale __pycache__ exists).
_SCRIPT_DIR = str(Path(__file__).resolve().parent)
if sys.path and Path(sys.path[0]).resolve() == Path(_SCRIPT_DIR).resolve():
    sys.path.pop(0)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--audio", required=True, type=Path, help="Input audio file")
    parser.add_argument("--output", required=True, type=Path, help="Output JSON path")
    parser.add_argument(
        "--model",
        required=True,
        help="Hugging Face model id, e.g. Qwen/Qwen3-ASR-0.6B",
    )
    parser.add_argument(
        "--aligner-model",
        required=True,
        help="Hugging Face forced aligner id, e.g. Qwen/Qwen3-ForcedAligner-0.6B",
    )
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


def torch_load_kwargs(device: str) -> dict:
    import torch

    dtype = torch.bfloat16 if device == "cuda" else torch.float32
    if device == "cuda":
        return {"dtype": dtype, "device_map": "cuda:0"}
    return {"dtype": dtype, "device_map": {"": "cpu"}}


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


def import_qwen_asr():
    try:
        from qwen_asr import Qwen3ASRModel, Qwen3ForcedAligner
    except ImportError:
        from qwen_asr.inference.qwen3_asr_model import Qwen3ASRModel  # type: ignore
        from qwen_asr.inference.qwen3_forced_aligner import Qwen3ForcedAligner  # type: ignore
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


def repair_zero_word_timestamps(tokens: list[AlignedToken]) -> None:
    """Qwen bulk FA often leaves a run of 0.0s tokens before the first real timestamp."""
    if not tokens:
        return

    anchor_index = next(
        (i for i, token in enumerate(tokens) if token.start > 0.05 or token.end > 0.05),
        None,
    )
    if anchor_index is not None and anchor_index > 0:
        anchor = (
            tokens[anchor_index].start
            if tokens[anchor_index].start > 0.05
            else tokens[anchor_index].end
        )
        if anchor > 0:
            step = min(0.25, anchor / max(anchor_index + 1, 1))
            for i in range(anchor_index):
                start = max(0.0, anchor - (anchor_index - i) * step)
                tokens[i].start = start
                tokens[i].end = min(anchor, start + max(step, 0.08))

    last = 0.0
    for token in tokens:
        if token.start > 0.05:
            last = token.start
        elif last > 0:
            token.start = last
            token.end = max(token.end, last + 0.05)


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
    latin = sum(1 for ch in text if ch.isascii() and ch.isalpha())
    if hangul > latin:
        return "Korean"
    if latin > hangul:
        return "English"
    return fallback


def split_line_specs(line_specs: list[dict], chunk_count: int) -> list[list[dict]]:
    if chunk_count <= 1 or not line_specs:
        return [line_specs]
    groups: list[list[dict]] = [[] for _ in range(chunk_count)]
    for index, spec in enumerate(line_specs):
        groups[min(index * chunk_count // len(line_specs), chunk_count - 1)].append(spec)
    return [group for group in groups if group]


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
        MAX_FORCE_ALIGN_INPUT_SECONDS,
        SAMPLE_RATE,
        normalize_audios,
        split_audio_into_chunks,
    )

    wav = normalize_audios(str(audio_path))[0]
    audio_duration_sec = len(wav) / float(SAMPLE_RATE)
    max_ms = ms(audio_duration_sec) + 500
    audio_chunks = split_audio_into_chunks(
        wav,
        SAMPLE_RATE,
        MAX_FORCE_ALIGN_INPUT_SECONDS,
    )
    line_groups = split_line_specs(line_specs, len(audio_chunks))

    all_tokens: list[AlignedToken] = []
    all_line_timings: list[dict] = []

    for chunk_index, ((chunk_wav, offset_sec), group) in enumerate(
        zip(audio_chunks, line_groups)
    ):
        chunk_text = " ".join(str(spec.get("text", "")).strip() for spec in group if spec.get("text"))
        if not chunk_text.strip():
            continue
        print(
            f"  chunk {chunk_index + 1}/{len(audio_chunks)}: "
            f"{len(group)} lines, offset={offset_sec:.1f}s",
            file=sys.stderr,
        )
        chunk_language = line_align_language(chunk_text, align_language)
        results = aligner.align(
            audio=(chunk_wav, SAMPLE_RATE),
            text=chunk_text,
            language=chunk_language,
        )
        if not results:
            continue
        tokens = forced_items_to_tokens(results[0])
        repair_zero_word_timestamps(tokens)
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

        import torch

        if torch.cuda.is_available():
            torch.cuda.empty_cache()

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


def transcribe_with_asr_model(
    model_id: str,
    aligner_model: str,
    audio_path: Path,
    language: str | None,
    device: str,
):
    Qwen3ASRModel, _Qwen3ForcedAligner = import_qwen_asr()
    load_kwargs = torch_load_kwargs(device)
    model = Qwen3ASRModel.from_pretrained(
        model_id,
        forced_aligner=aligner_model,
        forced_aligner_kwargs=load_kwargs,
        max_inference_batch_size=1,
        max_new_tokens=512,
        **load_kwargs,
    )
    language_arg = [language] if language else None
    results = model.transcribe(
        audio=str(audio_path),
        language=language_arg,
        return_time_stamps=True,
    )
    return results[0] if results else None


def main() -> int:
    args = parse_args()
    if not args.audio.exists():
        print(f"Audio file not found: {args.audio}", file=sys.stderr)
        return 1

    try:
        import_qwen_asr()
    except ImportError as err:
        print(
            f"qwen-asr import failed ({err}). Run ./scripts/setup-asr.sh from the project root.",
            file=sys.stderr,
        )
        return 1

    runtime_device = resolve_device(args.device)
    language_code = args.language.strip() if args.language else None
    align_language = resolve_align_language(language_code, args.align_language)
    align_text = args.lyrics_text.strip() if args.lyrics_text else ""

    line_specs: list[dict] = []
    if args.lyrics_lines_file and args.lyrics_lines_file.exists():
        parsed = json.loads(args.lyrics_lines_file.read_text(encoding="utf-8"))
        if isinstance(parsed, list):
            line_specs = parsed

    segments_out: list[dict] = []
    detected_language = language_code
    aligned_tokens: list[AlignedToken] = []
    line_timings: list[dict] = []

    try:
        if align_text and line_specs:
            aligner = load_forced_aligner(args.aligner_model, runtime_device)
            print(
                f"Aligning {len(line_specs)} lyric lines in chunked mode (<=180s audio per chunk)...",
                file=sys.stderr,
            )
            aligned_tokens, line_timings = align_lyrics_chunked(
                aligner,
                args.audio,
                line_specs,
                align_language,
            )
            alignment_source = "lyrics-chunked"
        elif align_text:
            aligner = load_forced_aligner(args.aligner_model, runtime_device)
            aligned_tokens = align_with_forced_aligner(
                aligner,
                args.audio,
                align_text,
                align_language,
            )
            alignment_source = "lyrics"
            if line_specs and aligned_tokens:
                line_timings = choose_line_timings(line_specs, align_text, aligned_tokens)
        else:
            result = transcribe_with_asr_model(
                args.model,
                args.aligner_model,
                args.audio,
                language_code,
                runtime_device,
            )
            alignment_source = "asr"
            if result is not None:
                detected_language = getattr(result, "language", None) or language_code
                text = str(getattr(result, "text", "") or "").strip()
                if text:
                    segments_out.append(
                        {
                            "text": text,
                            "start_ms": 0,
                            "end_ms": 0,
                            "words": [],
                        }
                    )
                stamps = getattr(result, "time_stamps", None)
                if stamps is not None:
                    aligned_tokens = forced_items_to_tokens(stamps)
                    align_text = " ".join(token.text for token in aligned_tokens)
                if line_specs and aligned_tokens:
                    line_timings = choose_line_timings(line_specs, align_text, aligned_tokens)
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
