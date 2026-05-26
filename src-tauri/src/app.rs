use std::path::PathBuf;
use std::sync::Mutex;

use crate::align::{
    align_lines, align_lyrics_from_line_timings, align_lyrics_to_forced_words,
    align_lyrics_to_timed_words, alignment_playback_quality, forced_line_timing_quality,
    AlignmentInput,
};
use crate::asr::{
    asr_available, asr_caption_lines, asr_setup_hint, effective_asr_model, forced_align_language,
    transcribe_video, AsrModelSize, AsrTranscript,
};
use crate::log::{progress, PhaseGuard, verbose};
use crate::lyrics::lyric_language_toggles;
use rayon::prelude::*;
use crate::captions::{parse_caption_text, CaptionProvider, YouTubeCaptionProvider};
use crate::db::Repository;
use crate::lyrics::{ColorCodedLyricsProvider, GeniusProvider, LyricsProvider};
use crate::members::{KpopFandomProvider, KpoppingProvider, MemberProfileProvider};
use crate::models::*;
use crate::video;

#[derive(Debug, Clone)]
pub struct AlignResult {
    pub alignment: Vec<AlignmentLine>,
    pub captions: Vec<CaptionLine>,
    pub summary: String,
}

pub struct AppContext {
    repo: Mutex<Repository>,
}

const ASR_MODEL_SETTING: &str = "whisper_model";

impl AppContext {
    pub fn open() -> Result<Self, String> {
        let app_dir = app_data_dir()?;
        std::fs::create_dir_all(&app_dir).map_err(to_string)?;
        let repo = Repository::open(app_dir.join("kpopmvlyrics.sqlite3")).map_err(to_string)?;
        Ok(Self {
            repo: Mutex::new(repo),
        })
    }

    pub fn asr_model_size(&self) -> AsrModelSize {
        self.repo
            .lock()
            .ok()
            .and_then(|repo| repo.get_user_setting(ASR_MODEL_SETTING).ok())
            .flatten()
            .map(|value| AsrModelSize::from_storage(&value))
            .unwrap_or_default()
    }

    pub fn set_asr_model_size(&self, size: AsrModelSize) -> Result<(), String> {
        let repo = self.repo.lock().map_err(to_string)?;
        repo.set_user_setting(ASR_MODEL_SETTING, size.as_storage())
            .map_err(to_string)
    }

    pub fn effective_asr_model(&self) -> AsrModelSize {
        effective_asr_model(self.asr_model_size())
    }

    pub fn resolve_video_metadata(&self, url: &str) -> Result<VideoMetadata, String> {
        video::resolve_video_metadata_inner(url).map_err(to_string)
    }

    pub fn list_video_formats(&self, url: &str) -> Result<Vec<VideoFormat>, String> {
        video::list_video_formats_inner(url).map_err(to_string)
    }

    pub fn resolve_stream(&self, url: &str, format_id: Option<&str>) -> Result<StreamSpec, String> {
        video::resolve_stream_spec_inner(url, format_id).map_err(to_string)
    }

    pub fn fetch_lyrics(&self, query: &str) -> Result<SongPackage, String> {
        let providers: Vec<Box<dyn LyricsProvider>> = vec![
            Box::new(ColorCodedLyricsProvider::default()),
            Box::new(GeniusProvider::default()),
        ];

        let mut last_error = None;
        for provider in providers {
            match provider.fetch(query) {
                Ok(mut package) => {
                    let mut repo = self.repo.lock().map_err(to_string)?;
                    repo.upsert_song_package(&mut package).map_err(to_string)?;
                    return Ok(package);
                }
                Err(err) => last_error = Some(err.to_string()),
            }
        }

        Err(last_error.unwrap_or_else(|| "No lyric providers configured".to_string()))
    }

    pub fn import_lyrics(
        &self,
        raw_text: &str,
        title: &str,
        artist: &str,
    ) -> Result<SongPackage, String> {
        let mut package =
            crate::lyrics::parse_manual_lyrics(raw_text, title, artist).map_err(to_string)?;
        let mut repo = self.repo.lock().map_err(to_string)?;
        repo.upsert_song_package(&mut package).map_err(to_string)?;
        Ok(package)
    }

