use anyhow::{anyhow, Result};
use html_escape::decode_html_entities;
use regex::Regex;
use reqwest::blocking::Client;
use serde::Deserialize;
use serde_json::json;
use url::Url;

use crate::models::CaptionLine;

pub trait CaptionProvider {
    fn fetch(&self, video_id: &str) -> Result<Vec<CaptionLine>>;
}

pub struct YouTubeCaptionProvider {
    client: Client,
}

impl Default for YouTubeCaptionProvider {
    fn default() -> Self {
        Self {
            client: Client::builder()
                .user_agent("Mozilla/5.0 (compatible; kpopmvlyrics/0.1)")
                .build()
                .expect("client"),
        }
    }
}

impl CaptionProvider for YouTubeCaptionProvider {
    fn fetch(&self, video_id: &str) -> Result<Vec<CaptionLine>> {
        let watch = self
            .client
            .get(format!("https://www.youtube.com/watch?v={video_id}"))
            .send()?
            .text()?;
        let mut tracks = Vec::new();
        if let Some(api_key) = innertube_api_key(&watch) {
            if let Ok(mut player_tracks) = self.fetch_innertube_caption_tracks(video_id, &api_key) {
                tracks.append(&mut player_tracks);
            }
        }
        tracks.extend(discover_caption_tracks(&watch));
        dedupe_tracks(&mut tracks);
        let tracks = prioritize_tracks(tracks);
        if tracks.is_empty() {
            return Err(anyhow!(
                "No public caption tracks found; import VTT/SRT manually"
            ));
        }

        let mut errors = Vec::new();
        for track in &tracks {
            for url in track.urls() {
                match self
                    .client
                    .get(&url)
                    .send()
                    .and_then(|response| response.text())
                {
                    Ok(body) if !body.trim().is_empty() => match parse_caption_text(&body) {
                        Ok(mut captions) if !captions.is_empty() => {
                            for caption in &mut captions {
                                caption.video_id = video_id.to_string();
                            }
                            return Ok(captions);
                        }
                        Ok(_) => {
                            errors.push(format!("{} returned no caption lines", track.label()))
                        }
                        Err(err) => errors.push(format!("{}: {err}", track.label())),
                    },
                    Ok(_) => errors.push(format!("{} returned an empty response", track.label())),
                    Err(err) => errors.push(format!("{} request failed: {err}", track.label())),
                }
            }
        }

        Err(anyhow!(
            "{}; import VTT/SRT manually if YouTube blocks timedtext download",
            errors.into_iter().take(4).collect::<Vec<_>>().join("; ")
        ))
    }
}

