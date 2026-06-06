//! Canonical lyric-export JSON shape, shared by every frontend.
//!
//! Centralising this here keeps the GTK4 editor, the macOS UniFFI/SwiftUI app,
//! and the (legacy) TypeScript webview from drifting apart — each calls the same
//! function rather than re-implementing the shape per platform.

use serde_json::{json, Value};

use crate::models::{AlignmentLine, SongPackage, VideoMetadata};

/// Build the export payload for a resolved video + song + alignment.
pub fn build_export_json(
    metadata: &VideoMetadata,
    song: &SongPackage,
    alignment: &[AlignmentLine],
) -> Value {
    json!({
        "version": 1,
        "video": {
            "platform": platform_from_url(&metadata.original_url),
            "videoId": metadata.video_id,
            "url": metadata.original_url,
        },
        "members": song.members.iter().map(|member| {
            json!({
                "name": member.stage_name,
                "color": member.color,
                "imageUrl": member.image_url,
                "localImagePath": member.local_image_path,
            })
        }).collect::<Vec<_>>(),
        "lyrics": song.lines.iter().map(|line| {
            let timing = alignment.iter().find(|item| item.lyric_index == line.index);
            let mut lyric = json!({
                "index": line.index,
                "startMs": timing.map(|item| item.start_ms),
                "endMs": timing.map(|item| item.end_ms),
                "layer": line.layer.as_str(),
                "member": line.member,
                "original": line.original,
            });
            if let Some(romanization) = &line.romanization {
                lyric["romanization"] = json!(romanization);
            }
            if let Some(english) = &line.english {
                lyric["english"] = json!(english);
            }
            lyric
        }).collect::<Vec<_>>(),
    })
}

/// Short platform tag derived from the source URL host.
pub fn platform_from_url(raw_url: &str) -> String {
    url::Url::parse(raw_url)
        .ok()
        .and_then(|url| {
            url.host_str()
                .map(|host| host.trim_start_matches("www.").to_string())
        })
        .map(|host| {
            if host == "youtube.com" || host == "youtu.be" || host.ends_with(".youtube.com") {
                "youtube".to_string()
            } else {
                host
            }
        })
        .unwrap_or_else(|| "unknown".to_string())
}
