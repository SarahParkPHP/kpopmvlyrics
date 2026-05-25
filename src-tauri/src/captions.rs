use anyhow::{anyhow, Result};
use html_escape::decode_html_entities;
use regex::Regex;
use reqwest::blocking::Client;
use serde::Deserialize;
use serde_json::Value;
use std::process::Command;
use url::Url;

use crate::models::CaptionLine;

const BROWSER_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";
const CAPTION_LANGUAGE_ALLOWLIST: &[&str] = &["en", "ko", "ja", "zh", "es"];

pub trait CaptionProvider {
    fn fetch(&self, video_id: &str) -> Result<Vec<CaptionLine>>;
}

#[derive(Debug, Clone)]
pub struct CaptionTrackSet {
    pub language_code: String,
    pub auto_generated: bool,
    pub label: String,
    pub lines: Vec<CaptionLine>,
}

pub struct YouTubeCaptionProvider {
    client: Client,
}

impl Default for YouTubeCaptionProvider {
    fn default() -> Self {
        Self {
            client: Client::builder()
                .user_agent(BROWSER_USER_AGENT)
                .build()
                .expect("client"),
        }
    }
}

impl CaptionProvider for YouTubeCaptionProvider {
    fn fetch(&self, video_id: &str) -> Result<Vec<CaptionLine>> {
        let tracks = self.fetch_all(video_id)?;
        pick_default_caption_track(&tracks)
            .map(|track| track.lines.clone())
            .ok_or_else(|| anyhow!("No usable caption tracks found; import VTT/SRT manually"))
    }
}

impl YouTubeCaptionProvider {
    pub fn fetch_all(&self, video_id: &str) -> Result<Vec<CaptionTrackSet>> {
        let mut results = self.fetch_all_via_ytdlp(video_id).unwrap_or_default();
        if results.is_empty() {
            results = self.fetch_all_via_watch_page(video_id)?;
        }
        if results.is_empty() {
            return Err(anyhow!(
                "No usable caption tracks found; install yt-dlp or import VTT/SRT manually"
            ));
        }
        results.sort_by_key(|track| default_track_priority(track));
        Ok(results)
    }

    fn fetch_all_via_ytdlp(&self, video_id: &str) -> Result<Vec<CaptionTrackSet>> {
        let info = fetch_ytdlp_json(video_id)?;
        let mut results = Vec::new();
        let mut manual_languages = std::collections::HashSet::new();

        for (auto_generated, key) in [(false, "subtitles"), (true, "automatic_captions")] {
            let Some(languages) = info.get(key).and_then(Value::as_object) else {
                continue;
            };
            for (language_code, formats) in languages {
                if !is_wanted_language(language_code) {
                    continue;
                }
                if auto_generated && manual_languages.contains(language_code) {
                    continue;
                }
                let Some(url) = pick_subtitle_url(formats) else {
                    continue;
                };
                match self.download_caption_url(video_id, &url) {
                    Ok(Some(lines)) => {
                        if !auto_generated {
                            manual_languages.insert(language_code.clone());
                        }
                        results.push(CaptionTrackSet {
                            language_code: language_code.clone(),
                            auto_generated,
                            label: track_label(language_code, auto_generated),
                            lines,
                        });
                    }
                    Ok(None) => {}
                    Err(err) => eprintln!(
                        "Caption track {} skipped: {err}",
                        track_label(language_code, auto_generated)
                    ),
                }
            }
        }

        Ok(results)
    }

    fn fetch_all_via_watch_page(&self, video_id: &str) -> Result<Vec<CaptionTrackSet>> {
        let tracks = self.discover_tracks(video_id)?;
        let mut results = Vec::new();
        for track in tracks {
            if let Some(lines) = self.download_track(video_id, &track)? {
                results.push(CaptionTrackSet {
                    language_code: track.language_code.clone().unwrap_or_default(),
                    auto_generated: track.kind.as_deref() == Some("asr"),
                    label: track.label(),
                    lines,
                });
            }
        }
        Ok(results)
    }

    fn download_caption_url(
        &self,
        video_id: &str,
        url: &str,
    ) -> Result<Option<Vec<CaptionLine>>> {
        let body = self
            .client
            .get(url)
            .send()
            .and_then(|response| response.error_for_status())
            .and_then(|response| response.text())?;
        let body = resolve_timedtext_body(&self.client, &body)?;
        if body.trim().is_empty() {
            return Ok(None);
        }
        let mut captions = parse_caption_text(&body)?;
        if captions.is_empty() {
            return Ok(None);
        }
        for caption in &mut captions {
            caption.video_id = video_id.to_string();
        }
        Ok(Some(captions))
    }

    fn discover_tracks(&self, video_id: &str) -> Result<Vec<CaptionTrack>> {
        let watch = self
            .client
            .get(format!("https://www.youtube.com/watch?v={video_id}"))
            .send()?
            .text()?;
        Ok(prioritize_tracks(discover_caption_tracks(&watch)))
    }