    pub fn fetch_captions(&self, video_id: &str) -> Result<Vec<CaptionLine>, String> {
        let provider = YouTubeCaptionProvider::default();
        let captions = provider.fetch(video_id).map_err(to_string)?;
        let mut repo = self.repo.lock().map_err(to_string)?;
        repo.upsert_caption_lines(video_id, &captions)
            .map_err(to_string)?;
        Ok(captions)
    }

    pub fn import_captions(
        &self,
        video_id: &str,
        raw_text: &str,
    ) -> Result<Vec<CaptionLine>, String> {
        let captions = parse_caption_text(raw_text).map_err(to_string)?;
        let mut repo = self.repo.lock().map_err(to_string)?;
        repo.upsert_caption_lines(video_id, &captions)
            .map_err(to_string)?;
        Ok(captions)
    }

    pub fn align_lyrics(
        &self,
        song_id: i64,
        video_id: &str,
    ) -> Result<AlignResult, String> {
        self.align_lyrics_with_progress(song_id, video_id, |_| {})
    }

    pub fn align_lyrics_with_progress(
        &self,
        song_id: i64,
        video_id: &str,
        mut report_progress: impl FnMut(f64),
    ) -> Result<AlignResult, String> {
        verbose(format!("align song_id={song_id} video_id={video_id}"));
        let lyrics = {
            let _phase = PhaseGuard::begin("align load lyric_lines");
            let repo = self.repo.lock().map_err(to_string)?;
            repo.lyric_lines(song_id).map_err(to_string)?
        };
        verbose(format!("align lyric count={}", lyrics.len()));

        report_progress(0.70);
        progress("align fetch captions", 0.70);
        let provider = YouTubeCaptionProvider::default();
        let track_sets = {
            let _phase = PhaseGuard::begin("align fetch_all captions");
            match provider.fetch_all(video_id) {
                Ok(tracks) => tracks,
                Err(err) => {
                    verbose(format!("align caption fetch failed: {err}; trying cache"));
                    let repo = self.repo.lock().map_err(to_string)?;
                    let cached = repo.caption_lines(video_id).map_err(to_string)?;
                    if cached.is_empty() {
                        return Err(err.to_string());
                    }
                    verbose(format!("align using {} cached caption lines", cached.len()));
                    vec![crate::captions::CaptionTrackSet {
                        language_code: String::new(),
                        auto_generated: false,
                        label: "imported".into(),
                        lines: cached,
                    }]
                }
            }
        };
        verbose(format!("align caption tracks={}", track_sets.len()));
        let (has_original, has_romanization, has_english) = lyric_language_toggles(&lyrics);
        let prefer_asr =
            has_original || (has_english && !has_original && !has_romanization);
        let asr_use_original = has_original;
        let asr_language = if has_original {
            Some(detect_original_language(&lyrics).unwrap_or("ko"))
        } else if has_english && !has_original && !has_romanization {
            Some("en")
        } else {
            None
        };
        verbose(format!(
            "align languages original={has_original} roman={has_romanization} english={has_english} prefer_asr={prefer_asr} asr_lang={asr_language:?}"
        ));
        let mut summary = String::from("Aligned from YouTube captions");

        let mut best_alignment = Vec::new();
        let mut best_captions = Vec::new();
        let mut best_score = f64::NEG_INFINITY;

        report_progress(0.74);
        progress("align caption tracks", 0.74);
        {
            let _phase = PhaseGuard::begin("align caption track scoring");
            if let Some((score, aligned, captions)) = track_sets
                .par_iter()
                .map(|track| {
                    verbose(format!(
                        "align scoring track label={} lines={}",
                        track.label,
                        track.lines.len()
                    ));
                    let aligned = align_lines(AlignmentInput {
                        lyrics: lyrics.clone(),
                        captions: track.lines.clone(),
                    });
                    let score = alignment_playback_quality(&aligned);
                    (score, aligned, track.lines.clone())
                })
                .max_by(|left, right| {
                    left.0
                        .partial_cmp(&right.0)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
            {
                best_score = score;
                best_alignment = aligned;
                best_captions = captions;
            }
        }
        verbose(format!(
            "align best caption score={best_score:.3} lines={}",
            best_alignment.len()
        ));

        if prefer_asr {
            report_progress(0.78);
            progress("align asr branch", 0.78);
            let _phase = PhaseGuard::begin("align asr_available check");
            let asr_ok = asr_available();
            verbose(format!("align asr_available={asr_ok}"));
            drop(_phase);
            if asr_ok {
                let model_size = self.effective_asr_model();
                verbose(format!("align asr model={}", model_size.model_filename()));
                let primary_align_language =
                    forced_align_language(asr_use_original, asr_language);
                let (lyrics_text, forced_lines) =
                    crate::align::build_lyrics_forced_alignment_bundle_with_hints(
                        &lyrics,
                        asr_use_original,
                        &best_alignment,
                    );
                let transcript = {
                    let _phase = PhaseGuard::begin("align transcribe_video");
                    transcribe_video(
                        video_id,
                        asr_language,
                        model_size,
                        Some(&lyrics_text),
                        Some(&forced_lines),
                        Some(primary_align_language),
                    )
                };
                match transcript {
                    Ok(transcript) if !transcript.words.is_empty() => {
                        verbose(format!(
                            "align asr words={} source={:?} line_timings={}",
                            transcript.words.len(),
                            transcript.alignment_source,
                            transcript.line_timings.len()
                        ));
                        let asr_captions = asr_caption_lines(video_id, &transcript);
                        let mut asr_alignment = alignment_from_transcript(
                            &lyrics,
                            &transcript,
                            asr_use_original,
                        );
                        merge_caption_baseline(&mut asr_alignment, &best_alignment);
                        let asr_score = alignment_playback_quality(&asr_alignment);
                        let timing_quality = forced_line_timing_quality(&asr_alignment);
                        verbose(format!(
                            "align asr score={asr_score:.3} timing_quality={timing_quality:.3} caption_score={best_score:.3}"
                        ));
                        if timing_quality > 0.0 && asr_score >= best_score {
                            best_alignment = asr_alignment;
                            best_captions = asr_captions;
                            let backend = transcript
                                .backend
                                .as_deref()
                                .unwrap_or(model_size.backend());
                            summary = format!(
                                "Aligned with Qwen3 ASR ({}, {} words, {})",
                                backend,
                                transcript.words.len(),
                                runtime_device_label(&transcript)
                            );
                            eprintln!("kpopmvlyrics: {summary}");
                        } else {
                            summary = format!(
                                "Qwen3 ASR alignment quality={timing_quality:.2} score={asr_score:.2} \
                                 was not better than captions (score={best_score:.2}); kept caption alignment"
                            );
                            eprintln!("kpopmvlyrics: {summary}");
                        }
                    }
                    Ok(_) => {
                        summary = "Qwen3 ASR returned no words; kept YouTube caption alignment"
                            .into();
                        eprintln!("kpopmvlyrics: {summary}");
                    }
                    Err(err) => {
                        summary = format!("Qwen3 ASR failed ({err}); kept YouTube caption alignment");
                        eprintln!("kpopmvlyrics: {summary}");
                    }
                }
            } else {
                summary = format!(
                    "Qwen3 ASR unavailable ({}); kept YouTube caption alignment",
                    asr_setup_hint()
                );
                eprintln!("kpopmvlyrics: {summary}");
            }
        }

        report_progress(0.84);
        progress("align persist", 0.84);
        if best_alignment.is_empty() {
            return Err("No caption tracks available for alignment".into());
        }

        {
            let _phase = PhaseGuard::begin("align upsert db");
            let mut repo = self.repo.lock().map_err(to_string)?;
            repo.upsert_caption_lines(video_id, &best_captions)
                .map_err(to_string)?;
            repo.upsert_alignment(song_id, video_id, &best_alignment)
                .map_err(to_string)?;
        }
        Ok(AlignResult {
            alignment: best_alignment,
            captions: best_captions,
            summary,
        })
    }

    pub fn save_alignment_edits(
        &self,
        song_id: i64,
        video_id: &str,
        lines: &[AlignmentLine],
    ) -> Result<(), String> {
        let mut repo = self.repo.lock().map_err(to_string)?;
        repo.upsert_alignment(song_id, video_id, lines)
            .map_err(to_string)
    }

    pub fn load_playback_cache(
        &self,
        song_id: i64,
        video_id: &str,
        lyric_count: usize,
    ) -> Option<(Vec<AlignmentLine>, Vec<CaptionLine>)> {
        let repo = self.repo.lock().ok()?;
        let alignment = repo.alignment_lines(song_id, video_id).ok()?;
        if alignment.is_empty() || alignment.len() != lyric_count {
            return None;
        }
        let captions = repo.caption_lines(video_id).ok()?;
        if captions.is_empty() {
            return None;
        }
        Some((alignment, captions))
    }

    pub fn search_member_profiles(&self, group_name: &str) -> Result<Vec<MemberProfile>, String> {
        let providers: Vec<Box<dyn MemberProfileProvider>> = vec![
            Box::new(KpoppingProvider::default()),
            Box::new(KpopFandomProvider::default()),
        ];
        let mut profiles = Vec::new();
        for provider in providers {
            if let Ok(mut found) = provider.search(group_name) {
                profiles.append(&mut found);
            }
        }
        profiles.sort_by(|a, b| a.stage_name.cmp(&b.stage_name));
        profiles.dedup_by(|a, b| a.stage_name.eq_ignore_ascii_case(&b.stage_name));

        let mut repo = self.repo.lock().map_err(to_string)?;
        repo.upsert_members(group_name, &profiles)
            .map_err(to_string)?;
        Ok(profiles)
    }

    pub fn save_member_override(
        &self,
        group_name: &str,
        member: &MemberProfile,
    ) -> Result<MemberProfile, String> {
        let mut repo = self.repo.lock().map_err(to_string)?;
        repo.save_member_override(group_name, member)
            .map_err(to_string)?;
        Ok(member.clone())
    }
}

/// Fill in caption-based timings for any lyric line that ASR left unsynced.
/// FA can fail on individual chunks (degenerate model output); the caption
/// baseline keeps those lines highlighted at approximately the right time.
fn merge_caption_baseline(asr: &mut [AlignmentLine], baseline: &[AlignmentLine]) {
    use std::collections::HashMap;
    let baseline_by_index: HashMap<usize, &AlignmentLine> =
        baseline.iter().map(|line| (line.lyric_index, line)).collect();
    for line in asr.iter_mut() {
        if crate::align::is_synced_line(line) {
            continue;
        }
        let Some(fallback) = baseline_by_index.get(&line.lyric_index) else {
            continue;
        };
        if !crate::align::is_synced_line(fallback) {
            continue;
        }
        line.caption_index = fallback.caption_index;
        line.start_ms = fallback.start_ms;
        line.end_ms = fallback.end_ms;
        line.confidence = (fallback.confidence * 0.6).max(0.1);
        line.needs_review = true;
    }
}

fn alignment_from_transcript(
    lyrics: &[LyricLine],
    transcript: &AsrTranscript,
    use_original: bool,
) -> Vec<AlignmentLine> {
    if !transcript.line_timings.is_empty() {
        align_lyrics_from_line_timings(lyrics, &transcript.line_timings)
    } else if transcript.alignment_source.as_deref() == Some("lyrics") {
        align_lyrics_to_forced_words(lyrics, &transcript.words, use_original)
    } else {
        align_lyrics_to_timed_words(lyrics, &transcript.words, use_original)
    }
}

/// Pick a BCP-47 language code based on the dominant script of the original lyrics.
/// Returns None for Latin-only originals (caller falls back to a configured default).
fn detect_original_language(lyrics: &[LyricLine]) -> Option<&'static str> {
    let mut hangul = 0usize;
    let mut kana = 0usize;
    let mut han = 0usize;
    for line in lyrics {
        for ch in line.original.chars() {
            let cp = ch as u32;
            match cp {
                0xAC00..=0xD7AF | 0x1100..=0x11FF | 0x3130..=0x318F => hangul += 1,
                0x3040..=0x309F | 0x30A0..=0x30FF => kana += 1,
                0x4E00..=0x9FFF | 0x3400..=0x4DBF => han += 1,
                _ => {}
            }
        }
    }
    let max = hangul.max(kana).max(han);
    if max == 0 {
        return None;
    }
    if max == hangul {
        Some("ko")
    } else if max == kana {
        Some("ja")
    } else {
        Some("zh")
    }
}

fn runtime_device_label(transcript: &AsrTranscript) -> &'static str {
    match transcript.device.as_deref() {
        Some(device) if device.eq_ignore_ascii_case("cuda") => "cuda",
        Some(device) if device.eq_ignore_ascii_case("cpu") => "cpu",
        Some(_) => "auto",
        None => "auto",
    }
}