impl YouTubeCaptionProvider {
    fn fetch_innertube_caption_tracks(
        &self,
        video_id: &str,
        api_key: &str,
    ) -> Result<Vec<CaptionTrack>> {
        let response: serde_json::Value = self
            .client
            .post(format!(
                "https://www.youtube.com/youtubei/v1/player?key={api_key}"
            ))
            .json(&json!({
                "context": {
                    "client": {
                        "clientName": "ANDROID",
                        "clientVersion": "20.10.38",
                        "hl": "en",
                        "gl": "US"
                    }
                },
                "videoId": video_id
            }))
            .send()?
            .json()?;
        let tracks = response
            .get("captions")
            .and_then(|captions| captions.get("playerCaptionsTracklistRenderer"))
            .and_then(|renderer| renderer.get("captionTracks"))
            .cloned()
            .ok_or_else(|| anyhow!("Innertube player response did not include caption tracks"))?;
        Ok(serde_json::from_value(tracks)?)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CaptionTrack {
    base_url: String,
    language_code: Option<String>,
    kind: Option<String>,
    name: Option<CaptionTrackName>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CaptionTrackName {
    simple_text: Option<String>,
}

impl CaptionTrack {
    fn label(&self) -> String {
        self.name
            .as_ref()
            .and_then(|name| name.simple_text.clone())
            .or_else(|| self.language_code.clone())
            .unwrap_or_else(|| "caption track".to_string())
    }

    fn urls(&self) -> Vec<String> {
        ["json3", "vtt", "srv3"]
            .iter()
            .filter_map(|fmt| with_caption_format(&self.base_url, fmt).ok())
            .collect()
    }
}

fn discover_caption_tracks(html: &str) -> Vec<CaptionTrack> {
    if let Some(tracks) = Regex::new(r#""captionTracks":(\[.*?\])\s*,\s*"audioTracks""#)
        .ok()
        .and_then(|re| re.captures(html))
        .and_then(|cap| cap.get(1).map(|m| m.as_str().to_string()))
        .and_then(|json| serde_json::from_str::<Vec<CaptionTrack>>(&json).ok())
        .filter(|tracks| !tracks.is_empty())
    {
        return tracks;
    }

    let mut tracks = Vec::new();
    let re = Regex::new(r#""baseUrl":"([^"]+)""#).expect("valid regex");
    for cap in re.captures_iter(html) {
        if let Some(raw) = cap.get(1) {
            let decoded = raw.as_str().replace("\\u0026", "&").replace("\\/", "/");
            if decoded.contains("timedtext") {
                tracks.push(CaptionTrack {
                    base_url: decoded,
                    language_code: None,
                    kind: None,
                    name: None,
                });
            }
        }
    }
    tracks
}

fn prioritize_tracks(mut tracks: Vec<CaptionTrack>) -> Vec<CaptionTrack> {
    tracks.sort_by_key(|track| {
        let language = track.language_code.as_deref().unwrap_or_default();
        let auto_generated = track.kind.as_deref() == Some("asr");
        match (language, auto_generated) {
            ("ko", false) => 0,
            ("en", false) => 1,
            ("ko", true) => 2,
            ("en", true) => 3,
            (_, false) => 4,
            (_, true) => 5,
        }
    });
    tracks
}

fn dedupe_tracks(tracks: &mut Vec<CaptionTrack>) {
    let mut seen = std::collections::HashSet::new();
    tracks.retain(|track| {
        let key = format!(
            "{}:{}:{}",
            track.language_code.as_deref().unwrap_or_default(),
            track.kind.as_deref().unwrap_or_default(),
            track.base_url
        );
        seen.insert(key)
    });
}

fn innertube_api_key(html: &str) -> Option<String> {
    Regex::new(r#""INNERTUBE_API_KEY":"([^"]+)""#)
        .ok()?
        .captures(html)
        .and_then(|cap| cap.get(1).map(|m| m.as_str().to_string()))
}

fn with_caption_format(base_url: &str, fmt: &str) -> Result<String> {
    let mut url = Url::parse(base_url)?;
    let pairs: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(key, _)| key != "fmt")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    {
        let mut query = url.query_pairs_mut();
        query.clear();
        for (key, value) in pairs {
            query.append_pair(&key, &value);
        }
        query.append_pair("fmt", fmt);
    }
    Ok(url.to_string())
}

pub fn parse_caption_text(raw: &str) -> Result<Vec<CaptionLine>> {
    let trimmed = raw.trim_start();
    if trimmed.starts_with("WEBVTT") || trimmed.contains("-->") {
        return parse_vtt_or_srt(raw);
    }
    if trimmed.starts_with('{') {
        return parse_youtube_json(raw);
    }
    if trimmed.starts_with('<') {
        return parse_timedtext_xml(raw);
    }
    Err(anyhow!(
        "Unsupported caption format; expected VTT, SRT, YouTube JSON, or timedtext XML captions"
    ))
}

fn parse_timedtext_xml(raw: &str) -> Result<Vec<CaptionLine>> {
    let text_re = Regex::new(r#"(?s)<text\b([^>]*)>(.*?)</text>"#)?;
    let start_re = Regex::new(r#"\bstart="([^"]+)""#)?;
    let dur_re = Regex::new(r#"\bdur="([^"]+)""#)?;
    let mut lines = Vec::new();
    for cap in text_re.captures_iter(raw) {
        let attrs = cap.get(1).map(|m| m.as_str()).unwrap_or_default();
        let body = cap.get(2).map(|m| m.as_str()).unwrap_or_default();
        let start_seconds = start_re
            .captures(attrs)
            .and_then(|cap| cap.get(1))
            .ok_or_else(|| anyhow!("Caption XML line missing start"))?
            .as_str()
            .parse::<f64>()?;
        let duration_seconds = dur_re
            .captures(attrs)
            .and_then(|cap| cap.get(1))
            .map(|m| m.as_str().parse::<f64>())
            .transpose()?
            .unwrap_or(1.0);
        let decoded = decode_html_entities(body).replace('\n', " ");
        let text = decode_html_entities(strip_tags(&decoded).trim()).to_string();
        if text.is_empty() {
            continue;
        }
        let start_ms = (start_seconds * 1000.0).round() as i64;
        let end_ms = ((start_seconds + duration_seconds) * 1000.0).round() as i64;
        lines.push(CaptionLine {
            id: None,
            video_id: String::new(),
            index: lines.len(),
            start_ms,
            end_ms,
            text,
        });
    }
    Ok(lines)
}

fn parse_vtt_or_srt(raw: &str) -> Result<Vec<CaptionLine>> {
    let timestamp_re = Regex::new(
        r"(?m)^\s*(?P<start>\d{1,2}:)?\d{1,2}:\d{2}[\.,]\d{3}\s+-->\s+(?P<end>\d{1,2}:)?\d{1,2}:\d{2}[\.,]\d{3}",
    )?;
    let mut lines = Vec::new();
    let normalized = raw.replace("\r\n", "\n");
    let blocks = normalized.split("\n\n");
    for block in blocks {
        let mut block_lines = block.lines().filter(|line| !line.trim().is_empty());
        let first = block_lines.next();
        let timing = match first {
            Some(line) if line.contains("-->") => line,
            Some(_) => block_lines.next().unwrap_or_default(),
            None => continue,
        };
        if !timestamp_re.is_match(timing) {
            continue;
        }
        let (start, end) = timing
            .split_once("-->")
            .ok_or_else(|| anyhow!("Invalid caption timing"))?;
        let text = block_lines
            .filter(|line| !line.trim_start().starts_with("NOTE"))
            .map(strip_tags)
            .collect::<Vec<_>>()
            .join(" ");
        let text = decode_html_entities(text.trim()).to_string();
        if text.is_empty() {
            continue;
        }
        lines.push(CaptionLine {
            id: None,
            video_id: String::new(),
            index: lines.len(),
            start_ms: parse_timestamp(start.trim())?,
            end_ms: parse_timestamp(end.split_whitespace().next().unwrap_or_default())?,
            text,
        });
    }
    Ok(lines)
}

fn parse_youtube_json(raw: &str) -> Result<Vec<CaptionLine>> {
    let value: serde_json::Value = serde_json::from_str(raw)?;
    let events = value
        .get("events")
        .and_then(|events| events.as_array())
        .ok_or_else(|| anyhow!("Caption JSON did not include events"))?;
    let mut lines = Vec::new();
    for event in events {
        let start_ms = event.get("tStartMs").and_then(|v| v.as_i64()).unwrap_or(0);
        let duration_ms = event
            .get("dDurationMs")
            .and_then(|v| v.as_i64())
            .unwrap_or(1000);
        let text = event
            .get("segs")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .filter_map(|seg| seg.get("utf8").and_then(|v| v.as_str()))
            .collect::<String>()
            .replace('\n', " ");
        let text = decode_html_entities(text.trim()).to_string();
        if text.is_empty() {
            continue;
        }
        lines.push(CaptionLine {
            id: None,
            video_id: String::new(),
            index: lines.len(),
            start_ms,
            end_ms: start_ms + duration_ms,
            text,
        });
    }
    Ok(lines)
}

fn parse_timestamp(raw: &str) -> Result<i64> {
    let parts: Vec<_> = raw
        .replace(',', ".")
        .split(':')
        .map(str::to_string)
        .collect();
    let (hours, minutes, seconds) = match parts.as_slice() {
        [minutes, seconds] => (0, minutes.as_str(), seconds.as_str()),
        [hours, minutes, seconds] => (hours.parse::<i64>()?, minutes.as_str(), seconds.as_str()),
        _ => return Err(anyhow!("Invalid timestamp: {raw}")),
    };
    let (seconds, millis) = seconds
        .split_once('.')
        .ok_or_else(|| anyhow!("Invalid timestamp: {raw}"))?;
    Ok(hours * 3_600_000
        + minutes.parse::<i64>()? * 60_000
        + seconds.parse::<i64>()? * 1000
        + millis.parse::<i64>()?)
}

fn strip_tags(line: &str) -> String {
    Regex::new(r"<[^>]+>")
        .expect("valid regex")
        .replace_all(line, "")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::{
        discover_caption_tracks, innertube_api_key, parse_caption_text, prioritize_tracks,
        CaptionProvider, YouTubeCaptionProvider,
    };

    #[test]
    fn parses_vtt_fixture() {
        let raw = "WEBVTT\n\n00:00:01.000 --> 00:00:02.500\nHello <b>world</b>\n\n00:00:03.000 --> 00:00:04.000\nAgain";
        let lines = parse_caption_text(raw).unwrap();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].start_ms, 1000);
        assert_eq!(lines[0].text, "Hello world");
    }

    #[test]
    fn parses_youtube_json_fixture() {
        let raw = r#"{"events":[{"tStartMs":500,"dDurationMs":800,"segs":[{"utf8":"Hi"},{"utf8":" there"}]}]}"#;
        let lines = parse_caption_text(raw).unwrap();
        assert_eq!(lines[0].text, "Hi there");
        assert_eq!(lines[0].end_ms, 1300);
    }

    #[test]
    fn parses_timedtext_xml_fixture() {
        let raw = r#"<transcript><text start="1.25" dur="2.5">Hello &amp; &lt;b&gt;world&lt;/b&gt;</text></transcript>"#;
        let lines = parse_caption_text(raw).unwrap();
        assert_eq!(lines[0].start_ms, 1250);
        assert_eq!(lines[0].end_ms, 3750);
        assert_eq!(lines[0].text, "Hello & world");
    }

    #[test]
    fn discovers_and_prioritizes_caption_tracks() {
        let html = r#""captionTracks":[{"baseUrl":"https://www.youtube.com/api/timedtext?v=x\u0026lang=en","languageCode":"en","name":{"simpleText":"English"}},{"baseUrl":"https://www.youtube.com/api/timedtext?v=x\u0026lang=ko","languageCode":"ko","name":{"simpleText":"Korean"}}],"audioTracks""#;
        let tracks = prioritize_tracks(discover_caption_tracks(html));
        assert_eq!(tracks[0].language_code.as_deref(), Some("ko"));
        assert!(tracks[0].urls()[0].contains("fmt=json3"));
    }

    #[test]
    fn reads_innertube_api_key() {
        let html = r#"{"INNERTUBE_API_KEY":"abc123"}"#;
        assert_eq!(innertube_api_key(html).as_deref(), Some("abc123"));
    }

    #[test]
    #[ignore = "uses live YouTube caption endpoints"]
    fn fetches_live_youtube_captions() {
        let captions = YouTubeCaptionProvider::default()
            .fetch("V1Lr-_AxeR8")
            .unwrap();
        assert!(!captions.is_empty());
    }
}
