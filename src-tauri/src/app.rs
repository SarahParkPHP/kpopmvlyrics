use std::path::PathBuf;
use std::sync::Mutex;

use crate::align::{
    align_lines, align_lyrics_from_line_timings, align_lyrics_to_forced_words,
    align_lyrics_to_timed_words, alignment_accuracy, alignment_playback_quality, AlignmentInput,
};
use crate::asr::{
    asr_available, asr_caption_lines, asr_setup_hint, effective_asr_model, forced_align_language,
    transcribe_video, AsrModelSize, AsrTranscript,
};
use crate::audio_visual;
use crate::captions::{parse_caption_text, CaptionProvider, YouTubeCaptionProvider};
use crate::db::Repository;
use crate::log::{progress, verbose, PhaseGuard};
use crate::lyrics::lyric_language_toggles;
use crate::lyrics::{
    ColorCodedHeavenProvider, ColorCodedLyricsProvider, GeniusProvider, LyricsProvider,
    LYRICS_CONFIDENT_SCORE,
};
use crate::members::{KpopFandomProvider, KpoppingProvider, MemberProfileProvider};
use crate::models::*;
use crate::video;
use rayon::prelude::*;

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
const ASR_DEMUCS_SETTING: &str = "asr_demucs_enabled";
/// Below this 0..1 alignment accuracy, a Demucs-enabled run is retried without
/// Demucs (the source/instrumental separation can hurt some mixes), keeping
/// whichever attempt scores higher.
const ACCURACY_RETRY_THRESHOLD: f64 = 0.6;
const ASR_API_KEY_PREFIX: &str = "asr_api_key_";
const ASR_BASE_URL_PREFIX: &str = "asr_base_url_";
const THEME_SETTING: &str = "gtk_theme";

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

    pub fn asr_demucs_enabled(&self) -> bool {
        if let Ok(value) = std::env::var("KPOPMVLYRICS_ASR_DEMUCS") {
            return setting_is_enabled(&value);
        }
        self.user_setting(ASR_DEMUCS_SETTING)
            .is_some_and(|value| setting_is_enabled(&value))
    }

    pub fn set_asr_demucs_enabled(&self, enabled: bool) -> Result<(), String> {
        self.set_user_setting(ASR_DEMUCS_SETTING, if enabled { "1" } else { "0" })
    }

    pub fn asr_api_key(&self, provider: &str) -> String {
        self.user_setting(&format!("{ASR_API_KEY_PREFIX}{provider}"))
            .unwrap_or_default()
    }

    pub fn set_asr_api_key(&self, provider: &str, value: &str) -> Result<(), String> {
        self.set_user_setting(&format!("{ASR_API_KEY_PREFIX}{provider}"), value)
    }

    pub fn asr_base_url(&self, provider: &str) -> String {
        self.user_setting(&format!("{ASR_BASE_URL_PREFIX}{provider}"))
            .unwrap_or_default()
    }

    pub fn set_asr_base_url(&self, provider: &str, value: &str) -> Result<(), String> {
        self.set_user_setting(&format!("{ASR_BASE_URL_PREFIX}{provider}"), value)
    }

    pub fn theme_preference(&self) -> String {
        self.user_setting(THEME_SETTING)
            .unwrap_or_else(|| "system".to_string())
    }

    pub fn set_theme_preference(&self, value: &str) -> Result<(), String> {
        self.set_user_setting(THEME_SETTING, value)
    }

    fn user_setting(&self, key: &str) -> Option<String> {
        self.repo
            .lock()
            .ok()
            .and_then(|repo| repo.get_user_setting(key).ok())
            .flatten()
    }

    fn set_user_setting(&self, key: &str, value: &str) -> Result<(), String> {
        let repo = self.repo.lock().map_err(to_string)?;
        repo.set_user_setting(key, value).map_err(to_string)
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

    pub fn build_timeline_spectrogram(
        &self,
        video_id: &str,
        url: &str,
    ) -> Result<AudioSpectrogram, String> {
        audio_visual::build_timeline_spectrogram(video_id, url).map_err(to_string)
    }

    pub fn fetch_lyrics(&self, query: &str) -> Result<SongPackage, String> {
        // Different color-coded sites cover different songs (and the same query can
        // produce a confidently-wrong match on one site), so we score every
        // color-coded result and keep the best instead of taking the first hit.
        // Genius is a lower-confidence fallback consulted only when no color-coded
        // source is confident.
        let color_sites: Vec<Box<dyn LyricsProvider>> = vec![
            Box::new(ColorCodedLyricsProvider::default()),
            Box::new(ColorCodedHeavenProvider::default()),
        ];

        let mut best: Option<(f64, SongPackage)> = None;
        let mut last_error = None;

        for provider in color_sites {
            match provider.fetch_scored(query) {
                Ok((score, package)) => {
                    verbose(format!("fetch_lyrics {} score={score:.3}", package.provider));
                    consider_lyrics_candidate(&mut best, score, package);
                    if best
                        .as_ref()
                        .map(|(score, _)| *score >= LYRICS_CONFIDENT_SCORE)
                        .unwrap_or(false)
                    {
                        break;
                    }
                }
                Err(err) => last_error = Some(err.to_string()),
            }
        }

        let confident = best
            .as_ref()
            .map(|(score, _)| *score >= LYRICS_CONFIDENT_SCORE)
            .unwrap_or(false);
        if !confident {
            match GeniusProvider::default().fetch_scored(query) {
                Ok((score, package)) => {
                    verbose(format!("fetch_lyrics genius score={score:.3}"));
                    consider_lyrics_candidate(&mut best, score, package);
                }
                Err(err) => last_error = Some(err.to_string()),
            }
        }

        let (_, mut package) = best.ok_or_else(|| {
            last_error.unwrap_or_else(|| "No lyric providers configured".to_string())
        })?;
        let mut repo = self.repo.lock().map_err(to_string)?;
        repo.upsert_song_package(&mut package).map_err(to_string)?;
        Ok(package)
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

    pub fn align_lyrics(&self, song_id: i64, video_id: &str) -> Result<AlignResult, String> {
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
        let prefer_asr = has_original || (has_english && !has_original && !has_romanization);
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

        let configured_model = self.effective_asr_model();
        if prefer_asr && configured_model.is_enabled() {
            report_progress(0.78);
            progress("align asr branch", 0.78);
            let asr_ok = if configured_model.is_local() {
                let _phase = PhaseGuard::begin("align asr_available check");
                let available = asr_available();
                verbose(format!("align asr_available={available}"));
                drop(_phase);
                available
            } else {
                true
            };
            if asr_ok {
                let model_size = configured_model;
                verbose(format!("align asr model={}", model_size.model_filename()));
                let provider = model_size.provider_id();
                let api_key = provider
                    .map(|provider| self.asr_api_key(provider))
                    .unwrap_or_default();
                let base_url = provider
                    .map(|provider| self.asr_base_url(provider))
                    .unwrap_or_default();
                if provider.is_some() && api_key.trim().is_empty() {
                    summary = format!(
                        "{} API key is not set; kept YouTube caption alignment",
                        model_size.backend()
                    );
                    eprintln!("kpopmvlyrics: {summary}");
                    report_progress(0.84);
                    progress("align persist", 0.84);
                    if best_alignment.is_empty() {
                        return Err("No caption tracks available for alignment".into());
                    }
                    let mut repo = self.repo.lock().map_err(to_string)?;
                    repo.upsert_caption_lines(video_id, &best_captions)
                        .map_err(to_string)?;
                    repo.upsert_alignment(song_id, video_id, &best_alignment)
                        .map_err(to_string)?;
                    return Ok(AlignResult {
                        alignment: best_alignment,
                        captions: best_captions,
                        summary,
                    });
                }
                let primary_align_language = forced_align_language(asr_use_original, asr_language);
                let (lyrics_text, forced_lines) =
                    crate::align::build_lyrics_forced_alignment_bundle_with_slices(
                        &lyrics,
                        asr_use_original,
                        &best_alignment,
                        primary_align_language,
                        // Audio length is unknown here; the Python worker clamps
                        // slice windows to the real wav length, so leave it open.
                        0,
                        crate::align::slice_buffer_ms(),
                    );

                // Score the caption-only baseline on the same 0..1 accuracy scale
                // (caption text vs lyrics) so ASR can be compared apples-to-apples.
                let caption_text = best_captions
                    .iter()
                    .map(|line| line.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" ");
                let baseline_accuracy =
                    alignment_accuracy(&lyrics, &best_alignment, &caption_text, &lyrics_text);
                verbose(format!("align caption baseline accuracy={baseline_accuracy:.3}"));

                // One ASR attempt (download → optional Demucs → Python worker →
                // map to lyric lines → merge caption fallback → score).
                let run_attempt = |demucs_enabled: bool| -> Result<AsrOutcome, String> {
                    let _phase = PhaseGuard::begin("align transcribe_video");
                    let transcript = transcribe_video(
                        video_id,
                        asr_language,
                        model_size,
                        demucs_enabled,
                        Some(&api_key),
                        Some(&base_url),
                        Some(&lyrics_text),
                        Some(&forced_lines),
                        Some(primary_align_language),
                    )
                    .map_err(|err| format!("ASR failed ({err})"))?;
                    if transcript.words.is_empty() {
                        return Err("ASR returned no timed words".into());
                    }
                    let captions = asr_caption_lines(video_id, &transcript);
                    let mut alignment =
                        alignment_from_transcript(&lyrics, &transcript, asr_use_original);
                    merge_caption_baseline(&mut alignment, &best_alignment);
                    let asr_text = transcript
                        .words
                        .iter()
                        .map(|word| word.text.as_str())
                        .collect::<Vec<_>>()
                        .join(" ");
                    let accuracy =
                        alignment_accuracy(&lyrics, &alignment, &asr_text, &lyrics_text);
                    verbose(format!(
                        "align asr attempt demucs={demucs_enabled} words={} accuracy={accuracy:.3} source={:?}",
                        transcript.words.len(),
                        transcript.alignment_source,
                    ));
                    Ok(AsrOutcome {
                        backend: transcript
                            .backend
                            .clone()
                            .unwrap_or_else(|| model_size.backend().to_string()),
                        device: runtime_device_label(&transcript),
                        words: transcript.words.len(),
                        accuracy,
                        alignment,
                        captions,
                    })
                };

                // Run the configured attempt; if Demucs was on and scored poorly,
                // retry without Demucs and keep whichever attempt is more accurate.
                let demucs_first = self.asr_demucs_enabled();
                let chosen: Option<AsrOutcome> = {
                    match run_attempt(demucs_first) {
                        Ok(first) => {
                            if demucs_first && first.accuracy < ACCURACY_RETRY_THRESHOLD {
                                verbose(format!(
                                    "align demucs accuracy {:.3} < {:.2}; retrying without demucs",
                                    first.accuracy, ACCURACY_RETRY_THRESHOLD
                                ));
                                match run_attempt(false) {
                                    Ok(retry) if retry.accuracy > first.accuracy => {
                                        verbose(format!(
                                            "align no-demucs retry won ({:.3} > {:.3})",
                                            retry.accuracy, first.accuracy
                                        ));
                                        Some(retry)
                                    }
                                    Ok(retry) => {
                                        verbose(format!(
                                            "align kept demucs result ({:.3} >= {:.3})",
                                            first.accuracy, retry.accuracy
                                        ));
                                        Some(first)
                                    }
                                    Err(err) => {
                                        verbose(format!("align no-demucs retry failed: {err}"));
                                        Some(first)
                                    }
                                }
                            } else {
                                Some(first)
                            }
                        }
                        Err(err) if demucs_first => {
                            // The Demucs pass failed outright (e.g. Demucs not
                            // installed, or stem separation errored). Don't abandon
                            // ASR — retry once on the original audio.
                            verbose(format!(
                                "align demucs attempt failed ({err}); retrying without demucs"
                            ));
                            match run_attempt(false) {
                                Ok(retry) => Some(retry),
                                Err(retry_err) => {
                                    summary =
                                        format!("{retry_err}; kept YouTube caption alignment");
                                    eprintln!("kpopmvlyrics: {summary}");
                                    None
                                }
                            }
                        }
                        Err(err) => {
                            summary = format!("{err}; kept YouTube caption alignment");
                            eprintln!("kpopmvlyrics: {summary}");
                            None
                        }
                    }
                };

                if let Some(outcome) = chosen {
                    if outcome.accuracy >= baseline_accuracy {
                        summary = format!(
                            "Aligned with ASR ({}, {} words, {}, accuracy {:.2})",
                            outcome.backend, outcome.words, outcome.device, outcome.accuracy,
                        );
                        eprintln!("kpopmvlyrics: {summary}");
                        best_alignment = outcome.alignment;
                        best_captions = outcome.captions;
                    } else {
                        summary = format!(
                            "ASR accuracy {:.2} was not better than captions ({:.2}); \
                             kept caption alignment",
                            outcome.accuracy, baseline_accuracy,
                        );
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
        } else if prefer_asr && !configured_model.is_enabled() {
            verbose("align asr disabled by user setting; using caption alignment only");
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

    /// Persist edited lyric lines (text, member, layer, and created/deleted lines).
    /// Replaces the song's stored lines via the full upsert path, mirroring import.
    pub fn save_lyric_lines(&self, package: &mut SongPackage) -> Result<(), String> {
        let mut repo = self.repo.lock().map_err(to_string)?;
        repo.upsert_song_package(package).map_err(to_string)
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

/// Keep `best` set to the highest-scoring lyric candidate seen so far.
fn consider_lyrics_candidate(
    best: &mut Option<(f64, SongPackage)>,
    score: f64,
    package: SongPackage,
) {
    if best
        .as_ref()
        .map(|(best_score, _)| score > *best_score)
        .unwrap_or(true)
    {
        *best = Some((score, package));
    }
}

/// Result of one ASR alignment attempt, scored so attempts (Demucs vs not) and
/// the caption baseline can be compared on the same accuracy scale.
struct AsrOutcome {
    alignment: Vec<AlignmentLine>,
    captions: Vec<CaptionLine>,
    accuracy: f64,
    words: usize,
    backend: String,
    device: &'static str,
}

/// Fill in caption-based timings for any lyric line that ASR left unsynced.
/// FA can fail on individual chunks (degenerate model output); the caption
/// baseline keeps those lines highlighted at approximately the right time.
fn merge_caption_baseline(asr: &mut [AlignmentLine], baseline: &[AlignmentLine]) {
    use std::collections::HashMap;
    let baseline_by_index: HashMap<usize, &AlignmentLine> = baseline
        .iter()
        .map(|line| (line.lyric_index, line))
        .collect();
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

pub const DEFAULT_MANUAL_LYRICS: &str =
    "Nayeon: Tell me what you want\nMomo: Tell me what you need\nSana: A to Z da malhaebwa";

pub const DEFAULT_MANUAL_CAPTIONS: &str = "WEBVTT\n\n00:00:01.000 --> 00:00:02.400\nTell me what you want\n\n00:00:02.500 --> 00:00:03.900\nTell me what you need\n\n00:00:04.000 --> 00:00:05.600\nA to Z da malhaebwa";

pub fn query_from_metadata(metadata: &VideoMetadata) -> String {
    clean_video_title(metadata.title.as_deref().unwrap_or(&metadata.original_url))
}

pub fn clean_video_title(title: &str) -> String {
    let cleaned = title.replace(" - YouTube", "").replace(" - youtube", "");
    let cleaned = regex::Regex::new(r"(?i)\s*\[[^\]]*(official|mv|m/v|music video)[^\]]*\]\s*")
        .ok()
        .map(|re| re.replace_all(&cleaned, " ").to_string())
        .unwrap_or(cleaned);
    let cleaned =
        regex::Regex::new(r"(?i)\s*\((official\s*)?(mv|m/v|music video|official video)\)\s*")
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
        // Strip any separator the artist part already ends with so a title like
        // `Artist - 'Song' M/V` does not become `Artist - - Song`.
        let artist = caps[1]
            .trim()
            .trim_end_matches(|ch: char| {
                matches!(ch, '-' | '–' | '—' | ':' | '|') || ch.is_whitespace()
            })
            .trim();
        return format!("{} - {}", artist, &caps[2])
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

fn setting_is_enabled(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

#[cfg(test)]
mod tests {
    use super::{
        apply_member_profiles, clean_video_title, detect_original_language, merge_members,
        restrict_members_to_lines,
    };
    use crate::models::{LyricLine, MemberProfile};

    #[test]
    fn clean_video_title_does_not_double_separator_for_quoted_song() {
        // YouTube title with both an "Artist -" separator and a quoted song must
        // not become "Artist - - Song" (which broke lyric search).
        assert_eq!(
            clean_video_title("MEOVV(미야오) - ‘DDI RO RI’ M/V"),
            "MEOVV(미야오) - DDI RO RI"
        );
        assert_eq!(
            clean_video_title("NMIXX \"Heavy Serenade\" M/V"),
            "NMIXX - Heavy Serenade"
        );
    }

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
            layer: crate::models::LyricLayer::default(),
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
            layer: crate::models::LyricLayer::default(),
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
            layer: crate::models::LyricLayer::default(),
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

