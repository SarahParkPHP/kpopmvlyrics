use regex::Regex;
use strsim::jaro_winkler;

use crate::models::{AlignmentLine, CaptionLine, LyricLine};

pub struct AlignmentInput {
    pub lyrics: Vec<LyricLine>,
    pub captions: Vec<CaptionLine>,
}

pub fn align_lines(input: AlignmentInput) -> Vec<AlignmentLine> {
    if input.lyrics.is_empty() {
        return Vec::new();
    }
    if input.captions.is_empty() {
        return interpolate_all(&input.lyrics);
    }

    let mut result = Vec::with_capacity(input.lyrics.len());
    let mut caption_cursor = 0usize;

    for lyric in &input.lyrics {
        let search_end = (caption_cursor + 8).min(input.captions.len());
        let mut best: Option<(usize, f64)> = None;

        for caption_index in caption_cursor..search_end {
            let caption = &input.captions[caption_index];
            let caption_text = normalize_text(&caption.text);
            let score = lyric_text_candidates(lyric)
                .into_iter()
                .map(normalize_text)
                .filter(|text| !text.is_empty() && !caption_text.is_empty())
                .map(|text| jaro_winkler(&text, &caption_text))
                .fold(0.0, f64::max);
            if best
                .map(|(_, best_score)| score > best_score)
                .unwrap_or(true)
            {
                best = Some((caption_index, score));
            }
        }

        if let Some((caption_index, score)) = best.filter(|(_, score)| *score >= 0.72) {
            let caption = &input.captions[caption_index];
            caption_cursor = caption_index.saturating_add(1);
            result.push(AlignmentLine {
                lyric_index: lyric.index,
                caption_index: Some(caption.index),
                start_ms: caption.start_ms,
                end_ms: caption.end_ms,
                confidence: score as f32,
                needs_review: score < 0.84,
            });
        } else {
            result.push(AlignmentLine {
                lyric_index: lyric.index,
                caption_index: None,
                start_ms: -1,
                end_ms: -1,
                confidence: 0.0,
                needs_review: true,
            });
        }
    }

    interpolate_missing(result)
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

fn interpolate_missing(mut lines: Vec<AlignmentLine>) -> Vec<AlignmentLine> {
    let len = lines.len();
    for index in 0..len {
        if lines[index].start_ms >= 0 {
            continue;
        }
        let prev = (0..index).rev().find(|i| lines[*i].start_ms >= 0);
        let next = (index + 1..len).find(|i| lines[*i].start_ms >= 0);
        match (prev, next) {
            (Some(prev), Some(next)) if next > prev + 1 => {
                let gap = (lines[next].start_ms - lines[prev].end_ms).max(250);
                let slots = (next - prev) as i64;
                let slot = gap / slots;
                let offset = (index - prev) as i64;
                lines[index].start_ms = lines[prev].end_ms + slot * offset;
                lines[index].end_ms =
                    (lines[index].start_ms + slot.max(700)).min(lines[next].start_ms);
            }
            (Some(prev), Some(next)) => {
                let start = lines[prev].end_ms;
                lines[index].start_ms = start;
                lines[index].end_ms = start.max(lines[next].start_ms);
            }
            (Some(prev), None) => {
                let start = lines[prev].end_ms + 1200 * (index - prev) as i64;
                lines[index].start_ms = start;
                lines[index].end_ms = start + 1100;
            }
            (None, Some(next)) => {
                let start = (lines[next].start_ms - 1200 * (next - index) as i64).max(0);
                lines[index].start_ms = start;
                lines[index].end_ms = (start + 1100).min(lines[next].start_ms);
            }
            (None, None) => {
                lines[index].start_ms = index as i64 * 1400;
                lines[index].end_ms = lines[index].start_ms + 1200;
            }
        }
        lines[index].needs_review = true;
    }
    lines
}

fn interpolate_all(lyrics: &[LyricLine]) -> Vec<AlignmentLine> {
    lyrics
        .iter()
        .map(|line| AlignmentLine {
            lyric_index: line.index,
            caption_index: None,
            start_ms: line.index as i64 * 1500,
            end_ms: line.index as i64 * 1500 + 1300,
            confidence: 0.0,
            needs_review: true,
        })
        .collect()
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
    fn interpolates_missing_lines_for_review() {
        let output = align_lines(AlignmentInput {
            lyrics: vec![lyric(0, "first"), lyric(1, "missing"), lyric(2, "last")],
            captions: vec![caption(0, "first", 1000), caption(1, "last", 5000)],
        });
        assert_eq!(output[1].caption_index, None);
        assert!(output[1].start_ms > output[0].end_ms);
        assert!(output[1].needs_review);
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
}
