use std::process::Command;

use anyhow::{anyhow, Result};
use regex::Regex;
use reqwest::blocking::Client;
use scraper::{Html, Selector};
use serde::Deserialize;
use url::Url;

use crate::models::{StreamSpec, VideoFormat, VideoMetadata};

const DEFAULT_STREAM_FORMAT: &str = "18/best[protocol=https][ext=mp4][vcodec^=avc1][acodec^=mp4a][height<=1080]/bestvideo[ext=mp4][vcodec^=avc1]+bestaudio[ext=m4a]/bestvideo+bestaudio/best[protocol=https][ext=mp4][vcodec^=mp4a]";

const AUTO_STREAM_FORMAT_FALLBACKS: &[&str] = &[
    DEFAULT_STREAM_FORMAT,
    "bestvideo[ext=mp4][vcodec^=avc1]+bestaudio[ext=m4a]/bestvideo+bestaudio/18",
    "18",
];

pub fn resolve_video_metadata_inner(input: &str) -> Result<VideoMetadata> {
    let video_id =
        extract_video_id(input).ok_or_else(|| anyhow!("Could not find a YouTube video id"))?;
    let watch_url = format!("https://www.youtube.com/watch?v={video_id}");
    let title = fetch_video_title(&watch_url).ok();
    Ok(VideoMetadata {
        video_id,
        artist_hint: title.as_deref().and_then(artist_hint_from_title),
        title,
        original_url: input.to_string(),
    })
}

pub fn list_video_formats_inner(input: &str) -> Result<Vec<VideoFormat>> {
    let metadata = resolve_video_metadata_inner(input)?;
    let watch_url = format!("https://www.youtube.com/watch?v={}", metadata.video_id);
    let payload = fetch_ytdlp_json(&watch_url)?;
    let audio_format_id = best_audio_format_id(&payload.formats)
        .ok_or_else(|| anyhow!("No HTTP audio stream found for this video"))?;

    let mut best_by_height: Vec<(i32, VideoFormat)> = Vec::new();

    for format in &payload.formats {
        let entry = if is_http_combined(format) {
            Some((
                format_score(format),
                VideoFormat {
                    format_id: format.format_id.clone(),
                    label: format_label(format),
                    height: format.height,
                    ext: format.ext.clone(),
                },
            ))
        } else if is_http_h264_video(format) {
            Some((
                format_score(format),
                VideoFormat {
                    format_id: format!("{}+{}", format.format_id, audio_format_id),
                    label: format_label(format),
                    height: format.height,
                    ext: Some("mp4".to_string()),
                },
            ))
        } else {
            None
        };

        let Some((score, video_format)) = entry else {
            continue;
        };

        if let Some(existing) = best_by_height
            .iter_mut()
            .find(|(_, item)| item.height == video_format.height)
        {
            if score > existing.0 {
                *existing = (score, video_format);
            }
            continue;
        }

        best_by_height.push((score, video_format));
    }

    best_by_height.sort_by(|left, right| {
        right
            .1
            .height
            .unwrap_or(0)
            .cmp(&left.1.height.unwrap_or(0))
            .then_with(|| right.0.cmp(&left.0))
    });

    Ok(best_by_height.into_iter().map(|(_, format)| format).collect())
}

pub fn resolve_stream_spec_inner(input: &str, format_id: Option<&str>) -> Result<StreamSpec> {
    let metadata = resolve_video_metadata_inner(input)?;
    let watch_url = format!("https://www.youtube.com/watch?v={}", metadata.video_id);

    if let Some(format) = format_id.filter(|value| !value.is_empty() && *value != "auto") {
        let urls = get_stream_urls(&watch_url, format)?;
        return stream_spec_from_urls(&urls).map_err(|reason| {
            anyhow!("yt-dlp returned unusable streams for format {format}: {reason}")
        });
    }

    let mut last_error = String::from("no stream formats attempted");
    for format in AUTO_STREAM_FORMAT_FALLBACKS {
        match get_stream_urls(&watch_url, format).and_then(|urls| stream_spec_from_urls(&urls)) {
            Ok(spec) => return Ok(spec),
            Err(err) => last_error = err.to_string(),
        }
    }

    Err(anyhow!(
        "yt-dlp did not return playable direct HTTP streams (HLS/m3u8 is not supported). Last error: {last_error}"
    ))
}

fn stream_spec_from_urls(urls: &[String]) -> Result<StreamSpec> {
    match urls {
        [uri] if is_hls_url(uri) => Err(anyhow!("only HLS playlist URL returned")),
        [uri] => Ok(StreamSpec::Progressive { uri: uri.clone() }),
        [video_uri, audio_uri] if is_hls_url(video_uri) || is_hls_url(audio_uri) => {
            Err(anyhow!("adaptive streams included HLS playlist URLs"))
        }
        [video_uri, audio_uri] => Ok(StreamSpec::Adaptive {
            video_uri: video_uri.clone(),
            audio_uri: audio_uri.clone(),
        }),
        [] => Err(anyhow!("yt-dlp returned no URLs")),
        urls => Err(anyhow!("unexpected URL count ({})", urls.len())),
    }
}