pub fn shift_alignment(lines: &[AlignmentLine], delta: i64) -> Vec<AlignmentLine> {
    lines
        .iter()
        .map(|line| AlignmentLine {
            start_ms: (line.start_ms + delta).max(0),
            end_ms: (line.end_ms + delta).max(0),
            needs_review: true,
            ..line.clone()
        })
        .collect()
}

pub const DEFAULT_MANUAL_LYRICS: &str = "Nayeon: Tell me what you want\nMomo: Tell me what you need\nSana: A to Z da malhaebwa";

pub const DEFAULT_MANUAL_CAPTIONS: &str = "WEBVTT\n\n00:00:01.000 --> 00:00:02.400\nTell me what you want\n\n00:00:02.500 --> 00:00:03.900\nTell me what you need\n\n00:00:04.000 --> 00:00:05.600\nA to Z da malhaebwa";

pub fn query_from_metadata(metadata: &VideoMetadata) -> String {
    clean_video_title(metadata.title.as_deref().unwrap_or(&metadata.original_url))
}

pub fn clean_video_title(title: &str) -> String {
    let cleaned = title
        .replace(" - YouTube", "")
        .replace(" - youtube", "");
    let cleaned = regex::Regex::new(r"(?i)\s*\[[^\]]*(official|mv|m/v|music video)[^\]]*\]\s*")
        .ok()
        .map(|re| re.replace_all(&cleaned, " ").to_string())
        .unwrap_or(cleaned);
    let cleaned = regex::Regex::new(r"(?i)\s*\((official\s*)?(mv|m/v|music video|official video)\)\s*")
        .ok()
        .map(|re| re.replace_all(&cleaned, " ").to_string())
        .unwrap_or(cleaned);
    let cleaned = regex::Regex::new(r"(?i)\s+(official\s*)?(mv|m/v|music video|official video)$")
        .ok()
        .map(|re| re.replace_all(&cleaned, "").to_string())
        .unwrap_or(cleaned);
    let cleaned = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if let Some(caps) = regex::Regex::new(r#"^(.*?)\s+["“'‘]([^"”'’]+)["”'’]"#)
        .ok()
        .and_then(|re| re.captures(&cleaned))
    {
        return format!("{} - {}", &caps[1], &caps[2])
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
    }
    cleaned
}

pub fn merge_members(primary: &[MemberProfile], secondary: &[MemberProfile]) -> Vec<MemberProfile> {
    if primary.is_empty() {
        return Vec::new();
    }
    let mut by_name: std::collections::HashMap<String, MemberProfile> = primary
        .iter()
        .map(|member| (member.stage_name.to_lowercase(), member.clone()))
        .collect();
    for member in secondary {
        let key = member.stage_name.to_lowercase();
        let existing = by_name.get(&key).cloned().or_else(|| {
            by_name
                .values()
                .find(|item| names_match(&item.stage_name, &member.stage_name))
                .cloned()
        });
        if let Some(existing) = existing {
            let stage_name = existing.stage_name.clone();
            let merged = MemberProfile {
                image_url: member.image_url.clone().or(existing.image_url.clone()),
                local_image_path: member
                    .local_image_path
                    .clone()
                    .or(existing.local_image_path.clone()),
                real_name: existing.real_name.clone().or(member.real_name.clone()),
                ..existing
            };
            by_name.insert(stage_name.to_lowercase(), merged);
        }
    }
    by_name.into_values().collect()
}

pub fn apply_member_profiles(
    members: &[MemberProfile],
    profiles: &[MemberProfile],
    lines: &[LyricLine],
) -> Vec<MemberProfile> {
    let merged = merge_members(members, profiles);
    restrict_members_to_lines(&merged, lines)
}

pub fn restrict_members_to_lines(
    members: &[MemberProfile],
    lines: &[LyricLine],
) -> Vec<MemberProfile> {
    let referenced = crate::lyrics::canonical_referenced_members(lines, members);
    if referenced.is_empty() {
        return members.to_vec();
    }
    members
        .iter()
        .filter(|member| {
            referenced
                .iter()
                .any(|name| name.eq_ignore_ascii_case(&member.stage_name))
        })
        .cloned()
        .collect()
}

pub fn format_ms(ms: i64) -> String {
    let safe = ms.max(0);
    let minutes = safe / 60000;
    let seconds = (safe % 60000) / 1000;
    let millis = safe % 1000;
    format!("{minutes}:{seconds:02}.{millis:03}")
}

fn names_match(left: &str, right: &str) -> bool {
    fn normalize(value: &str) -> String {
        value
            .to_lowercase()
            .replace("kim ", "")
            .replace("huh ", "")
            .replace("hong ", "")
            .replace("miyawaki ", "")
            .replace("nakamura ", "")
            .chars()
            .filter(|ch| ch.is_ascii_alphabetic())
            .collect()
    }
    let a = normalize(left);
    let b = normalize(right);
    !a.is_empty() && !b.is_empty() && (a == b || a.contains(&b) || b.contains(&a))
}

fn app_data_dir() -> Result<PathBuf, String> {
    dirs::data_dir()
        .map(|path| path.join("kpopmvlyrics"))
        .ok_or_else(|| "Could not resolve application data directory".to_string())
}

fn to_string<E: std::fmt::Display>(err: E) -> String {
    err.to_string()
}

#[cfg(test)]
mod tests {
    use super::{
        apply_member_profiles, detect_original_language, merge_members, restrict_members_to_lines,
    };
    use crate::models::{LyricLine, MemberProfile};

    fn lyric_with_original(text: &str) -> LyricLine {
        LyricLine {
            id: None,
            song_id: None,
            index: 0,
            member: None,
            original: text.into(),
            romanization: None,
            english: None,
            with_all: false,
            segments: Vec::new(),
        }
    }

    #[test]
    fn detect_original_language_picks_korean_for_hangul() {
        let lyrics = vec![
            lyric_with_original("이건 서울 City"),
            lyric_with_original("수많은 기적이 이뤄진 곳"),
        ];
        assert_eq!(detect_original_language(&lyrics), Some("ko"));
    }

    #[test]
    fn detect_original_language_picks_japanese_for_kana() {
        let lyrics = vec![lyric_with_original("こんにちは 世界")];
        assert_eq!(detect_original_language(&lyrics), Some("ja"));
    }

    #[test]
    fn detect_original_language_returns_none_for_latin_only() {
        let lyrics = vec![lyric_with_original("Tell me what you want")];
        assert_eq!(detect_original_language(&lyrics), None);
    }

    fn profile(stage_name: &str) -> MemberProfile {
        MemberProfile {
            id: None,
            stage_name: stage_name.to_string(),
            real_name: None,
            color: "#e84855".to_string(),
            image_url: None,
            local_image_path: None,
            provider: None,
        }
    }

    #[test]
    fn merge_members_does_not_dump_full_group_roster_when_lyrics_are_untagged() {
        let profiles = vec![profile("Bang Chan"), profile("Woojin")];
        assert!(merge_members(&[], &profiles).is_empty());
    }

    #[test]
    fn apply_member_profiles_keeps_only_members_referenced_in_lyrics() {
        let lines = vec![LyricLine {
            id: None,
            song_id: None,
            index: 0,
            member: Some("Felix".into()),
            original: "line".into(),
            romanization: None,
            english: None,
            with_all: false,
            segments: Vec::new(),
        }];
        let members = vec![profile("Felix")];
        let profiles = vec![
            MemberProfile {
                image_url: Some("https://example.test/felix.jpg".into()),
                ..profile("Felix")
            },
            MemberProfile {
                image_url: Some("https://example.test/woojin.jpg".into()),
                ..profile("Woojin")
            },
        ];
        let merged = apply_member_profiles(&members, &profiles, &lines);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].stage_name, "Felix");
        assert_eq!(
            restrict_members_to_lines(&profiles, &lines)
                .iter()
                .map(|member| member.stage_name.as_str())
                .collect::<Vec<_>>(),
            vec!["Felix"]
        );
    }

    #[test]
    fn restrict_members_to_lines_resolves_abbreviated_singer_tags() {
        let lines = vec![LyricLine {
            id: None,
            song_id: None,
            index: 0,
            member: Some("H, FL".into()),
            original: "line".into(),
            romanization: None,
            english: None,
            with_all: false,
            segments: Vec::new(),
        }];
        let profiles = vec![profile("HAN"), profile("Felix"), profile("Woojin")];
        let filtered = restrict_members_to_lines(&profiles, &lines);
        assert_eq!(
            filtered
                .iter()
                .map(|member| member.stage_name.as_str())
                .collect::<Vec<_>>(),
            vec!["HAN", "Felix"]
        );
    }
}
