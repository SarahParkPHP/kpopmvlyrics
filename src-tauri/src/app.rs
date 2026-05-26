use std::path::PathBuf;
use std::sync::Mutex;

use crate::align::{
    align_lines, align_lyrics_to_whisper_words, alignment_playback_quality, AlignmentInput,
};
use crate::lyrics::lyric_language_toggles;
use crate::whisper::{whisper_available, whisper_caption_lines, whisper_setup_hint, transcribe_video};
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

impl AppContext {
    pub fn open() -> Result<Self, String> {
        let app_dir = app_data_dir()?;
        std::fs::create_dir_all(&app_dir).map_err(to_string)?;
        let repo = Repository::open(app_dir.join("kpopmvlyrics.sqlite3")).map_err(to_string)?;
        Ok(Self {
            repo: Mutex::new(repo),
        })
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
        let lyrics = {
            let repo = self.repo.lock().map_err(to_string)?;
            repo.lyric_lines(song_id).map_err(to_string)?
        };

        let provider = YouTubeCaptionProvider::default();
        let track_sets = match provider.fetch_all(video_id) {
            Ok(tracks) => tracks,
            Err(err) => {
                let repo = self.repo.lock().map_err(to_string)?;
                let cached = repo.caption_lines(video_id).map_err(to_string)?;
                if cached.is_empty() {
                    return Err(err.to_string());
                }
                vec![crate::captions::CaptionTrackSet {
                    language_code: String::new(),
                    auto_generated: false,
                    label: "imported".into(),
                    lines: cached,
                }]
            }
        };
        let (has_original, has_romanization, has_english) = lyric_language_toggles(&lyrics);
        let prefer_whisper = has_english && !has_original && !has_romanization;
        let mut summary = String::from("Aligned from YouTube captions");

        let mut best_alignment = Vec::new();
        let mut best_captions = Vec::new();
        let mut best_score = f64::NEG_INFINITY;

        for track in &track_sets {
            let aligned = align_lines(AlignmentInput {
                lyrics: lyrics.clone(),
                captions: track.lines.clone(),
            });
            let score = alignment_playback_quality(&aligned);
            if score > best_score {
                best_score = score;
                best_alignment = aligned;
                best_captions = track.lines.clone();
            }
        }

        if prefer_whisper {
            if whisper_available() {
                match transcribe_video(video_id, Some("en")) {
                    Ok(transcript) if !transcript.words.is_empty() => {
                        let whisper_captions = whisper_caption_lines(video_id, &transcript);
                        let whisper_alignment =
                            align_lyrics_to_whisper_words(&lyrics, &transcript.words);
                        best_alignment = whisper_alignment;
                        best_captions = whisper_captions;
                        let device = transcript.device.as_deref().unwrap_or("cpu");
                        summary = format!(
                            "Aligned with Whisper ({}, {} words)",
                            device,
                            transcript.words.len()
                        );
                        eprintln!("kpopmvlyrics: {summary}");
                    }
                    Ok(_) => {
                        summary = "Whisper returned no words; kept YouTube caption alignment"
                            .into();
                        eprintln!("kpopmvlyrics: {summary}");
                    }
                    Err(err) => {
                        summary = format!("Whisper failed ({err}); kept YouTube caption alignment");
                        eprintln!("kpopmvlyrics: {summary}");
                    }
                }
            } else {
                summary = format!(
                    "Whisper unavailable ({}); kept YouTube caption alignment",
                    whisper_setup_hint()
                );
                eprintln!("kpopmvlyrics: {summary}");
            }
        }

        if best_alignment.is_empty() {
            return Err("No caption tracks available for alignment".into());
        }

        let mut repo = self.repo.lock().map_err(to_string)?;
        repo.upsert_caption_lines(video_id, &best_captions)
            .map_err(to_string)?;
        repo.upsert_alignment(song_id, video_id, &best_alignment)
            .map_err(to_string)?;
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
    use super::{apply_member_profiles, merge_members, restrict_members_to_lines};
    use crate::models::{LyricLine, MemberProfile};

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
