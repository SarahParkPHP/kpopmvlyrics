use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;
use rayon::prelude::*;
use strsim::jaro_winkler;

use crate::models::{AlignmentLine, CaptionLine, LyricLine};

const MATCH_THRESHOLD: f64 = 0.72;
const WHISPER_MATCH_THRESHOLD: f64 = 0.78;
const WHISPER_MIN_TOKEN_RECALL: f64 = 0.52;
const WHISPER_MAX_WORD_SPAN: usize = 48;
const WHISPER_MIN_INTERPOLATED_LINE_MS: i64 = 900;

pub struct AlignmentInput {
    pub lyrics: Vec<LyricLine>,
    pub captions: Vec<CaptionLine>,
}

pub fn align_lines(input: AlignmentInput) -> Vec<AlignmentLine> {
    if input.lyrics.is_empty() {
        return Vec::new();
    }
    if input.captions.is_empty() {
        return unmatched_all(&input.lyrics);
    }

    let captions: Vec<CaptionLine> = input
        .captions
        .into_iter()
        .filter(|caption| is_usable_caption(caption))
        .collect();
    if captions.is_empty() {
        return unmatched_all(&input.lyrics);
    }

    let pairs = align_sequence(&input.lyrics, &captions);
    let mut alignment: Vec<AlignmentLine> = input
        .lyrics
        .iter()
        .map(|lyric| {
            if let Some(caption_index) = pairs.get(&lyric.index).copied() {
                let caption = &captions[caption_index];
                let score =
                    best_text_match_score_for_caption(lyric, &normalize_text(&caption.text));
                AlignmentLine {
                    lyric_index: lyric.index,
                    caption_index: Some(caption.index),
                    start_ms: caption.start_ms,
                    end_ms: caption.end_ms,
                    confidence: score as f32,
                    needs_review: score < 0.84,
                }
            } else {
                AlignmentLine {
                    lyric_index: lyric.index,
                    caption_index: None,
                    start_ms: -1,
                    end_ms: -1,
                    confidence: 0.0,
                    needs_review: true,
                }
            }
        })
        .collect();
    interpolate_unsynced_timings(&mut alignment);
    alignment
}

fn is_usable_caption(caption: &CaptionLine) -> bool {
    !normalize_text(&caption.text).is_empty()
}

fn unmatched_all(lyrics: &[LyricLine]) -> Vec<AlignmentLine> {
    lyrics
        .iter()
        .map(|line| AlignmentLine {
            lyric_index: line.index,
            caption_index: None,
            start_ms: -1,
            end_ms: -1,
            confidence: 0.0,
            needs_review: true,
        })
        .collect()
}

fn align_sequence(
    lyrics: &[LyricLine],
    captions: &[CaptionLine],
) -> std::collections::HashMap<usize, usize> {
    let n = lyrics.len();
    let m = captions.len();
    let normalized_captions: Vec<String> = captions
        .iter()
        .map(|caption| normalize_text(&caption.text))
        .collect();
    let scores: Vec<Vec<f64>> = lyrics
        .par_iter()
        .map(|lyric| {
            captions
                .iter()
                .enumerate()
                .map(|(index, _caption)| {
                    let mut score =
                        best_text_match_score_for_caption(lyric, &normalized_captions[index]);
                    if index > 0 {
                        let combined = format!(
                            "{} {}",
                            normalized_captions[index - 1], normalized_captions[index]
                        );
                        score = score.max(best_text_match_score_for_caption(lyric, &combined));
                    }
                    score
                })
                .collect()
        })
        .collect();

    let mut dp = vec![vec![f64::NEG_INFINITY; m + 1]; n + 1];
    let mut choice = vec![vec![0u8; m + 1]; n + 1];
    dp[0][0] = 0.0;

    for j in 1..=m {
        dp[0][j] = 0.0;
        choice[0][j] = 2;
    }
    for i in 1..=n {
        dp[i][0] = 0.0;
        choice[i][0] = 1;
    }

    for i in 1..=n {
        for j in 1..=m {
            let mut best = dp[i - 1][j];
            let mut best_choice = 1u8;

            if dp[i][j - 1] > best {
                best = dp[i][j - 1];
                best_choice = 2;
            }

            let pair_score = scores[i - 1][j - 1];
            if pair_score >= MATCH_THRESHOLD {
                let matched = dp[i - 1][j - 1] + pair_score;
                if matched > best {
                    best = matched;
                    best_choice = 0;
                }
            }

            dp[i][j] = best;
            choice[i][j] = best_choice;
        }
    }

    let mut pairs = std::collections::HashMap::new();
    let mut i = n;
    let mut j = m;
    while i > 0 || j > 0 {
        match choice[i][j] {
            0 => {
                pairs.insert(lyrics[i - 1].index, j - 1);
                i -= 1;
                j -= 1;
            }
            1 => i -= 1,
            _ => j -= 1,
        }
    }

    pairs
}

fn best_text_match_score_for_caption(lyric: &LyricLine, caption_text: &str) -> f64 {
    if caption_text.is_empty() {
        return 0.0;
    }

    lyric_text_candidates(lyric)
        .into_iter()
        .map(normalize_text)
        .filter(|text| !text.is_empty())
        .map(|lyric_text| combined_similarity(&lyric_text, caption_text))
        .fold(0.0, f64::max)
}