fn fetch_video_title(watch_url: &str) -> Result<String> {
    let html = Client::builder()
        .user_agent("Mozilla/5.0 (compatible; kpopmvlyrics/0.1)")
        .build()?
        .get(watch_url)
        .send()?
        .text()?;
    if let Some(title) = title_from_player_response(&html) {
        return Ok(clean_youtube_title(&title));
    }
    title_from_html(&html)
        .map(|title| clean_youtube_title(&title))
        .ok_or_else(|| anyhow!("Could not read YouTube video title"))
}

fn title_from_player_response(html: &str) -> Option<String> {
    let re = Regex::new(r#"(?s)ytInitialPlayerResponse\s*=\s*(\{.*?\});"#).ok()?;
    let value: PlayerResponse = serde_json::from_str(re.captures(html)?.get(1)?.as_str()).ok()?;
    value.video_details?.title
}

fn title_from_html(html: &str) -> Option<String> {
    let document = Html::parse_document(html);
    let selector = Selector::parse("meta[property='og:title'], title").ok()?;
    let node = document.select(&selector).next()?;
    node.value()
        .attr("content")
        .map(str::to_string)
        .or_else(|| Some(node.text().collect::<Vec<_>>().join(" ")))
}

fn clean_youtube_title(title: &str) -> String {
    let suffix = Regex::new(r"\s*-\s*YouTube\s*$").expect("valid regex");
    suffix.replace(title.trim(), "").trim().to_string()
}

fn artist_hint_from_title(title: &str) -> Option<String> {
    title
        .split_once(" - ")
        .or_else(|| title.split_once(" '"))
        .or_else(|| title.split_once(" \""))
        .map(|(artist, _)| artist.trim().to_string())
        .filter(|artist| !artist.is_empty())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlayerResponse {
    video_details: Option<VideoDetails>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VideoDetails {
    title: Option<String>,
}

pub fn extract_video_id(input: &str) -> Option<String> {
    if input.len() == 11 && input.chars().all(is_video_id_char) {
        return Some(input.to_string());
    }

    if let Ok(url) = Url::parse(input) {
        if let Some(host) = url.host_str() {
            if host.contains("youtu.be") {
                return url.path_segments()?.next().map(clean_id);
            }
            if host.contains("youtube.com") {
                if let Some(id) = url
                    .query_pairs()
                    .find_map(|(k, v)| (k == "v").then(|| v.to_string()))
                {
                    return Some(clean_id(&id));
                }
                let segments: Vec<_> = url.path_segments()?.collect();
                for key in ["embed", "shorts", "live"] {
                    if let Some(pos) = segments.iter().position(|segment| *segment == key) {
                        return segments.get(pos + 1).map(|id| clean_id(id));
                    }
                }
            }
        }
    }

    Regex::new(r#"(?i)(?:v=|youtu\.be/|embed/|shorts/)([A-Za-z0-9_-]{11})"#)
        .ok()?
        .captures(input)
        .and_then(|cap| cap.get(1).map(|m| m.as_str().to_string()))
}

fn clean_id(value: &str) -> String {
    value
        .chars()
        .take_while(|ch| is_video_id_char(*ch))
        .collect()
}

fn is_video_id_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_' || ch == '-'
}

fn fetch_ytdlp_json(watch_url: &str) -> Result<YtDlpJson> {
    let output = Command::new("yt-dlp")
        .args(["--no-playlist", "-J", watch_url])
        .output()
        .map_err(|err| anyhow!("Could not run yt-dlp: {err}. Install yt-dlp."))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("yt-dlp failed: {}", stderr.trim()));
    }

    serde_json::from_slice(&output.stdout).map_err(|err| anyhow!("Could not parse yt-dlp output: {err}"))
}

fn get_stream_urls(watch_url: &str, format: &str) -> Result<Vec<String>> {
    let output = Command::new("yt-dlp")
        .args(["--no-playlist", "-f", format, "--get-url", watch_url])
        .output()
        .map_err(|err| anyhow!("Could not run yt-dlp: {err}. Install yt-dlp."))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("yt-dlp failed: {}", stderr.trim()));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout
        .lines()
        .filter(|line| line.starts_with("http://") || line.starts_with("https://"))
        .map(str::to_string)
        .collect())
}

fn is_hls_url(url: &str) -> bool {
    url.contains("hls_playlist") || url.contains(".m3u8")
}

fn is_http_combined(format: &YtDlpFormat) -> bool {
    let protocol = format.protocol.as_deref().unwrap_or("");
    if !protocol.starts_with("http") {
        return false;
    }

    let vcodec = format.vcodec.as_deref().unwrap_or("none");
    let acodec = format.acodec.as_deref().unwrap_or("none");
    vcodec != "none" && acodec != "none"
}

