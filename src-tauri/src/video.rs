use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{anyhow, Result};
use regex::Regex;
use reqwest::blocking::Client;
use scraper::{Html, Selector};
use serde::Deserialize;
use tauri::{AppHandle, Emitter};
use url::Url;

use crate::media_server::MediaServer;
use crate::models::{VideoDownloadProgress, VideoFormat, VideoMetadata};

const DEFAULT_STREAM_FORMAT: &str = "bestvideo[ext=mp4][vcodec^=avc1]+bestaudio[ext=m4a]/bestvideo+bestaudio/best[ext=mp4][vcodec^=avc1][acodec^=mp4a]/18";
const STAGING_PREFIX: &str = "._staging_";

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

pub fn resolve_video_stream_inner(
    input: &str,
    format_id: Option<&str>,
    cache_dir: &Path,
    media_server: &MediaServer,
    app: Option<AppHandle>,
) -> Result<String> {
    let metadata = resolve_video_metadata_inner(input)?;
    let watch_url = format!("https://www.youtube.com/watch?v={}", metadata.video_id);
    let format = format_id
        .filter(|value| !value.is_empty() && *value != "auto")
        .unwrap_or(DEFAULT_STREAM_FORMAT);

    if is_direct_progressive_format(format) {
        if let Some(url) = get_direct_stream_url(&watch_url, format)? {
            emit_progress(&app, 100.0, "Stream ready", false);
            return Ok(url);
        }
    }

    let cache_path = resolve_cache_path(cache_dir, &metadata.video_id, format);
    if cache_path.is_file() {
        emit_progress(&app, 100.0, "Using cached video", false);
        return local_media_url(media_server, &cache_path);
    }

    download_merged_stream(
        &watch_url,
        format,
        cache_dir,
        &metadata.video_id,
        &cache_path,
        app.clone(),
    )?;
    local_media_url(media_server, &cache_path)
}

fn local_media_url(media_server: &MediaServer, cache_path: &Path) -> Result<String> {
    media_server
        .media_url(cache_path)
        .map_err(|err| anyhow!(err))
}

pub fn cleanup_incomplete_downloads(cache_dir: &Path) -> Result<()> {
    if !cache_dir.is_dir() {
        return Ok(());
    }

    for entry in std::fs::read_dir(cache_dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();

        if path.is_dir() && name.starts_with(STAGING_PREFIX) {
            let _ = std::fs::remove_dir_all(&path);
            continue;
        }

        if path.is_file() && name.ends_with(".part.mp4") {
            let _ = std::fs::remove_file(&path);
        }
    }

    Ok(())
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

fn get_direct_stream_url(watch_url: &str, format: &str) -> Result<Option<String>> {
    let output = Command::new("yt-dlp")
        .args(["--no-playlist", "-f", format, "--get-url", watch_url])
        .output()
        .map_err(|err| anyhow!("Could not run yt-dlp: {err}. Install yt-dlp."))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("yt-dlp failed: {}", stderr.trim()));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let urls: Vec<String> = stdout
        .lines()
        .filter(|line| line.starts_with("http://") || line.starts_with("https://"))
        .map(str::to_string)
        .collect();

    if urls.len() == 1 && !is_hls_url(&urls[0]) {
        return Ok(Some(urls[0].clone()));
    }

    Ok(None)
}