fn interpolate_unsynced_timings(alignment: &mut [AlignmentLine]) {
    if alignment.is_empty() {
        return;
    }
    alignment.sort_by_key(|line| line.lyric_index);

    let avg_duration = average_synced_duration(alignment).unwrap_or(2000).max(400);
    let synced_positions: Vec<usize> = alignment
        .iter()
        .enumerate()
        .filter(|(_, line)| is_synced_line(line))
        .map(|(index, _)| index)
        .collect();

    if synced_positions.is_empty() {
        distribute_evenly(alignment, 0, alignment.len(), 0, avg_duration * alignment.len() as i64);
        return;
    }

    if synced_positions[0] > 0 {
        let end = alignment[synced_positions[0]].start_ms.max(0);
        distribute_evenly(alignment, 0, synced_positions[0], 0, end);
    }

    for window in synced_positions.windows(2) {
        let left = window[0];
        let right = window[1];
        let gap_start = alignment[left].end_ms.max(alignment[left].start_ms);
        let gap_end = alignment[right].start_ms;
        if gap_end > gap_start {
            distribute_evenly(alignment, left + 1, right, gap_start, gap_end);
        } else {
            distribute_evenly(
                alignment,
                left + 1,
                right,
                gap_start,
                gap_start + avg_duration * (right - left - 1) as i64,
            );
        }
    }

    let last_synced = *synced_positions.last().expect("checked above");
    if last_synced + 1 < alignment.len() {
        let start = alignment[last_synced]
            .end_ms
            .max(alignment[last_synced].start_ms);
        distribute_evenly(
            alignment,
            last_synced + 1,
            alignment.len(),
            start,
            start + avg_duration * (alignment.len() - last_synced - 1) as i64,
        );
    }
}

fn average_synced_duration(alignment: &[AlignmentLine]) -> Option<i64> {
    let durations: Vec<i64> = alignment
        .iter()
        .filter(|line| is_synced_line(line))
        .map(|line| (line.end_ms - line.start_ms).max(0))
        .filter(|duration| *duration > 0)
        .collect();
    if durations.is_empty() {
        return None;
    }
    Some(durations.iter().sum::<i64>() / durations.len() as i64)
}

fn distribute_evenly(
    alignment: &mut [AlignmentLine],
    from: usize,
    to: usize,
    range_start: i64,
    range_end: i64,
) {
    let unsynced: Vec<usize> = (from..to)
        .filter(|index| !is_synced_line(&alignment[*index]))
        .collect();
    if unsynced.is_empty() {
        return;
    }

    let count = unsynced.len() as i64;
    let range = (range_end - range_start).max(count * 250);
    let slot = (range / count).max(250);

    for (offset, index) in unsynced.iter().enumerate() {
        let start = range_start + slot * offset as i64;
        let end = if offset as i64 == count - 1 {
            range_end.max(start + slot)
        } else {
            start + slot
        };
        alignment[*index].start_ms = start;
        alignment[*index].end_ms = end.max(start + 200);
        alignment[*index].needs_review = true;
    }
}

pub fn has_playback_timing(line: &AlignmentLine) -> bool {
    line.start_ms >= 0 && line.end_ms >= line.start_ms
}

fn combined_similarity(lyric_text: &str, caption_text: &str) -> f64 {
    let significant_lyric = significant_tokens(lyric_text);
    let significant_caption = significant_tokens(caption_text);
    if significant_lyric.is_empty() || significant_caption.is_empty() {
        return 0.0;
    }

    let jw = jaro_winkler(lyric_text, caption_text);
    let token = significant_token_match_score(&significant_lyric, &significant_caption);
    let contain = containment_score(lyric_text, caption_text);

    let mut best = jw.max(contain);
    if token >= 0.6 {
        best = best.max(token);
    }
    if token >= 0.55 && jw >= 0.55 {
        best = best.max((token * 0.55) + (jw * 0.45));
    }

    // Reject matches where the important words do not line up at all.
    if token < 0.45 {
        return 0.0;
    }

    best
}

fn significant_tokens(text: &str) -> Vec<String> {
    text.split_whitespace()
        .filter(|token| !is_filler_token(token))
        .map(str::to_string)
        .collect()
}

fn is_filler_token(token: &str) -> bool {
    matches!(
        token,
        "a" | "an"
            | "the"
            | "and"
            | "or"
            | "to"
            | "in"
            | "on"
            | "at"
            | "of"
            | "yeah"
            | "oh"
            | "uh"
            | "ah"
            | "hey"
            | "woo"
            | "la"
            | "na"
            | "eh"
            | "oom"
            | "baby"
    )
}

fn significant_token_match_score(lyric_tokens: &[String], caption_tokens: &[String]) -> f64 {
    if lyric_tokens.is_empty() || caption_tokens.is_empty() {
        return 0.0;
    }

    let mut matched = 0usize;
    for lyric_token in lyric_tokens {
        let best = caption_tokens
            .iter()
            .map(|caption_token| {
                if lyric_token == caption_token {
                    1.0
                } else if lyric_token.contains(caption_token.as_str())
                    || caption_token.contains(lyric_token.as_str())
                {
                    0.9
                } else {
                    jaro_winkler(lyric_token, caption_token)
                }
            })
            .fold(0.0, f64::max);
        if best >= 0.78 {
            matched += 1;
        }
    }

    matched as f64 / lyric_tokens.len() as f64
}