fn is_http_h264_video(format: &YtDlpFormat) -> bool {
    let protocol = format.protocol.as_deref().unwrap_or("");
    if !protocol.starts_with("http") {
        return false;
    }

    let vcodec = format.vcodec.as_deref().unwrap_or("none");
    let acodec = format.acodec.as_deref().unwrap_or("none");
    vcodec.starts_with("avc1") && acodec == "none" && format.ext.as_deref() == Some("mp4")
}

fn best_audio_format_id(formats: &[YtDlpFormat]) -> Option<String> {
    formats
        .iter()
        .filter(|format| {
            let protocol = format.protocol.as_deref().unwrap_or("");
            protocol.starts_with("http")
                && format.vcodec.as_deref() == Some("none")
                && format
                    .acodec
                    .as_deref()
                    .is_some_and(|codec| codec.starts_with("mp4a"))
        })
        .max_by_key(|format| audio_format_score(format))
        .map(|format| format.format_id.clone())
}

fn audio_format_score(format: &YtDlpFormat) -> i32 {
    let mut score = format.abr.map(|value| (value * 10.0) as i32).unwrap_or(0);
    if format.ext.as_deref() == Some("m4a") {
        score += 1000;
    }
    score
}

fn format_score(format: &YtDlpFormat) -> i32 {
    let mut score = format.height.unwrap_or(0) as i32 * 100;
    if is_http_combined(format) {
        score += 10_000;
    } else if is_http_h264_video(format) {
        score += 9_000;
    }
    if format.ext.as_deref() == Some("mp4") {
        score += 1000;
    }
    if format
        .vcodec
        .as_deref()
        .is_some_and(|codec| codec.starts_with("avc1"))
    {
        score += 500;
    }
    if format
        .acodec
        .as_deref()
        .is_some_and(|codec| codec.starts_with("mp4a"))
    {
        score += 250;
    }
    score
}

fn format_label(format: &YtDlpFormat) -> String {
    let ext = format.ext.as_deref().unwrap_or("mp4").to_uppercase();
    if let Some(height) = format.height {
        return format!("{height}p {ext}");
    }

    format
        .format_note
        .as_deref()
        .filter(|note| !note.is_empty())
        .map(|note| format!("{note} {ext}"))
        .unwrap_or_else(|| format!("Format {ext}"))
}

#[derive(Debug, Deserialize)]
struct YtDlpJson {
    formats: Vec<YtDlpFormat>,
}

#[derive(Debug, Deserialize)]
struct YtDlpFormat {
    format_id: String,
    ext: Option<String>,
    height: Option<u32>,
    vcodec: Option<String>,
    acodec: Option<String>,
    protocol: Option<String>,
    format_note: Option<String>,
    abr: Option<f32>,
}

#[cfg(test)]
mod tests {
    use super::{
        artist_hint_from_title, clean_youtube_title, extract_video_id, is_hls_url,
        stream_spec_from_urls, DEFAULT_STREAM_FORMAT,
    };
    use crate::models::StreamSpec;

    #[test]
    fn extracts_common_youtube_urls() {
        assert_eq!(
            extract_video_id("https://youtu.be/abcdefghijk").as_deref(),
            Some("abcdefghijk")
        );
        assert_eq!(
            extract_video_id("https://www.youtube.com/watch?v=ZYXWVUTSRQP&t=4").as_deref(),
            Some("ZYXWVUTSRQP")
        );
        assert_eq!(
            extract_video_id("https://youtube.com/shorts/1234567890_").as_deref(),
            Some("1234567890_")
        );
    }

    #[test]
    fn cleans_youtube_title() {
        assert_eq!(
            clean_youtube_title("LE SSERAFIM (르세라핌) 'BOOMPALA' OFFICIAL MV - YouTube"),
            "LE SSERAFIM (르세라핌) 'BOOMPALA' OFFICIAL MV"
        );
        assert_eq!(
            artist_hint_from_title("LE SSERAFIM (르세라핌) 'BOOMPALA' OFFICIAL MV").as_deref(),
            Some("LE SSERAFIM (르세라핌)")
        );
    }

    #[test]
    fn default_stream_format_prefers_progressive_or_adaptive() {
        assert!(DEFAULT_STREAM_FORMAT.starts_with("18/"));
        assert!(DEFAULT_STREAM_FORMAT.contains("protocol=https"));
    }

    #[test]
    fn rejects_hls_playlists() {
        assert!(is_hls_url(
            "https://manifest.googlevideo.com/api/manifest/hls_playlist/playlist/index.m3u8"
        ));
        assert!(stream_spec_from_urls(&[
            "https://manifest.googlevideo.com/api/manifest/hls_playlist/playlist/index.m3u8"
                .to_string(),
        ])
        .is_err());
    }

    #[test]
    fn accepts_progressive_http_url() {
        let spec = stream_spec_from_urls(&["https://example.com/video.mp4".to_string()])
            .expect("progressive url");
        assert!(matches!(spec, StreamSpec::Progressive { .. }));
    }
}
