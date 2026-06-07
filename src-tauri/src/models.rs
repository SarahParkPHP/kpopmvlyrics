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
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum StreamSpec {
    Progressive {
        uri: String,
    },
    Adaptive {
        video_uri: String,
        audio_uri: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct VideoPosition {
    pub ms: u64,
    pub duration_ms: Option<u64>,
    pub playing: bool,
    pub buffering: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AudioSpectrogram {
    pub video_id: String,
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u8>,
    #[serde(default)]
    pub waveform: Vec<u8>,
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum LyricLayer {
    #[default]
    Lead,
    Backing,
    Adlib,
}

impl LyricLayer {
    /// Timeline track order, top to bottom.
    pub const ALL: [LyricLayer; 3] = [LyricLayer::Lead, LyricLayer::Backing, LyricLayer::Adlib];

    pub fn as_str(self) -> &'static str {
        match self {
            LyricLayer::Lead => "lead",
            LyricLayer::Backing => "backing",
            LyricLayer::Adlib => "adlib",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "lead" => Some(LyricLayer::Lead),
            "backing" => Some(LyricLayer::Backing),
            "adlib" => Some(LyricLayer::Adlib),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            LyricLayer::Lead => "Lead vocals",
            LyricLayer::Backing => "Backing vocals",
            LyricLayer::Adlib => "Adlibs",
        }
    }
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
    pub with_all: bool,
    #[serde(default)]
    pub layer: LyricLayer,
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
