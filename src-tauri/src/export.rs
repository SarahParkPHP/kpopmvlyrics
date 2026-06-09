//! Canonical lyric-export JSON shape, shared by every frontend.
//!
//! Centralising this here keeps the GTK4 editor, the macOS UniFFI/SwiftUI app,
//! and the (legacy) TypeScript webview from drifting apart — each calls the same
//! function rather than re-implementing the shape per platform.

use serde_json::{json, Value};

use crate::models::{AlignmentLine, LyricLine, SongPackage, VideoMetadata};

/// Build the export payload for a resolved video + song + alignment.
pub fn build_export_json(
    metadata: &VideoMetadata,
    song: &SongPackage,
    alignment: &[AlignmentLine],
) -> Value {
    let member_json = |member: &crate::models::MemberProfile| {
        json!({
            "name": member.stage_name,
            "color": member.color,
            "imageUrl": member.image_url,
            "localImagePath": member.local_image_path,
        })
    };
    json!({
        "version": 1,
        "video": {
            "platform": platform_from_url(&metadata.original_url),
            "videoId": metadata.video_id,
            "url": metadata.original_url,
        },
        "song": {
            "title": song.song.title,
            "artist": song.song.artist,
            "groupName": song.song.group_name,
            "sourceUrl": song.song.source_url,
            "agency": song.song.agency,
            "copyright": song.song.copyright,
            "releaseDate": song.song.release_date,
            "primaryLanguage": song.song.primary_language,
            "secondaryLanguages": song.song.secondary_languages,
        },
        "members": song.members.iter().map(member_json).collect::<Vec<_>>(),
        "featuredArtists": song.song.featured_artists.iter().map(member_json).collect::<Vec<_>>(),
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

/// Build an IMSC1 (TTML1 text-profile) document for the timed lyrics.
///
/// Each lyric line that has alignment timing becomes a `<p begin=… end=…>`.
/// Members and featured artists are emitted as `ttm:agent` definitions and
/// referenced per line via `ttm:agent`, so the singer of each line is preserved.
pub fn build_ttml(
    metadata: &VideoMetadata,
    song: &SongPackage,
    alignment: &[AlignmentLine],
) -> String {
    // Stable agent id per performer name (members first, then featured artists).
    let mut agents: Vec<(String, String)> = Vec::new();
    let mut agent_id_for: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for member in song.members.iter().chain(song.song.featured_artists.iter()) {
        let name = member.stage_name.trim();
        if name.is_empty() {
            continue;
        }
        let key = name.to_lowercase();
        if !agent_id_for.contains_key(&key) {
            let id = format!("a{}", agents.len() + 1);
            agent_id_for.insert(key, id.clone());
            agents.push((id, name.to_string()));
        }
    }

    // Lines with timing, ordered by start time.
    let mut timed: Vec<(&LyricLine, &AlignmentLine)> = song
        .lines
        .iter()
        .filter_map(|line| {
            alignment
                .iter()
                .find(|item| item.lyric_index == line.index)
                .map(|item| (line, item))
        })
        .collect();
    timed.sort_by_key(|(_, timing)| timing.start_ms);

    let lang = bcp47_language(song.song.primary_language.as_deref()).unwrap_or_default();
    let title = if song.song.title.trim().is_empty() {
        "Lyrics".to_string()
    } else {
        song.song.title.clone()
    };

    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    out.push_str(&format!(
        "<tt xmlns=\"http://www.w3.org/ns/ttml\" \
xmlns:ttp=\"http://www.w3.org/ns/ttml#parameter\" \
xmlns:tts=\"http://www.w3.org/ns/ttml#styling\" \
xmlns:ttm=\"http://www.w3.org/ns/ttml#metadata\" \
ttp:profile=\"http://www.w3.org/ns/ttml/profile/imsc1/text\" \
ttp:timeBase=\"media\" xml:lang=\"{}\">\n",
        xml_escape(&lang)
    ));

    out.push_str("  <head>\n    <metadata>\n");
    out.push_str(&format!(
        "      <ttm:title>{}</ttm:title>\n",
        xml_escape(&title)
    ));
    if let Some(copyright) = song
        .song
        .copyright
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        out.push_str(&format!(
            "      <ttm:copyright>{}</ttm:copyright>\n",
            xml_escape(copyright)
        ));
    }
    if !metadata.original_url.trim().is_empty() {
        out.push_str(&format!(
            "      <ttm:desc>Source: {}</ttm:desc>\n",
            xml_escape(metadata.original_url.trim())
        ));
    }
    for (id, name) in &agents {
        out.push_str(&format!(
            "      <ttm:agent type=\"person\" xml:id=\"{id}\"><ttm:name type=\"full\">{}</ttm:name></ttm:agent>\n",
            xml_escape(name)
        ));
    }
    out.push_str("    </metadata>\n");
    out.push_str(
        "    <styling>\n      <style xml:id=\"s1\" tts:fontFamily=\"sansSerif\" \
tts:fontSize=\"100%\" tts:textAlign=\"center\" tts:color=\"white\"/>\n    </styling>\n",
    );
    out.push_str(
        "    <layout>\n      <region xml:id=\"r1\" tts:origin=\"10% 80%\" \
tts:extent=\"80% 15%\" tts:displayAlign=\"after\"/>\n    </layout>\n",
    );
    out.push_str("  </head>\n  <body>\n    <div style=\"s1\" region=\"r1\">\n");
    for (line, timing) in &timed {
        let mut attrs = format!(
            " xml:id=\"L{}\" begin=\"{}\" end=\"{}\"",
            line.index,
            ttml_time(timing.start_ms),
            ttml_time(timing.end_ms)
        );
        if let Some(id) = line
            .member
            .as_deref()
            .map(|name| name.trim().to_lowercase())
            .and_then(|key| agent_id_for.get(&key))
        {
            attrs.push_str(&format!(" ttm:agent=\"{id}\""));
        }
        out.push_str(&format!(
            "      <p{attrs}>{}</p>\n",
            xml_escape(line.original.trim())
        ));
    }
    out.push_str("    </div>\n  </body>\n</tt>\n");
    out
}

/// Format milliseconds as a TTML `clock-time` (`HH:MM:SS.mmm`).
fn ttml_time(ms: i64) -> String {
    let ms = ms.max(0);
    let hours = ms / 3_600_000;
    let minutes = (ms % 3_600_000) / 60_000;
    let seconds = (ms % 60_000) / 1000;
    let millis = ms % 1000;
    format!("{hours:02}:{minutes:02}:{seconds:02}.{millis:03}")
}

/// Best-effort BCP-47 code for a free-form primary-language string. Returns
/// `None` when it can't tell, so `xml:lang` is left empty (undetermined).
fn bcp47_language(language: Option<&str>) -> Option<String> {
    let raw = language?.trim();
    if raw.is_empty() {
        return None;
    }
    let lower = raw.to_lowercase();
    let code = match lower.as_str() {
        "korean" | "kor" | "ko" | "kr" => "ko",
        "english" | "eng" | "en" => "en",
        "japanese" | "jpn" | "ja" | "jp" => "ja",
        "chinese" | "mandarin" | "zho" | "zh" | "cn" => "zh",
        "spanish" | "spa" | "es" => "es",
        "thai" | "tha" | "th" => "th",
        "vietnamese" | "vie" | "vi" => "vi",
        "indonesian" | "ind" | "id" => "id",
        "french" | "fra" | "fr" => "fr",
        _ => {
            // Pass through anything that already looks like a language tag.
            if (2..=3).contains(&lower.len()) && lower.chars().all(|ch| ch.is_ascii_alphabetic()) {
                return Some(lower);
            }
            return None;
        }
    };
    Some(code.to_string())
}

fn xml_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
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

#[cfg(test)]
mod tests {
    use super::{build_ttml, ttml_time};
    use crate::models::{AlignmentLine, LyricLine, MemberProfile, Song, SongPackage, VideoMetadata};

    fn line(index: usize, member: Option<&str>, text: &str) -> LyricLine {
        LyricLine {
            id: None,
            song_id: None,
            index,
            member: member.map(str::to_string),
            original: text.to_string(),
            romanization: None,
            english: None,
            with_all: false,
            layer: Default::default(),
            segments: Vec::new(),
        }
    }

    fn timing(index: usize, start_ms: i64, end_ms: i64) -> AlignmentLine {
        AlignmentLine {
            lyric_index: index,
            caption_index: None,
            start_ms,
            end_ms,
            confidence: 1.0,
            needs_review: false,
        }
    }

    #[test]
    fn formats_clock_time() {
        assert_eq!(ttml_time(0), "00:00:00.000");
        assert_eq!(ttml_time(3_661_234), "01:01:01.234");
    }

    #[test]
    fn builds_imsc1_document_with_timed_paragraphs() {
        let metadata = VideoMetadata {
            video_id: "vid".into(),
            title: Some("S-Class".into()),
            artist_hint: None,
            original_url: "https://www.youtube.com/watch?v=vid".into(),
        };
        let package = SongPackage {
            song: Song {
                id: None,
                title: "S-Class".into(),
                artist: "Stray Kids".into(),
                group_name: Some("Stray Kids".into()),
                primary_language: Some("Korean".into()),
                ..Default::default()
            },
            members: vec![MemberProfile {
                id: None,
                stage_name: "Hyunjin".into(),
                real_name: None,
                color: "#bb71ff".into(),
                image_url: None,
                local_image_path: None,
                provider: None,
            }],
            lines: vec![
                line(0, Some("Hyunjin"), "DDI RO RI <woah>"),
                line(1, None, "Hear this"),
            ],
            provider: "test".into(),
        };
        let alignment = vec![timing(1, 13_000, 15_000), timing(0, 10_000, 12_500)];

        let ttml = build_ttml(&metadata, &package, &alignment);

        assert!(ttml.starts_with("<?xml version=\"1.0\""));
        assert!(ttml.contains("ttp:profile=\"http://www.w3.org/ns/ttml/profile/imsc1/text\""));
        assert!(ttml.contains("xml:lang=\"ko\""));
        assert!(ttml.contains("<ttm:name type=\"full\">Hyunjin</ttm:name>"));
        // Ordered by start time: the 10s line precedes the 13s line.
        let first = ttml.find("begin=\"00:00:10.000\"").unwrap();
        let second = ttml.find("begin=\"00:00:13.000\"").unwrap();
        assert!(first < second);
        // Text is XML-escaped and the singer is referenced as an agent.
        assert!(ttml.contains("ttm:agent=\"a1\">DDI RO RI &lt;woah&gt;</p>"));
    }
}