fn download_merged_stream(
    watch_url: &str,
    format: &str,
    cache_dir: &Path,
    video_id: &str,
    cache_path: &Path,
    app: Option<AppHandle>,
) -> Result<()> {
    std::fs::create_dir_all(cache_dir)?;

    let safe_format = sanitize_format(format);
    let safe_video_id = safe_video_id(video_id);
    let staging_dir = cache_dir.join(format!("{STAGING_PREFIX}{safe_video_id}_{safe_format}"));
    cleanup_staging_dir(&staging_dir);

    if cache_path.is_file() {
        let _ = std::fs::remove_file(cache_path);
    }

    std::fs::create_dir_all(&staging_dir)?;
    let output_template = staging_dir.join("video.%(ext)s");
    let merged_path = staging_dir.join("video.mp4");

    emit_progress(&app, 0.0, "Starting download", true);

    let mut child = Command::new("yt-dlp")
        .args([
            "--no-playlist",
            "--newline",
            "-f",
            format,
            "--merge-output-format",
            "mp4",
            "--postprocessor-args",
            "ffmpeg:-movflags +faststart",
            "-o",
            &output_template.to_string_lossy(),
            watch_url,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| anyhow!("Could not run yt-dlp: {err}. Install yt-dlp."))?;

    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("Could not read yt-dlp progress"))?;
    let app = app.clone();
    let progress_app = app.clone();
    let progress_reader = std::thread::spawn(move || {
        let reader = BufReader::new(stderr);
        for line in reader.lines().map_while(Result::ok) {
            if let Some(percent) = parse_download_percent(&line) {
                let status = if line.contains("[Merger]") {
                    "Merging audio and video"
                } else {
                    "Downloading video"
                };
                emit_progress(&progress_app, percent, status, true);
            } else if line.contains("[Merger]") {
                emit_progress(&progress_app, 99.0, "Merging audio and video", true);
            }
        }
    });

    let status = child
        .wait()
        .map_err(|err| anyhow!("yt-dlp exited unexpectedly: {err}"))?;
    let _ = progress_reader.join();

    if !status.success() {
        cleanup_staging_dir(&staging_dir);
        emit_progress(&app, 0.0, "Download failed", false);
        return Err(anyhow!(
            "yt-dlp failed to prepare video stream. ffmpeg is required for HD playback."
        ));
    }

    if !merged_path.is_file() {
        cleanup_staging_dir(&staging_dir);
        emit_progress(&app, 0.0, "Download failed", false);
        return Err(anyhow!("yt-dlp did not produce a merged video file"));
    }

    emit_progress(&app, 99.0, "Finalizing video", true);

    if cache_path.is_file() {
        let _ = std::fs::remove_file(cache_path);
    }

    std::fs::rename(&merged_path, cache_path).map_err(|err| {
        cleanup_staging_dir(&staging_dir);
        anyhow!("Could not finalize downloaded video: {err}")
    })?;

    cleanup_staging_dir(&staging_dir);
    emit_progress(&app, 100.0, "Download complete", false);
    Ok(())
}

fn cleanup_staging_dir(staging_dir: &Path) {
    if staging_dir.exists() {
        let _ = std::fs::remove_dir_all(staging_dir);
    }
}

fn parse_download_percent(line: &str) -> Option<f32> {
    let re = Regex::new(r"(?i)\[download\]\s+([\d.]+)%").ok()?;
    re.captures(line)
        .and_then(|caps| caps.get(1))
        .and_then(|value| value.as_str().parse().ok())
}

fn emit_progress(app: &Option<AppHandle>, percent: f32, status: &str, active: bool) {
    let Some(app) = app else {
        return;
    };

    let _ = app.emit(
        "video-download-progress",
        VideoDownloadProgress {
            percent,
            status: status.to_string(),
            active,
        },
    );
}

fn resolve_cache_path(cache_dir: &Path, video_id: &str, format: &str) -> PathBuf {
    let cache_path = cache_file_path(cache_dir, video_id, format);
    if cache_path.is_file() {
        return cache_path;
    }

    let legacy_path = cache_dir.join(format!("{video_id}_{}.mp4", sanitize_format(format)));
    if legacy_path.is_file() {
        if let Err(err) = std::fs::rename(&legacy_path, &cache_path) {
            eprintln!("Could not migrate legacy cache file: {err}");
            return legacy_path;
        }
    }

    cache_path
}

fn cache_file_path(cache_dir: &Path, video_id: &str, format: &str) -> PathBuf {
    cache_dir.join(format!(
        "{}_{}.mp4",
        safe_video_id(video_id),
        sanitize_format(format)
    ))
}

fn safe_video_id(video_id: &str) -> String {
    let sanitized: String = video_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .collect();

    if sanitized.is_empty() || sanitized.starts_with('-') {
        format!("id_{sanitized}")
    } else {
        sanitized
    }
}

fn sanitize_format(format: &str) -> String {
    format
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn is_direct_progressive_format(format: &str) -> bool {
    !format.contains('+') && !format.contains('[') && format.chars().all(|ch| ch.is_ascii_digit())
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
        artist_hint_from_title, clean_youtube_title, extract_video_id, is_direct_progressive_format,
        parse_download_percent, sanitize_format, DEFAULT_STREAM_FORMAT,
    };

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
    fn detects_direct_progressive_formats() {
        assert!(is_direct_progressive_format("18"));
        assert!(!is_direct_progressive_format("136+140"));
        assert!(!is_direct_progressive_format(DEFAULT_STREAM_FORMAT));
    }

    #[test]
    fn parses_download_percent() {
        assert_eq!(
            parse_download_percent("[download]  45.2% of   27.00MiB at  2.50MiB/s ETA 00:05"),
            Some(45.2)
        );
    }

    #[test]
    fn sanitizes_format_ids() {
        assert_eq!(sanitize_format("136+140"), "136_140");
    }
}