fn containment_score(lyric_text: &str, caption_text: &str) -> f64 {
    let (longer, shorter) = if lyric_text.len() >= caption_text.len() {
        (lyric_text, caption_text)
    } else {
        (caption_text, lyric_text)
    };

    if shorter.len() < 3 || !longer.contains(shorter) {
        return 0.0;
    }

    let ratio = shorter.len() as f64 / longer.len() as f64;
    0.78 + (ratio * 0.2)
}

pub fn normalize_text(value: &str) -> String {
    let value = value.to_lowercase();
    let value = NORMALIZE_BRACKETED.replace_all(&value, " ");
    let value = NORMALIZE_PUNCTUATION.replace_all(&value, " ");
    let value = NORMALIZE_ROMANIZATION_NOISE.replace_all(&value, " ");
    NORMALIZE_WHITESPACE
        .replace_all(value.trim(), " ")
        .to_string()
}

static NORMALIZE_BRACKETED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[[^\]]+\]|\([^\)]+\)").expect("valid regex"));
static NORMALIZE_PUNCTUATION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[^\p{L}\p{N}\s]").expect("valid regex"));
static NORMALIZE_ROMANIZATION_NOISE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(yeah|oh|uh|ah|hey|woo|la|na)\b").expect("valid regex"));
static NORMALIZE_WHITESPACE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s+").expect("valid regex"));

pub fn is_synced_line(line: &AlignmentLine) -> bool {
    line.caption_index.is_some() && line.start_ms >= 0
}

pub fn alignment_playback_quality(lines: &[AlignmentLine]) -> f64 {
    lines
        .iter()
        .filter(|line| has_playback_timing(line))
        .map(|line| 1.0 + f64::from(line.confidence))
        .sum()
}

pub fn lyric_alignment_text(line: &LyricLine) -> &str {
    if let Some(text) = line
        .english
        .as_deref()
        .filter(|text| !text.trim().is_empty())
    {
        return text;
    }
    if let Some(text) = line
        .romanization
        .as_deref()
        .filter(|text| !text.trim().is_empty())
    {
        return text;
    }
    &line.original
}

pub fn lyric_whisper_alignment_text(line: &LyricLine, use_original: bool) -> &str {
    if use_original && !line.original.trim().is_empty() {
        return &line.original;
    }
    lyric_alignment_text(line)
}

pub fn lyric_line_for_forced_alignment(line: &LyricLine, use_original: bool) -> Option<&str> {
    let text = lyric_whisper_alignment_text(line, use_original).trim();
    if text.is_empty() || looks_like_metadata(&normalize_text(text)) {
        return None;
    }
    Some(text)
}

pub fn build_lyrics_forced_alignment_bundle_with_hints(
    lyrics: &[LyricLine],
    use_original: bool,
    hints: &[AlignmentLine],
) -> (String, Vec<crate::asr::ForcedAlignLine>) {
    let hint_by_index: HashMap<usize, &AlignmentLine> =
        hints.iter().map(|line| (line.lyric_index, line)).collect();
    let mut text = String::new();
    let mut lines = Vec::new();
    for line in lyrics {
        let Some(line_text) = lyric_line_for_forced_alignment(line, use_original) else {
            continue;
        };
        if !text.is_empty() {
            text.push(' ');
        }
        let char_start = text.len();
        text.push_str(line_text);
        let (hint_start_ms, hint_end_ms) = hint_by_index
            .get(&line.index)
            .filter(|hint| has_playback_timing(hint))
            .map(|hint| (Some(hint.start_ms), Some(hint.end_ms)))
            .unwrap_or((None, None));
        lines.push(crate::asr::ForcedAlignLine {
            index: line.index,
            text: line_text.to_string(),
            char_start,
            char_end: text.len(),
            hint_start_ms,
            hint_end_ms,
        });
    }
    (text, lines)
}

/// Reject forced-alignment output that clusters most lines on a few timestamps
/// or leaves an opening line spanning tens of seconds (symptom of bad FA word times).
pub fn forced_line_timing_quality(lines: &[AlignmentLine]) -> f64 {
    let synced: Vec<&AlignmentLine> = lines
        .iter()
        .filter(|line| has_playback_timing(line))
        .collect();
    if synced.len() < 4 {
        return 0.0;
    }

    let distinct_starts = synced
        .iter()
        .map(|line| line.start_ms)
        .collect::<HashSet<_>>()
        .len();
    let start_spread = distinct_starts as f64 / synced.len() as f64;
    if start_spread < 0.35 {
        return 0.0;
    }

    if let Some(first) = synced.iter().min_by_key(|line| line.lyric_index) {
        let first_span = first.end_ms - first.start_ms;
        if first_span > 25_000 {
            return 0.0;
        }
    }

    let max_span = synced
        .iter()
        .map(|line| line.end_ms - line.start_ms)
        .max()
        .unwrap_or(0);
    if max_span > 45_000 {
        return 0.0;
    }

    start_spread
}

fn is_cjk_codepoint(cp: u32) -> bool {
    (0x4E00..=0x9FFF).contains(&cp)
        || (0x3400..=0x4DBF).contains(&cp)
        || (0x3040..=0x309F).contains(&cp)
        || (0x30A0..=0x30FF).contains(&cp)
        || (0xAC00..=0xD7AF).contains(&cp)
        || (0x3000..=0x303F).contains(&cp)
        || (0xFF00..=0xFFEF).contains(&cp)
}

