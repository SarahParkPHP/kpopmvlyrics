use anyhow::{anyhow, Result};
use regex::Regex;
use reqwest::blocking::Client;
use scraper::{Html, Selector};
use serde::Deserialize;
use url::Url;

use crate::models::VideoMetadata;

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

#[cfg(test)]
mod tests {
    use super::{artist_hint_from_title, clean_youtube_title, extract_video_id};

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
}
