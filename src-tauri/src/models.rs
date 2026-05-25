use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct VideoMetadata {
    pub video_id: String,
    pub title: Option<String>,
    pub artist_hint: Option<String>,
    pub original_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct VideoFormat {
    pub format_id: String,
    pub label: String,
    pub height: Option<u32>,
    pub ext: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct VideoDownloadProgress {
    pub percent: f32,
    pub status: String,
    pub active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SongPackage {
    pub song: Song,
    pub lines: Vec<LyricLine>,
    pub members: Vec<MemberProfile>,
    pub provider: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Song {
    pub id: Option<i64>,
    pub title: String,
    pub artist: String,
    pub group_name: Option<String>,
    pub source_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LyricLine {
    pub id: Option<i64>,
    pub song_id: Option<i64>,
    pub index: usize,
    pub member: Option<String>,
    pub original: String,
    pub romanization: Option<String>,
    pub english: Option<String>,
    #[serde(default)]
    pub segments: Vec<LyricSegment>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LyricSegment {
    pub language: String,
    pub text: String,
    pub member: Option<String>,
    pub color: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CaptionLine {
    pub id: Option<i64>,
    pub video_id: String,
    pub index: usize,
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AlignmentLine {
    pub lyric_index: usize,
    pub caption_index: Option<usize>,
    pub start_ms: i64,
    pub end_ms: i64,
    pub confidence: f32,
    pub needs_review: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MemberProfile {
    pub id: Option<i64>,
    pub stage_name: String,
    pub real_name: Option<String>,
    pub color: String,
    pub image_url: Option<String>,
    pub local_image_path: Option<String>,
    pub provider: Option<String>,
}