fn tokenise_alignment_words(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        if ch.is_whitespace() {
            if !current.is_empty() {
                out.push(std::mem::take(&mut current));
            }
            continue;
        }
        if is_cjk_codepoint(ch as u32) {
            if !current.is_empty() {
                out.push(std::mem::take(&mut current));
            }
            out.push(ch.to_string());
        } else {
            current.push(ch);
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

pub fn clamp_alignment_line_ends(alignment: &mut [AlignmentLine]) {
    let synced_indices: Vec<usize> = alignment
        .iter()
        .enumerate()
        .filter(|(_, line)| has_playback_timing(line))
        .map(|(index, _)| index)
        .collect();

    let mut ordered = synced_indices;
    ordered.sort_by_key(|&index| alignment[index].start_ms);

    for window in ordered.windows(2) {
        let left = window[0];
        let right = window[1];
        let next_start = alignment[right].start_ms;
        let line = &mut alignment[left];
        if line.end_ms >= next_start {
            line.end_ms = (next_start - 1).max(line.start_ms + 200);
        }
    }
}

pub fn align_lyrics_from_line_timings(
    lyrics: &[LyricLine],
    timings: &[crate::asr::AsrLineTiming],
) -> Vec<AlignmentLine> {
    let mut alignment: Vec<AlignmentLine> = lyrics
        .iter()
        .map(|line| {
            timings
                .iter()
                .find(|timing| timing.lyric_index == line.index)
                .and_then(|timing| {
                    let start_ms = timing.start_ms?;
                    let end_ms = timing.end_ms?;
                    Some(AlignmentLine {
                        lyric_index: line.index,
                        caption_index: Some(0),
                        start_ms,
                        end_ms: end_ms.max(start_ms + 200),
                        confidence: 0.98,
                        needs_review: false,
                    })
                })
                .unwrap_or_else(|| unsynced_alignment_line(line.index))
        })
        .collect();
    clamp_alignment_line_ends(&mut alignment);
    let synced_count = alignment.iter().filter(|line| is_synced_line(line)).count();
    if synced_count * 2 < alignment.len() {
        interpolate_whisper_unsynced_timings(&mut alignment);
    }
    alignment
}

pub fn align_lyrics_to_forced_words(
    lyrics: &[LyricLine],
    words: &[crate::asr::AsrWord],
    use_original: bool,
) -> Vec<AlignmentLine> {
    if lyrics.is_empty() {
        return Vec::new();
    }
    if words.is_empty() {
        return lyrics
            .iter()
            .map(|line| unsynced_alignment_line(line.index))
            .collect();
    }

    let mut word_index = 0;
    let mut alignment = Vec::with_capacity(lyrics.len());
    for line in lyrics {
        let text = lyric_whisper_alignment_text(line, use_original);
        if text.trim().is_empty() || looks_like_metadata(&normalize_text(text)) {
            alignment.push(unsynced_alignment_line(line.index));
            continue;
        }

        let expected = tokenise_alignment_words(text);
        if expected.is_empty() {
            alignment.push(unsynced_alignment_line(line.index));
            continue;
        }

        if word_index + expected.len() > words.len() {
            alignment.push(unsynced_alignment_line(line.index));
            continue;
        }

        let chunk = &words[word_index..word_index + expected.len()];
        word_index += expected.len();
        let start_ms = chunk.iter().map(|word| word.start_ms).min().unwrap_or(0);
        let end_ms = chunk
            .iter()
            .map(|word| word.end_ms)
            .max()
            .unwrap_or(start_ms + 900)
            .max(start_ms + 200);
        alignment.push(AlignmentLine {
            lyric_index: line.index,
            caption_index: Some(word_index.saturating_sub(1)),
            start_ms,
            end_ms,
            confidence: 0.98,
            needs_review: false,
        });
    }

    if word_index != words.len() {
        return align_lyrics_to_timed_words(lyrics, words, use_original);
    }

    clamp_alignment_line_ends(&mut alignment);
    interpolate_whisper_unsynced_timings(&mut alignment);
    alignment
}

pub fn align_lyrics_to_timed_words(
    lyrics: &[LyricLine],
    words: &[crate::asr::AsrWord],
    use_original: bool,
) -> Vec<AlignmentLine> {
    if lyrics.is_empty() {
        return Vec::new();
    }
    if words.is_empty() {
        return lyrics
            .iter()
            .map(|line| unsynced_alignment_line(line.index))
            .collect();
    }

    let n = lyrics.len();
    let m = words.len();
    let normalized_lyrics: Vec<String> = lyrics
        .par_iter()
        .map(|line| normalize_text(lyric_whisper_alignment_text(line, use_original)))
        .collect();
    let normalized_words: Vec<String> = words
        .par_iter()
        .map(|word| normalize_text(&word.text))
        .collect();
    let span_texts: Vec<Vec<String>> = (0..m)
        .into_par_iter()
        .map(|start| {
            let max_end = (start + WHISPER_MAX_WORD_SPAN).min(m);
            (start..max_end)
                .map(|end| normalized_words[start..=end].join(" "))
                .collect()
        })
        .collect();
    let span_scores: Vec<Vec<Vec<f64>>> = (0..n)
        .into_par_iter()
        .map(|lyric_idx| {
            let target = &normalized_lyrics[lyric_idx];
            if target.is_empty() || looks_like_metadata(target) {
                return vec![vec![0.0; WHISPER_MAX_WORD_SPAN]; m];
            }
            (0..m)
                .map(|start| {
                    let mut offsets = vec![0.0; WHISPER_MAX_WORD_SPAN];
                    for (offset, span_text) in span_texts[start].iter().enumerate() {
                        offsets[offset] = whisper_span_score_normalized(target, span_text);
                    }
                    offsets
                })
                .collect()
        })
        .collect();

    let mut dp = vec![vec![f64::NEG_INFINITY; m + 1]; n + 1];
    let mut prev = vec![vec![WhisperAlignChoice::default(); m + 1]; n + 1];
    dp[0][0] = 0.0;

    for i in 1..=n {
        let skip_only =
            normalized_lyrics[i - 1].is_empty() || looks_like_metadata(&normalized_lyrics[i - 1]);

        for j in 0..=m {
            if j > 0 && dp[i][j - 1] > dp[i][j] {
                dp[i][j] = dp[i][j - 1];
                prev[i][j] = WhisperAlignChoice::SkipWord;
            }

            if dp[i - 1][j] > dp[i][j] {
                dp[i][j] = dp[i - 1][j];
                prev[i][j] = WhisperAlignChoice::SkipLyric;
            }

            if skip_only {
                continue;
            }

            for end in j..m.min(j + WHISPER_MAX_WORD_SPAN) {
                let score = span_scores[i - 1][j][end - j];
                if score <= 0.0 {
                    continue;
                }
                let candidate = dp[i - 1][j] + score;
                if candidate > dp[i][end + 1] {
                    dp[i][end + 1] = candidate;
                    prev[i][end + 1] = WhisperAlignChoice::Match {
                        word_start: j,
                        word_end: end,
                        score,
                    };
                }
            }
        }
    }

    let end_word = (0..=m)
        .max_by(|left, right| {
            dp[n][*left]
                .partial_cmp(&dp[n][*right])
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .unwrap_or(0);

    let mut matched = vec![None; n];
    let mut i = n;
    let mut j = end_word;
    while i > 0 {
        match prev[i][j] {
            WhisperAlignChoice::SkipLyric => {
                i -= 1;
            }
            WhisperAlignChoice::SkipWord => {
                j -= 1;
            }
            WhisperAlignChoice::Match {
                word_start,
                word_end,
                score,
            } => {
                matched[i - 1] = Some((word_start, word_end, score));
                i -= 1;
                j = word_start;
            }
            WhisperAlignChoice::Unset => {
                i -= 1;
            }
        }
    }

    let mut alignment = Vec::with_capacity(n);
    for (index, line) in lyrics.iter().enumerate() {
        if let Some((word_start, word_end, score)) = matched[index] {
            alignment.push(AlignmentLine {
                lyric_index: line.index,
                caption_index: Some(word_end),
                start_ms: words[word_start].start_ms,
                end_ms: words[word_end]
                    .end_ms
                    .max(words[word_end].start_ms + 200),
                confidence: score as f32,
                needs_review: score < 0.86,
            });
        } else {
            alignment.push(unsynced_alignment_line(line.index));
        }
    }

    clamp_alignment_line_ends(&mut alignment);
    interpolate_whisper_unsynced_timings(&mut alignment);
    alignment
}

#[derive(Clone, Copy, Debug, Default)]
enum WhisperAlignChoice {
    #[default]
    Unset,
    SkipLyric,
    SkipWord,
    Match {
        word_start: usize,
        word_end: usize,
        score: f64,
    },
}

fn whisper_span_score_normalized(target: &str, span_text: &str) -> f64 {
    if target.is_empty() || span_text.is_empty() {
        return 0.0;
    }

    let similarity = combined_similarity(target, span_text);
    if similarity < WHISPER_MATCH_THRESHOLD {
        return 0.0;
    }
    if !leading_token_matches(target, span_text) {
        return 0.0;
    }

    let recall = ordered_token_recall(target, span_text);
    if recall < WHISPER_MIN_TOKEN_RECALL {
        return 0.0;
    }

    similarity * (0.55 + (0.45 * recall))
}

fn leading_token_matches(lyric: &str, span: &str) -> bool {
    let lyric_tokens = significant_tokens(lyric);
    let span_tokens = significant_tokens(span);
    if lyric_tokens.is_empty() || span_tokens.is_empty() {
        return false;
    }

    let lookahead = span_tokens.len().min(8);
    lyric_tokens.iter().take(3).any(|lyric_token| {
        span_tokens
            .iter()
            .take(lookahead)
            .any(|span_token| token_similar(lyric_token, span_token) >= 0.88)
    })
}

fn ordered_token_recall(lyric: &str, span: &str) -> f64 {
    let lyric_tokens = significant_tokens(lyric);
    if lyric_tokens.is_empty() {
        return 0.0;
    }
    let span_tokens = significant_tokens(span);
    if span_tokens.is_empty() {
        return 0.0;
    }

    let mut span_index = 0usize;
    let mut matched = 0usize;
    for lyric_token in &lyric_tokens {
        while span_index < span_tokens.len() {
            if token_similar(lyric_token, &span_tokens[span_index]) >= 0.85 {
                matched += 1;
                span_index += 1;
                break;
            }
            span_index += 1;
        }
    }

    matched as f64 / lyric_tokens.len() as f64
}

fn token_similar(left: &str, right: &str) -> f64 {
    if left == right {
        1.0
    } else {
        jaro_winkler(left, right)
    }
}

fn interpolate_whisper_unsynced_timings(alignment: &mut [AlignmentLine]) {
    interpolate_unsynced_timings(alignment);

    alignment.sort_by_key(|line| line.lyric_index);
    let synced_positions: Vec<usize> = alignment
        .iter()
        .enumerate()
        .filter(|(_, line)| is_synced_line(line))
        .map(|(index, _)| index)
        .collect();

    for window in synced_positions.windows(2) {
        let left = window[0];
        let right = window[1];
        let gap_lines = right - left - 1;
        if gap_lines == 0 {
            continue;
        }

        let gap_start = alignment[left].end_ms.max(alignment[left].start_ms);
        let gap_end = alignment[right].start_ms;
        let needed = gap_lines as i64 * WHISPER_MIN_INTERPOLATED_LINE_MS;
        let distribute_end = if gap_end - gap_start >= needed {
            gap_end
        } else {
            gap_start + needed
        };
        distribute_evenly(alignment, left + 1, right, gap_start, distribute_end);
    }
}

fn unsynced_alignment_line(lyric_index: usize) -> AlignmentLine {
    AlignmentLine {
        lyric_index,
        caption_index: None,
        start_ms: -1,
        end_ms: -1,
        confidence: 0.0,
        needs_review: true,
    }
}

fn looks_like_metadata(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower.starts_with("english:")
        || lower.starts_with("credits")
        || lower.starts_with("disclaimer")
}

fn lyric_text_candidates(line: &LyricLine) -> Vec<&str> {
    let mut values = Vec::new();
    if let Some(text) = line
        .english
        .as_deref()
        .filter(|text| !text.trim().is_empty())
    {
        values.push(text);
    }
    if let Some(text) = line
        .romanization
        .as_deref()
        .filter(|text| !text.trim().is_empty())
    {
        values.push(text);
    }
    if !line.original.trim().is_empty() {
        values.push(&line.original);
    }
    values
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lyric(index: usize, text: &str) -> LyricLine {
        LyricLine {
            id: None,
            song_id: Some(1),
            index,
            member: None,
            original: text.to_string(),
            romanization: Some(text.to_string()),
            english: None,
            with_all: false,
            layer: crate::models::LyricLayer::default(),
            segments: Vec::new(),
        }
    }

    fn caption(index: usize, text: &str, start_ms: i64) -> CaptionLine {
        CaptionLine {
            id: None,
            video_id: "v".into(),
            index,
            start_ms,
            end_ms: start_ms + 900,
            text: text.to_string(),
        }
    }

    #[test]
    fn aligns_exact_and_partial_sequence() {
        let output = align_lines(AlignmentInput {
            lyrics: vec![lyric(0, "hello hello"), lyric(1, "shine tonight")],
            captions: vec![
                caption(0, "hello hello", 1000),
                caption(1, "shine bright tonight", 2500),
            ],
        });
        assert_eq!(output[0].caption_index, Some(0));
        assert_eq!(output[1].caption_index, Some(1));
        assert!(!output[0].needs_review);
        assert!(output[1].confidence > 0.8);
    }

    #[test]
    fn missing_lyric_lines_stay_unsynced_without_timeline_slots() {
        let output = align_lines(AlignmentInput {
            lyrics: vec![lyric(0, "first"), lyric(1, "missing"), lyric(2, "last")],
            captions: vec![caption(0, "first", 1000), caption(1, "last", 5000)],
        });
        assert_eq!(output[1].caption_index, None);
        assert!(has_playback_timing(&output[1]));
        assert!(output[1].start_ms >= output[0].end_ms);
        assert!(output[1].end_ms <= output[2].start_ms);
        assert!(output[1].needs_review);
    }

    #[test]
    fn missing_opening_lyric_does_not_shift_later_caption_matches() {
        let output = align_lines(AlignmentInput {
            lyrics: vec![
                lyric(0, "SSERAFIM baby"),
                lyric(1, "Boompala boom boom"),
                lyric(2, "next line here"),
            ],
            captions: vec![
                caption(0, "Boompala boom boom", 5000),
                caption(1, "next line here", 7000),
            ],
        });
        assert_eq!(output[0].caption_index, None);
        assert!(has_playback_timing(&output[0]));
        assert!(output[0].end_ms <= output[1].start_ms);
        assert_eq!(output[1].caption_index, Some(0));
        assert_eq!(output[1].start_ms, 5000);
        assert_eq!(output[2].caption_index, Some(1));
        assert_eq!(output[2].start_ms, 7000);
    }

    #[test]
    fn multiple_missing_lyrics_do_not_shift_the_rest() {
        let output = align_lines(AlignmentInput {
            lyrics: vec![
                lyric(0, "missing intro"),
                lyric(1, "also missing"),
                lyric(2, "first synced"),
                lyric(3, "second synced"),
            ],
            captions: vec![
                caption(0, "first synced", 10000),
                caption(1, "second synced", 12000),
            ],
        });
        assert_eq!(output[0].caption_index, None);
        assert_eq!(output[1].caption_index, None);
        assert_eq!(output[2].caption_index, Some(0));
        assert_eq!(output[2].start_ms, 10000);
        assert_eq!(output[3].caption_index, Some(1));
        assert_eq!(output[3].start_ms, 12000);
    }

    #[test]
    fn garbled_caption_does_not_count_as_missing_lyric() {
        let output = align_lines(AlignmentInput {
            lyrics: vec![
                lyric(0, "SSERAFIM baby"),
                lyric(1, "Oom bala oom bala"),
            ],
            captions: vec![
                caption(0, "Surfing, baby.", 33680),
                caption(1, "Oom bala. A oom bala oom bala", 37760),
            ],
        });
        assert_eq!(output[0].caption_index, None);
        assert!(has_playback_timing(&output[0]));
        assert!(output[0].end_ms <= output[1].start_ms);
        assert_eq!(output[1].caption_index, Some(1));
        assert_eq!(output[1].start_ms, 37760);
    }

    #[test]
    fn does_not_match_unrelated_caption_with_only_shared_noise() {
        let score = combined_similarity("sserafim baby", "surfing baby");
        assert!(score < MATCH_THRESHOLD);
    }

    #[test]
    fn aligns_against_best_available_lyric_variant() {
        let output = align_lines(AlignmentInput {
            lyrics: vec![LyricLine {
                id: None,
                song_id: Some(1),
                index: 0,
                member: None,
                original: "어린 맘속 헤매던 cosmos".to_string(),
                romanization: Some("eorin mamsok hemaedeon cosmos".to_string()),
                english: Some("My childish heart once lost in the cosmos".to_string()),
                with_all: false,
                layer: crate::models::LyricLayer::default(),
                segments: Vec::new(),
            }],
            captions: vec![caption(
                0,
                "My childish heart once lost in the cosmos",
                17233,
            )],
        });
        assert_eq!(output[0].caption_index, Some(0));
        assert!(output[0].confidence > 0.95);
    }

    #[test]
    fn matches_similar_but_not_identical_caption_text() {
        let output = align_lines(AlignmentInput {
            lyrics: vec![lyric(0, "boompala boom boom")],
            captions: vec![caption(0, "boompala boom boom yeah", 1200)],
        });
        assert_eq!(output[0].caption_index, Some(0));
        assert!(output[0].confidence >= MATCH_THRESHOLD as f32);
    }

    fn english_lyric(index: usize, text: &str) -> LyricLine {
        LyricLine {
            id: None,
            song_id: Some(1),
            index,
            member: None,
            original: String::new(),
            romanization: None,
            english: Some(text.to_string()),
            with_all: false,
            layer: crate::models::LyricLayer::default(),
            segments: Vec::new(),
        }
    }

    #[test]
    fn interpolates_garbled_caption_gap_for_twice_intro() {
        let output = align_lines(AlignmentInput {
            lyrics: vec![
                english_lyric(0, "(Hahaha) This is for all my ladies"),
                english_lyric(1, "Who don't get hyped enough (Hey ladies)"),
                english_lyric(2, "If you've been done wrong"),
                english_lyric(3, "Then this your song so turn it up (Turn it up for me uh uh)"),
            ],
            captions: vec![
                caption(0, "This is for all my ladies who don't get", 3760),
                caption(1, "hyped to love. If you've been wrong,", 6320),
                caption(2, "that is your song. So turn it up. I want", 8720),
            ],
        });
        let middle = &output[1];
        assert_eq!(middle.caption_index, None);
        assert!(has_playback_timing(middle));
        assert!(middle.start_ms >= output[0].end_ms);
        assert!(middle.end_ms <= output[2].start_ms);
        assert!(middle.start_ms >= 3760);
        assert!(middle.end_ms <= 8720);
    }

    #[test]
    fn picks_english_manual_captions_over_korean_for_english_lyrics() {
        let lyrics = vec![
            lyric(0, "Boompala boompala boompala yeah"),
            lyric(1, "You can't hold on to the clouds in the air"),
            lyric(2, "Wake up saying hi to the mirror"),
        ];
        let english = vec![
            caption(0, "Boompala boompala boompala yeah", 38815),
            caption(1, "You can't hold on to the clouds in the air", 43330),
            caption(2, "Wake up saying hi to the mirror", 48259),
        ];
        let korean = vec![
            caption(0, "Boompala boompala boompala yeah", 38815),
            caption(1, "허공의 구름에 매달릴 수 없어", 43330),
            caption(2, "일어나 거울 속 나에게 인사해", 48259),
        ];

        let english_score = alignment_playback_quality(&align_lines(AlignmentInput {
            lyrics: lyrics.clone(),
            captions: english,
        }));
        let korean_score = alignment_playback_quality(&align_lines(AlignmentInput {
            lyrics,
            captions: korean,
        }));

        assert!(english_score > korean_score);
    }

    #[test]
    fn aligns_english_lyrics_to_forced_word_timestamps() {
        use crate::asr::AsrWord;

        let lyrics = vec![
            english_lyric(0, "This is the first line"),
            english_lyric(1, "Here comes the second"),
        ];
        let words = vec![
            AsrWord {
                text: "This".into(),
                start_ms: 1000,
                end_ms: 1200,
            },
            AsrWord {
                text: "is".into(),
                start_ms: 1200,
                end_ms: 1350,
            },
            AsrWord {
                text: "the".into(),
                start_ms: 1350,
                end_ms: 1500,
            },
            AsrWord {
                text: "first".into(),
                start_ms: 1500,
                end_ms: 1700,
            },
            AsrWord {
                text: "line".into(),
                start_ms: 1700,
                end_ms: 2000,
            },
            AsrWord {
                text: "Here".into(),
                start_ms: 2200,
                end_ms: 2400,
            },
            AsrWord {
                text: "comes".into(),
                start_ms: 2400,
                end_ms: 2600,
            },
            AsrWord {
                text: "the".into(),
                start_ms: 2600,
                end_ms: 2750,
            },
            AsrWord {
                text: "second".into(),
                start_ms: 2750,
                end_ms: 3100,
            },
        ];

        let output = align_lyrics_to_forced_words(&lyrics, &words, false);
        assert_eq!(output.len(), 2);
        assert!(is_synced_line(&output[0]));
        assert!(is_synced_line(&output[1]));
        assert_eq!(output[0].start_ms, 1000);
        assert_eq!(output[1].start_ms, 2200);
    }

    #[test]
    fn aligns_english_lyrics_to_whisper_word_timestamps() {
        use crate::asr::AsrWord;

        let lyrics = vec![
            english_lyric(0, "(Hahaha) This is for all my ladies"),
            english_lyric(1, "Who don't get hyped enough (Hey ladies)"),
            english_lyric(2, "If you've been done wrong"),
        ];
        let words = vec![
            AsrWord {
                text: "This".into(),
                start_ms: 3760,
                end_ms: 3900,
            },
            AsrWord {
                text: "is".into(),
                start_ms: 3900,
                end_ms: 4020,
            },
            AsrWord {
                text: "for".into(),
                start_ms: 4020,
                end_ms: 4140,
            },
            AsrWord {
                text: "all".into(),
                start_ms: 4140,
                end_ms: 4260,
            },
            AsrWord {
                text: "my".into(),
                start_ms: 4260,
                end_ms: 4380,
            },
            AsrWord {
                text: "ladies".into(),
                start_ms: 4380,
                end_ms: 4700,
            },
            AsrWord {
                text: "who".into(),
                start_ms: 4700,
                end_ms: 4820,
            },
            AsrWord {
                text: "don't".into(),
                start_ms: 4820,
                end_ms: 4980,
            },
            AsrWord {
                text: "get".into(),
                start_ms: 4980,
                end_ms: 5080,
            },
            AsrWord {
                text: "hyped".into(),
                start_ms: 5080,
                end_ms: 5240,
            },
            AsrWord {
                text: "enough".into(),
                start_ms: 5240,
                end_ms: 5480,
            },
            AsrWord {
                text: "If".into(),
                start_ms: 5600,
                end_ms: 5720,
            },
            AsrWord {
                text: "you've".into(),
                start_ms: 5720,
                end_ms: 5880,
            },
            AsrWord {
                text: "been".into(),
                start_ms: 5880,
                end_ms: 6000,
            },
            AsrWord {
                text: "done".into(),
                start_ms: 6000,
                end_ms: 6160,
            },
            AsrWord {
                text: "wrong".into(),
                start_ms: 6160,
                end_ms: 6400,
            },
        ];

        let output = align_lyrics_to_timed_words(&lyrics, &words, false);
        assert!(has_playback_timing(&output[1]));
        assert!(output[1].start_ms >= 4700);
        assert!(output[1].end_ms <= 5600);
        assert!(output[1].confidence >= WHISPER_MATCH_THRESHOLD as f32);
    }

    #[test]
    fn rejects_whisper_false_match_on_different_opening_word() {
        use crate::asr::AsrWord;

        let lyrics = vec![
            english_lyric(0, "Have you feeling low when you're grown you got the"),
            english_lyric(1, "(Ooh) This your moment go get it"),
        ];
        let words = vec![
            AsrWord {
                text: "feeling".into(),
                start_ms: 31_700,
                end_ms: 32_000,
            },
            AsrWord {
                text: "low".into(),
                start_ms: 32_000,
                end_ms: 32_300,
            },
            AsrWord {
                text: "You".into(),
                start_ms: 33_740,
                end_ms: 33_900,
            },
            AsrWord {
                text: "got".into(),
                start_ms: 33_900,
                end_ms: 34_100,
            },
            AsrWord {
                text: "it".into(),
                start_ms: 34_100,
                end_ms: 34_300,
            },
            AsrWord {
                text: "you".into(),
                start_ms: 34_820,
                end_ms: 35_000,
            },
            AsrWord {
                text: "already".into(),
                start_ms: 35_280,
                end_ms: 35_600,
            },
            AsrWord {
                text: "know".into(),
                start_ms: 35_680,
                end_ms: 36_000,
            },
        ];

        let output = align_lyrics_to_timed_words(&lyrics, &words, false);
        assert!(!is_synced_line(&output[1]));
    }

    #[test]
    fn rejects_clustered_forced_line_timings() {
        let lines = vec![
            AlignmentLine {
                lyric_index: 0,
                caption_index: Some(0),
                start_ms: 0,
                end_ms: 100_000,
                confidence: 0.98,
                needs_review: false,
            },
            AlignmentLine {
                lyric_index: 1,
                caption_index: Some(0),
                start_ms: 103_120,
                end_ms: 103_320,
                confidence: 0.98,
                needs_review: false,
            },
            AlignmentLine {
                lyric_index: 2,
                caption_index: Some(0),
                start_ms: 103_120,
                end_ms: 103_320,
                confidence: 0.98,
                needs_review: false,
            },
            AlignmentLine {
                lyric_index: 3,
                caption_index: Some(0),
                start_ms: 103_120,
                end_ms: 103_320,
                confidence: 0.98,
                needs_review: false,
            },
        ];
        assert_eq!(forced_line_timing_quality(&lines), 0.0);
    }

    #[test]
    fn accepts_spread_forced_line_timings() {
        let lines = (0..8)
            .map(|index| AlignmentLine {
                lyric_index: index,
                caption_index: Some(0),
                start_ms: 90_000 + (index as i64 * 2_000),
                end_ms: 90_000 + (index as i64 * 2_000) + 1_500,
                confidence: 0.98,
                needs_review: false,
            })
            .collect::<Vec<_>>();
        assert!(forced_line_timing_quality(&lines) > 0.0);
    }
}