    fn download_track(
        &self,
        video_id: &str,
        track: &CaptionTrack,
    ) -> Result<Option<Vec<CaptionLine>>> {
        for url in track.urls() {
            if let Ok(Some(lines)) = self.download_caption_url(video_id, &url) {
                return Ok(Some(lines));
            }
        }
        Ok(None)
    }
}

fn track_label(language_code: &str, auto_generated: bool) -> String {
    if auto_generated {
        format!("{language_code} (auto-generated)")
    } else {
        language_code.to_string()
    }
}

fn is_wanted_language(language_code: &str) -> bool {
    CAPTION_LANGUAGE_ALLOWLIST.contains(&language_code)
}

fn pick_subtitle_url(formats: &Value) -> Option<String> {
    let entries = formats.as_array()?;
    for ext in ["json3", "vtt", "srv1"] {
        if let Some(url) = entries.iter().find_map(|entry| {
            (entry.get("ext").and_then(Value::as_str) == Some(ext))
                .then(|| entry.get("url").and_then(Value::as_str))
                .flatten()
        }) {
            return Some(url.to_string());
        }
    }
    None
}

fn fetch_ytdlp_json(video_id: &str) -> Result<Value> {
    let watch_url = format!("https://www.youtube.com/watch?v={video_id}");
    let output = Command::new("yt-dlp")
        .args(["--no-playlist", "-J", "--skip-download", &watch_url])
        .output()
        .map_err(|err| anyhow!("Could not run yt-dlp: {err}. Install yt-dlp."))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("yt-dlp failed: {}", stderr.trim()));
    }

    serde_json::from_slice(&output.stdout)
        .map_err(|err| anyhow!("Could not parse yt-dlp output: {err}"))
}

fn resolve_timedtext_body(client: &Client, body: &str) -> Result<String> {
    let trimmed = body.trim_start();
    if trimmed.starts_with("#EXTM3U") {
        for line in trimmed.lines() {
            if line.starts_with("http://") || line.starts_with("https://") {
                return client
                    .get(line)
                    .send()
                    .and_then(|response| response.error_for_status())
                    .and_then(|response| response.text())
                    .map_err(Into::into);
            }
        }
        return Err(anyhow!("Caption playlist did not include a timedtext URL"));
    }
    Ok(body.to_string())
}

pub fn pick_default_caption_track<'a>(
    tracks: &'a [CaptionTrackSet],
) -> Option<&'a CaptionTrackSet> {
    tracks
        .iter()
        .min_by_key(|track| default_track_priority(track))
}

fn default_track_priority(track: &CaptionTrackSet) -> u8 {
    match (track.language_code.as_str(), track.auto_generated) {
        ("en", false) => 0,
        ("en", true) => 1,
        ("ko", false) => 2,
        ("ko", true) => 3,
        (_, false) => 4,
        (_, true) => 5,
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
            ("en", false) => 0,
            ("en", true) => 1,
            ("ko", false) => 2,
            ("ko", true) => 3,
            (_, false) => 4,
            (_, true) => 5,
        }
    });
    tracks
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
        discover_caption_tracks, innertube_api_key, is_wanted_language, parse_caption_text,
        pick_default_caption_track, pick_subtitle_url, prioritize_tracks, CaptionProvider,
        CaptionTrackSet, YouTubeCaptionProvider,
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
        assert_eq!(tracks[0].language_code.as_deref(), Some("en"));
        assert!(tracks[0].urls()[0].contains("fmt=json3"));
    }

    #[test]
    fn prefers_english_manual_track_by_default() {
        let tracks = vec![
            CaptionTrackSet {
                language_code: "ko".into(),
                auto_generated: false,
                label: "Korean".into(),
                lines: vec![],
            },
            CaptionTrackSet {
                language_code: "en".into(),
                auto_generated: false,
                label: "English".into(),
                lines: vec![],
            },
        ];
        assert_eq!(
            pick_default_caption_track(&tracks).map(|track| track.label.as_str()),
            Some("English")
        );
    }

    #[test]
    fn prefers_json3_subtitle_url_from_ytdlp_formats() {
        let formats = serde_json::json!([
            {"ext": "vtt", "url": "https://example.test/vtt"},
            {"ext": "json3", "url": "https://example.test/json3"}
        ]);
        assert_eq!(
            pick_subtitle_url(&formats).as_deref(),
            Some("https://example.test/json3")
        );
    }

    #[test]
    fn ignores_unwanted_subtitle_languages() {
        assert!(is_wanted_language("en"));
        assert!(is_wanted_language("ko"));
        assert!(!is_wanted_language("ta"));
        assert!(!is_wanted_language("en-zh"));
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
        assert!(captions.iter().any(|line| {
            line.text
                .to_ascii_lowercase()
                .contains("wake up saying hi to the mirror")
        }));
    }
}
