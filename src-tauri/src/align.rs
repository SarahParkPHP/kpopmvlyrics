use std::collections::HashSet;

use regex::Regex;
use strsim::jaro_winkler;

use crate::models::{AlignmentLine, CaptionLine, LyricLine};

const MATCH_THRESHOLD: f64 = 0.72;

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
                let score = match_score(lyric, caption);
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
    let scores: Vec<Vec<f64>> = lyrics
        .iter()
        .map(|lyric| {
            captions
                .iter()
                .enumerate()
                .map(|(index, caption)| {
                    let mut score = match_score(lyric, caption);
                    if index > 0 {
                        score = score.max(combined_caption_match_score(
                            lyric,
                            &captions[index - 1],
                            caption,
                        ));
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

fn match_score(lyric: &LyricLine, caption: &CaptionLine) -> f64 {
    best_text_match_score(lyric, &caption.text)
}

fn combined_caption_match_score(
    lyric: &LyricLine,
    left: &CaptionLine,
    right: &CaptionLine,
) -> f64 {
    let combined = format!("{} {}", left.text.trim(), right.text.trim());
    best_text_match_score(lyric, &combined)
}

fn best_text_match_score(lyric: &LyricLine, caption_text: &str) -> f64 {
    let caption_text = normalize_text(caption_text);
    if caption_text.is_empty() {
        return 0.0;
    }

    lyric_text_candidates(lyric)
        .into_iter()
        .map(normalize_text)
        .filter(|text| !text.is_empty())
        .map(|lyric_text| combined_similarity(&lyric_text, &caption_text))
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

fn token_overlap_score(a: &str, b: &str) -> f64 {
    let ta: HashSet<&str> = a.split_whitespace().collect();
    let tb: HashSet<&str> = b.split_whitespace().collect();
    if ta.is_empty() || tb.is_empty() {
        return 0.0;
    }

    let shared = ta.intersection(&tb).count();
    if shared == 0 {
        return 0.0;
    }

    let min_len = ta.len().min(tb.len());
    let max_len = ta.len().max(tb.len());
    let overlap = shared as f64 / min_len as f64;
    let coverage = shared as f64 / max_len as f64;
    overlap * 0.75 + coverage * 0.25
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
    let bracketed = Regex::new(r"\[[^\]]+\]|\([^\)]+\)").expect("valid regex");
    let punctuation = Regex::new(r"[^\p{L}\p{N}\s]").expect("valid regex");
    let romanization_noise = Regex::new(r"\b(yeah|oh|uh|ah|hey|woo|la|na)\b").expect("valid regex");
    let whitespace = Regex::new(r"\s+").expect("valid regex");
    let value = value.to_lowercase();
    let value = bracketed.replace_all(&value, " ");
    let value = punctuation.replace_all(&value, " ");
    let value = romanization_noise.replace_all(&value, " ");
    whitespace.replace_all(value.trim(), " ").to_string()
}

pub fn is_synced_line(line: &AlignmentLine) -> bool {
    line.caption_index.is_some() && line.start_ms >= 0
}

pub fn alignment_quality(lines: &[AlignmentLine]) -> f64 {
    lines
        .iter()
        .filter(|line| is_synced_line(line))
        .map(|line| 1.0 + f64::from(line.confidence))
        .sum()
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

        let english_score = alignment_quality(&align_lines(AlignmentInput {
            lyrics: lyrics.clone(),
            captions: english,
        }));
        let korean_score = alignment_quality(&align_lines(AlignmentInput {
            lyrics,
            captions: korean,
        }));

        assert!(english_score > korean_score);
    }
}
