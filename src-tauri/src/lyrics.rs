use anyhow::{anyhow, Result};
use ego_tree::NodeRef;
use html_escape::decode_html_entities;
use regex::Regex;
use reqwest::blocking::Client;
use scraper::{ElementRef, Html, Node, Selector};
use strsim::jaro_winkler;

use crate::models::{LyricLine, LyricSegment, MemberProfile, Song, SongPackage};

pub trait LyricsProvider {
    fn fetch(&self, query: &str) -> Result<SongPackage>;
}

pub struct ColorCodedLyricsProvider {
    client: Client,
}

impl Default for ColorCodedLyricsProvider {
    fn default() -> Self {
        Self {
            client: Client::builder()
                .user_agent("kpopmvlyrics/0.1")
                .build()
                .expect("client"),
        }
    }
}

impl LyricsProvider for ColorCodedLyricsProvider {
    fn fetch(&self, query: &str) -> Result<SongPackage> {
        let search_url = format!("https://colorcodedlyrics.com/?s={}", urlencoding(query));
        let search_html = self.client.get(search_url).send()?.text()?;
        let document = Html::parse_document(&search_html);
        let link = select_best_colorcodedlyrics_link(&document, query)
            .ok_or_else(|| anyhow!("ColorCodedLyrics result not found"))?;
        let html = self.client.get(&link).send()?.text()?;
        parse_colorcodedlyrics_html(&html, Some(link))
    }
}

pub struct GeniusProvider {
    client: Client,
}

impl Default for GeniusProvider {
    fn default() -> Self {
        Self {
            client: Client::builder()
                .user_agent("kpopmvlyrics/0.1")
                .build()
                .expect("client"),
        }
    }
}

impl LyricsProvider for GeniusProvider {
    fn fetch(&self, query: &str) -> Result<SongPackage> {
        let search_url = format!("https://genius.com/search?q={}", urlencoding(query));
        let html = self.client.get(search_url).send()?.text()?;
        let document = Html::parse_document(&html);
        let selector = Selector::parse("a[href*='/']").unwrap();
        let link = document
            .select(&selector)
            .filter_map(|node| node.value().attr("href"))
            .find(|href| href.starts_with("https://genius.com/") && href.contains("lyrics"))
            .ok_or_else(|| anyhow!("Genius lyric page not found"))?;
        let page = self.client.get(link).send()?.text()?;
        parse_genius_html(&page, Some(link.to_string()))
    }
}

pub fn parse_manual_lyrics(raw_text: &str, title: &str, artist: &str) -> Result<SongPackage> {
    let lines = parse_plain_lines(raw_text);
    if lines.is_empty() {
        return Err(anyhow!("No lyric lines found"));
    }
    Ok(SongPackage {
        song: Song {
            id: None,
            title: title.trim().to_string(),
            artist: artist.trim().to_string(),
            group_name: Some(artist.trim().to_string()),
            source_url: None,
        },
        members: members_from_lines(&lines),
        lines,
        provider: "manual".into(),
    })
}

pub fn parse_colorcodedlyrics_html(html: &str, source_url: Option<String>) -> Result<SongPackage> {
    let document = Html::parse_document(html);
    let title_selector = Selector::parse("h1.entry-title, h1").unwrap();
    let title = document
        .select(&title_selector)
        .next()
        .map(|node| node.text().collect::<Vec<_>>().join(" "))
        .unwrap_or_else(|| "Untitled".to_string());

    let content_html = first_selector_html(&document, &[".entry-content", "article", "body"])
        .ok_or_else(|| anyhow!("No lyric content found"))?;
    let (lines, color_members) = parse_colorcodedlyrics_content(&content_html);
    if lines.is_empty() {
        return Err(anyhow!("No ColorCodedLyrics lines parsed"));
    }
    let (artist, title) = split_artist_title(&title);
    Ok(SongPackage {
        song: Song {
            id: None,
            title,
            artist: artist.clone(),
            group_name: Some(artist),
            source_url,
        },
        members: if color_members.is_empty() {
            members_from_lines(&lines)
        } else {
            color_members
        },
        lines,
        provider: "colorcodedlyrics".into(),
    })
}

pub fn parse_genius_html(html: &str, source_url: Option<String>) -> Result<SongPackage> {
    let document = Html::parse_document(html);
    let title_selector = Selector::parse("h1").unwrap();
    let title = document
        .select(&title_selector)
        .next()
        .map(|node| node.text().collect::<Vec<_>>().join(" "))
        .unwrap_or_else(|| "Untitled".to_string());
    let lyric_selector = Selector::parse("[data-lyrics-container='true']").unwrap();
    let mut raw = String::new();
    for node in document.select(&lyric_selector) {
        raw.push_str(&node.text().collect::<Vec<_>>().join("\n"));
        raw.push('\n');
    }
    if raw.trim().is_empty() {
        return Err(anyhow!("No Genius lyric container found"));
    }
    let lines = parse_plain_lines(&raw);
    let (artist, title) = split_artist_title(&title);
    Ok(SongPackage {
        song: Song {
            id: None,
            title,
            artist: artist.clone(),
            group_name: Some(artist),
            source_url,
        },
        members: members_from_lines(&lines),
        lines,
        provider: "genius".into(),
    })
}

fn parse_plain_lines(raw: &str) -> Vec<LyricLine> {
    let member_re =
        Regex::new(r"^\s*(?:\[)?([A-Za-z0-9가-힣 ._'’-]{1,32})(?:\])?\s*[:：]\s*(.+)$").unwrap();
    raw.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| !line.starts_with("Translations") && !line.starts_with("Romanization"))
        .filter(|line| !line.starts_with('[') || !line.ends_with(']'))
        .filter(|line| !looks_like_metadata(line))
        .enumerate()
        .map(|(index, line)| {
            let (member, text) = member_re
                .captures(line)
                .and_then(|cap| {
                    Some((
                        cap.get(1)?.as_str().trim().to_string(),
                        cap.get(2)?.as_str().trim().to_string(),
                    ))
                })
                .map(|(member, text)| (Some(member), text))
                .unwrap_or_else(|| (None, line.to_string()));
            LyricLine {
                id: None,
                song_id: None,
                index,
                member,
                original: text,
                romanization: None,
                english: None,
                segments: Vec::new(),
            }
        })
        .collect()
}

fn parse_colorcodedlyrics_content(html: &str) -> (Vec<LyricLine>, Vec<MemberProfile>) {
    let color_span_re =
        Regex::new(r#"(?is)<span[^>]*style="[^"]*color:\s*([^;"']+)[^"]*"[^>]*>(.*?)</span>"#)
            .expect("valid regex");
    let br_re = Regex::new(r#"(?i)<br\s*/?>"#).expect("valid regex");
    let blocks = content_blocks(html);

    let mut color_names: Vec<(String, String)> = Vec::new();
    for block_html in &blocks {
        let block_text = strip_tags(block_html);
        let spans: Vec<(String, String)> = color_span_re
            .captures_iter(block_html)
            .filter_map(|cap| {
                let color = cap.get(1)?.as_str().trim().to_lowercase();
                let text = clean_member_name(&strip_tags(cap.get(2)?.as_str()));
                if text.is_empty() {
                    None
                } else {
                    Some((color, text))
                }
            })
            .collect();
        if spans.len() >= 2
            && block_text.len() < 160
            && spans.iter().all(|(_, text)| {
                text.split_whitespace().count() <= 3
                    && text.chars().all(|ch| {
                        ch.is_alphabetic() || ch.is_whitespace() || ch == '-' || ch == '_'
                    })
            })
        {
            color_names = spans;
            break;
        }
    }

    let mut color_to_member = std::collections::HashMap::new();
    let palette = [
        "#e84855", "#2f80ed", "#27ae60", "#f2994a", "#9b51e0", "#00a6a6", "#d81b60", "#607d8b",
    ];
    let members: Vec<MemberProfile> = color_names
        .iter()
        .enumerate()
        .map(|(index, (color, name))| {
            if let Some(normalized) = normalize_color(color) {
                color_to_member.insert(normalized, name.clone());
            }
            MemberProfile {
                id: None,
                stage_name: name.clone(),
                real_name: None,
                color: normalize_color(color)
                    .unwrap_or_else(|| palette[index % palette.len()].into()),
                image_url: None,
                local_image_path: None,
                provider: Some("colorcodedlyrics".into()),
            }
        })
        .collect();

    if let Some(lines) = parse_colorcodedlyrics_columns(html, &color_to_member) {
        return (lines, members);
    }

    let mut active_language: Option<String> = None;
    let mut lines = Vec::new();
    for block_html in &blocks {
        let block_text = strip_tags(block_html);
        let marker = block_text.trim().to_lowercase();
        if marker == "english" || marker == "romanization" || marker == "korean" {
            if !lines.is_empty() {
                break;
            }
            active_language = Some(marker);
            continue;
        }
        if marker.contains("credits") || marker.contains("disclaimer") {
            break;
        }
        if active_language.is_none() {
            continue;
        }

        let spans: Vec<_> = color_span_re.captures_iter(block_html).collect();
        if spans.is_empty() {
            for text in html_lines(block_html, &br_re) {
                push_lyric_line(&mut lines, None, text);
            }
            continue;
        }

        for span in spans {
            let color = span
                .get(1)
                .and_then(|value| normalize_color(value.as_str()));
            let member = color
                .as_deref()
                .and_then(|value| color_to_member.get(value).cloned());
            let Some(span_html) = span.get(2).map(|value| value.as_str()) else {
                continue;
            };
            for text in html_lines(span_html, &br_re) {
                push_lyric_line(&mut lines, member.clone(), text);
            }
        }
    }

    if lines.is_empty() && blocks.is_empty() {
        (parse_plain_lines(&strip_tags(html)), members)
    } else {
        (lines, members)
    }
}

fn clean_member_name(value: &str) -> String {
    value
        .trim()
        .trim_matches(|ch: char| ch == ',' || ch == ';' || ch == '/' || ch.is_whitespace())
        .trim()
        .to_string()
}

#[derive(Debug, Clone)]
struct ParsedColumnLine {
    text: String,
    color: Option<String>,
    segments: Vec<ParsedSegment>,
}

#[derive(Debug, Clone)]
struct ParsedSegment {
    text: String,
    color: Option<String>,
}

fn parse_colorcodedlyrics_columns(
    html: &str,
    color_to_member: &std::collections::HashMap<String, String>,
) -> Option<Vec<LyricLine>> {
    let document = Html::parse_fragment(html);
    let column_selector = Selector::parse(".wp-block-column").ok()?;
    let mut romanization: Vec<Vec<ParsedColumnLine>> = Vec::new();
    let mut hangul: Vec<Vec<ParsedColumnLine>> = Vec::new();
    let mut translation: Vec<Vec<ParsedColumnLine>> = Vec::new();

    for column in document.select(&column_selector) {
        let Some((language, lines)) = parse_lyric_column(column) else {
            continue;
        };
        match language.as_str() {
            "romanization" => romanization = lines,
            "hangul" | "korean" => hangul = lines,
            "translation" | "english" => translation = lines,
            _ => {}
        }
    }

    if romanization.is_empty() && hangul.is_empty() && translation.is_empty() {
        return None;
    }

    let group_count = romanization.len().max(hangul.len()).max(translation.len());
    let mut lines = Vec::new();
    for group_index in 0..group_count {
        let roman_group = romanization
            .get(group_index)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let korean_group = hangul.get(group_index).map(Vec::as_slice).unwrap_or(&[]);
        let english_group = translation
            .get(group_index)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let line_count = roman_group
            .len()
            .max(korean_group.len())
            .max(english_group.len());
        for index in 0..line_count {
            let roman = roman_group.get(index);
            let korean = korean_group.get(index);
            let english = english_group.get(index);
            let original = korean
                .or(roman)
                .or(english)
                .map(|line| line.text.trim().to_string())
                .unwrap_or_default();
            if original.is_empty() || looks_like_metadata(&original) {
                continue;
            }
            let member = aggregate_members(korean.or(roman), color_to_member);
            lines.push(LyricLine {
                id: None,
                song_id: None,
                index: lines.len(),
                member: member.clone(),
                original,
                romanization: roman
                    .map(|line| line.text.trim().to_string())
                    .filter(|text| !text.is_empty()),
                english: english
                    .map(|line| line.text.trim().to_string())
                    .filter(|text| !text.is_empty()),
                segments: lyric_segments(roman, korean, english, color_to_member),
            });
        }
    }

    (!lines.is_empty()).then_some(lines)
}

fn parse_lyric_column(column: ElementRef<'_>) -> Option<(String, Vec<Vec<ParsedColumnLine>>)> {
    let paragraph_selector = Selector::parse("p").ok()?;
    let mut language = None;
    let mut groups = Vec::new();
    for paragraph in column.select(&paragraph_selector) {
        let marker = strip_tags(&paragraph.html()).trim().to_lowercase();
        if language.is_none() {
            if matches!(
                marker.as_str(),
                "romanization" | "hangul" | "korean" | "translation" | "english"
            ) {
                language = Some(marker);
            }
            continue;
        }
        if marker.contains("credits") || marker.contains("disclaimer") {
            break;
        }
        let lines = parsed_lines_from_block(&paragraph.inner_html());
        if !lines.is_empty() {
            groups.push(lines);
        }
    }
    language.map(|language| (language, groups))
}

fn parsed_lines_from_block(html: &str) -> Vec<ParsedColumnLine> {
    let br_re = Regex::new(r#"(?i)<br\s*/?>"#).expect("valid regex");
    let mut lines = Vec::new();
    for segment in br_re.replace_all(html, "\n").split('\n') {
        let segment = segment.trim();
        if segment.is_empty() {
            continue;
        }
        let fragment = Html::parse_fragment(segment);
        let mut segment_parts = Vec::new();
        for child in fragment.root_element().children() {
            collect_segment_parts(child, None, &mut segment_parts);
        }
        if segment_parts.is_empty() {
            let text = strip_tags(segment).trim().to_string();
            if !text.is_empty() && !looks_like_metadata(&text) {
                lines.push(ParsedColumnLine {
                    text: text.clone(),
                    color: None,
                    segments: vec![ParsedSegment { text, color: None }],
                });
            }
        } else {
            let text = segment_parts
                .iter()
                .map(|part| part.text.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            if !text.is_empty() && !looks_like_metadata(&text) {
                let color = segment_parts.iter().find_map(|part| part.color.clone());
                lines.push(ParsedColumnLine {
                    text,
                    color,
                    segments: segment_parts,
                });
            }
        }
    }
    lines
}

fn collect_segment_parts(
    node: NodeRef<'_, Node>,
    inherited_color: Option<String>,
    parts: &mut Vec<ParsedSegment>,
) {
    if let Some(text) = node.value().as_text() {
        push_segment_text(parts, inherited_color, text.trim());
        return;
    }

    let Some(element) = node.value().as_element() else {
        return;
    };
    if element.name() == "br" {
        return;
    }

    let color = element
        .attr("style")
        .and_then(style_color)
        .or(inherited_color);
    for child in node.children() {
        collect_segment_parts(child, color.clone(), parts);
    }
}

fn push_segment_text(parts: &mut Vec<ParsedSegment>, color: Option<String>, raw_text: &str) {
    let text = raw_text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        return;
    }
    if let Some(last) = parts.last_mut().filter(|line| line.color == color) {
        if !last.text.ends_with(' ') {
            last.text.push(' ');
        }
        last.text.push_str(&text);
    } else {
        parts.push(ParsedSegment { text, color });
    }
}

fn style_color(style: &str) -> Option<String> {
    let color_re = Regex::new(r#"(?i)color:\s*([^;"']+)"#).expect("valid regex");
    color_re
        .captures(style)
        .and_then(|cap| normalize_color(cap.get(1)?.as_str()))
}

fn aggregate_members(
    line: Option<&ParsedColumnLine>,
    color_to_member: &std::collections::HashMap<String, String>,
) -> Option<String> {
    let mut members = Vec::new();
    for segment in line.into_iter().flat_map(|line| &line.segments) {
        let Some(color) = &segment.color else {
            continue;
        };
        let Some(member) = color_to_member.get(color) else {
            continue;
        };
        if !members.iter().any(|existing: &String| existing == member) {
            members.push(member.clone());
        }
    }
    if members.is_empty() {
        line.and_then(|line| line.color.as_ref())
            .and_then(|color| color_to_member.get(color))
            .cloned()
    } else {
        Some(members.join(", "))
    }
}

fn lyric_segments(
    roman: Option<&ParsedColumnLine>,
    korean: Option<&ParsedColumnLine>,
    english: Option<&ParsedColumnLine>,
    color_to_member: &std::collections::HashMap<String, String>,
) -> Vec<LyricSegment> {
    let mut segments = Vec::new();
    push_lyric_segments(&mut segments, "original", korean, color_to_member);
    push_lyric_segments(&mut segments, "romanization", roman, color_to_member);
    push_lyric_segments(&mut segments, "english", english, color_to_member);
    segments
}

fn push_lyric_segments(
    output: &mut Vec<LyricSegment>,
    language: &str,
    line: Option<&ParsedColumnLine>,
    color_to_member: &std::collections::HashMap<String, String>,
) {
    let Some(line) = line else {
        return;
    };
    if line.segments.is_empty() {
        if let Some(color) = line.color.clone() {
            output.push(LyricSegment {
                language: language.to_string(),
                text: line.text.clone(),
                member: line
                    .color
                    .as_ref()
                    .and_then(|value| color_to_member.get(value).cloned()),
                color: Some(color),
            });
        }
        return;
    }
    if line.segments.len() <= 1
        && line
            .segments
            .first()
            .and_then(|segment| segment.color.as_ref())
            .is_none()
        && line.color.is_none()
    {
        return;
    }
    for segment in &line.segments {
        output.push(LyricSegment {
            language: language.to_string(),
            text: segment.text.clone(),
            member: segment
                .color
                .as_ref()
                .and_then(|color| color_to_member.get(color).cloned()),
            color: segment.color.clone(),
        });
    }
}

fn select_best_colorcodedlyrics_link(document: &Html, query: &str) -> Option<String> {
    let selector = Selector::parse("h2 a, article a, .entry-title a").ok()?;
    let query_key = search_key(query);
    let tokens: Vec<String> = query_key
        .split_whitespace()
        .filter(|token| token.len() > 1)
        .map(ToOwned::to_owned)
        .collect();

    document
        .select(&selector)
        .filter_map(|node| {
            let href = node.value().attr("href")?;
            if !href.contains("colorcodedlyrics.com") {
                return None;
            }
            let text = node.text().collect::<Vec<_>>().join(" ");
            let text_key = search_key(&text);
            let href_key = search_key(href);
            let candidate_key = format!("{text_key} {href_key}");
            let mut score = jaro_winkler(&query_key, &text_key);
            if !tokens.is_empty() {
                let covered = tokens
                    .iter()
                    .filter(|token| candidate_key.contains(token.as_str()))
                    .count() as f64;
                score += covered / tokens.len() as f64;
            }
            Some((score, href.to_string()))
        })
        .max_by(|left, right| {
            left.0
                .partial_cmp(&right.0)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(_, href)| href)
}

fn search_key(value: &str) -> String {
    let decoded = decode_html_entities(value).to_string().to_lowercase();
    Regex::new(r"(?i)\b(official|mv|m/v|music video|lyrics|color coded|youtube)\b")
        .expect("valid regex")
        .replace_all(&decoded, " ")
        .chars()
        .map(|ch| {
            if ch.is_alphanumeric() || ch.is_whitespace() {
                ch
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn content_blocks(html: &str) -> Vec<String> {
    let p_re = Regex::new(r#"(?is)<p\b[^>]*>(.*?)</p>"#).expect("valid regex");
    let div_re = Regex::new(r#"(?is)<div\b[^>]*class="[^"]*ujudUb[^"]*"[^>]*>(.*?)</div>"#)
        .expect("valid regex");
    let mut blocks: Vec<(usize, String)> = p_re
        .captures_iter(html)
        .filter_map(|cap| Some((cap.get(0)?.start(), cap.get(1)?.as_str().to_string())))
        .collect();
    blocks.extend(
        div_re
            .captures_iter(html)
            .filter_map(|cap| Some((cap.get(0)?.start(), cap.get(1)?.as_str().to_string()))),
    );
    blocks.sort_by_key(|(position, _)| *position);
    blocks.into_iter().map(|(_, block)| block).collect()
}

fn push_lyric_line(lines: &mut Vec<LyricLine>, member: Option<String>, text: String) {
    let text = text.trim();
    if text.is_empty() || looks_like_metadata(text) {
        return;
    }
    lines.push(LyricLine {
        id: None,
        song_id: None,
        index: lines.len(),
        member,
        original: text.to_string(),
        romanization: None,
        english: None,
        segments: Vec::new(),
    });
}

fn html_lines(html: &str, br_re: &Regex) -> Vec<String> {
    br_re
        .replace_all(html, "\n")
        .lines()
        .map(strip_tags)
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect()
}

fn strip_tags(html: &str) -> String {
    let br_re = Regex::new(r#"(?i)<br\s*/?>"#).expect("valid regex");
    let tag_re = Regex::new(r#"(?is)<[^>]+>"#).expect("valid regex");
    let text = br_re.replace_all(html, "\n");
    decode_html_entities(&tag_re.replace_all(&text, "")).to_string()
}

fn normalize_color(color: &str) -> Option<String> {
    let color = color.trim().trim_end_matches(';').to_lowercase();
    if color.starts_with('#') {
        Some(color)
    } else {
        None
    }
}

fn looks_like_metadata(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower.starts_with("lyrics:")
        || lower.starts_with("lyrics/")
        || lower.starts_with("composer")
        || lower.starts_with("arranger")
        || lower.starts_with("producer")
        || lower.starts_with("credits")
        || lower.starts_with("info:")
        || lower.starts_with("english:")
        || lower.starts_with("disclaimer")
        || Regex::new(r"(?i)^\[(ep|album|single|mini album|digital single)\]\s+")
            .expect("valid regex")
            .is_match(text)
        || Regex::new(r"^\d{4}\.\d{2}\.\d{2}$")
            .expect("valid regex")
            .is_match(text)
}

fn members_from_lines(lines: &[LyricLine]) -> Vec<MemberProfile> {
    let palette = [
        "#e84855", "#2f80ed", "#27ae60", "#f2994a", "#9b51e0", "#00a6a6", "#d81b60", "#607d8b",
    ];
    let mut members = Vec::new();
    for line in lines {
        let Some(name) = &line.member else { continue };
        if members
            .iter()
            .any(|member: &MemberProfile| member.stage_name.eq_ignore_ascii_case(name))
        {
            continue;
        }
        members.push(MemberProfile {
            id: None,
            stage_name: name.clone(),
            real_name: None,
            color: palette[members.len() % palette.len()].to_string(),
            image_url: None,
            local_image_path: None,
            provider: Some("lyrics".into()),
        });
    }
    members
}

fn split_artist_title(raw: &str) -> (String, String) {
    let cleaned = raw.replace("Lyrics", "").replace("Color Coded", "");
    if let Some((artist, title)) = cleaned
        .split_once(" – ")
        .or_else(|| cleaned.split_once(" - "))
    {
        (artist.trim().to_string(), title.trim().to_string())
    } else {
        ("Unknown Artist".into(), cleaned.trim().to_string())
    }
}

fn first_selector_html(document: &Html, selectors: &[&str]) -> Option<String> {
    for selector in selectors {
        let selector = Selector::parse(selector).ok()?;
        if let Some(node) = document.select(&selector).next() {
            return Some(node.inner_html());
        }
    }
    None
}

fn urlencoding(query: &str) -> String {
    url::form_urlencoded::byte_serialize(query.as_bytes()).collect()
}

#[cfg(test)]
mod tests {
    use super::{
        parse_colorcodedlyrics_html, parse_genius_html, parse_manual_lyrics,
        select_best_colorcodedlyrics_link,
    };
    use scraper::Html;

    #[test]
    fn parses_manual_member_prefixes() {
        let package = parse_manual_lyrics("A: hello\nB: world", "Song", "Group").unwrap();
        assert_eq!(package.lines[0].member.as_deref(), Some("A"));
        assert_eq!(package.members.len(), 2);
    }

    #[test]
    fn parses_colorcodedlyrics_fixture() {
        let html = r#"<h1 class="entry-title">GROUP - Song Lyrics</h1><div class="entry-content">Jisoo: hello<br/>Jennie: world</div>"#;
        let package =
            parse_colorcodedlyrics_html(html, Some("https://example.test".into())).unwrap();
        assert_eq!(package.song.title, "Song");
        assert_eq!(package.lines.len(), 2);
    }

    #[test]
    fn parses_colorcodedlyrics_colored_spans_after_language_marker() {
        let html = r##"
            <h1 class="entry-title">LE SSERAFIM - BOOMPALA Lyrics</h1>
            <div class="entry-content">
              <p style="text-align:center"><span style="color: #d16ea3">Sakura</span>, <span style="color: #d4f6ff">Chaewon</span></p>
              <p><strong><span>English</span></strong></p>
              <p><span style="color: #d16ea3">On my chest<br/>Only loving myself</span><br/><span style="color: #d4f6ff">One two three bye bye</span></p>
              <p><strong><span>Credits</span></strong></p>
            </div>
        "##;
        let package = parse_colorcodedlyrics_html(html, None).unwrap();
        assert_eq!(package.members.len(), 2);
        assert_eq!(package.lines.len(), 3);
        assert_eq!(package.lines[0].member.as_deref(), Some("Sakura"));
        assert_eq!(package.lines[2].member.as_deref(), Some("Chaewon"));
        assert_eq!(package.lines[0].original, "On my chest");
    }

    #[test]
    fn parses_colorcodedlyrics_romanization_without_metadata_fallback() {
        let html = r##"
            <h1 class="entry-title">NMIXX - Heavy Serenade Lyrics</h1>
            <div class="entry-content">
              <p style="text-align:center"><span style="color: #a0a0a0">Lily</span>, <span style="color: #c0c0c0">Haewon</span></p>
              <p>[EP] Heavy Serenade</p>
              <p>2026.05.11</p>
              <p><strong>Romanization</strong></p>
              <p><span style="color: #a0a0a0">Baby say goodbye if you see her</span><br/><span style="color: #c0c0c0">Heavy serenade</span></p>
              <p><strong>English</strong></p>
              <p>metadata should not replace romanized lines</p>
            </div>
        "##;
        let package = parse_colorcodedlyrics_html(html, None).unwrap();
        assert_eq!(package.lines.len(), 2);
        assert_eq!(package.lines[0].original, "Baby say goodbye if you see her");
        assert!(package
            .lines
            .iter()
            .all(|line| !line.original.contains("[EP]") && !line.original.contains("2026.05.11")));
        assert_eq!(package.members.len(), 2);
    }

    #[test]
    fn parses_colorcodedlyrics_columns_as_language_variants() {
        let html = r##"
            <h1 class="entry-title">NMIXX - Heavy Serenade Lyrics</h1>
            <div class="entry-content">
              <p style="text-align:center"><span style="color: #89b6e6">Lily</span>, <span style="color: #e8ffe8">Haewon,</span> <span style="color: #e8364c">Jiwoo</span></p>
              <div class="wp-block-columns">
                <div class="wp-block-column">
                  <p><strong>Romanization</strong></p>
                  <p><span style="color: #e8ffe8;">We’re blooming</span><br/><span style="color: #e8364c;">eorin mamsok hemaedeon cosmos</span></p>
                </div>
                <div class="wp-block-column">
                  <p><strong>Hangul</strong></p>
                  <p><span style="color: #e8ffe8;">We’re blooming</span><br/><span style="color: #e8364c;">어린 맘속 헤매던 cosmos</span></p>
                </div>
                <div class="wp-block-column">
                  <p><strong>Translation</strong></p>
                  <p>We’re blooming<br/>My childish heart once lost in the cosmos</p>
                </div>
              </div>
            </div>
        "##;
        let package = parse_colorcodedlyrics_html(html, None).unwrap();
        assert_eq!(package.members[1].stage_name, "Haewon");
        assert_eq!(package.lines.len(), 2);
        assert_eq!(package.lines[1].member.as_deref(), Some("Jiwoo"));
        assert_eq!(package.lines[1].original, "어린 맘속 헤매던 cosmos");
        assert_eq!(
            package.lines[1].romanization.as_deref(),
            Some("eorin mamsok hemaedeon cosmos")
        );
        assert_eq!(
            package.lines[1].english.as_deref(),
            Some("My childish heart once lost in the cosmos")
        );
    }

    #[test]
    fn keeps_column_variants_aligned_across_uneven_stanzas() {
        let html = r##"
            <h1 class="entry-title">NMIXX - Heavy Serenade Lyrics</h1>
            <div class="entry-content">
              <p style="text-align:center"><span style="color: #e8364c">Jiwoo</span>, <span style="color: #f093bc">Kyujin</span></p>
              <div class="wp-block-columns">
                <div class="wp-block-column">
                  <p><strong>Romanization</strong></p>
                  <p><span style="color: #e8364c;">one</span><br/><span style="color: #e8364c;">two</span><br/><span style="color: #e8364c;">three</span></p>
                  <p><span style="color: #f093bc;">Then I realize</span><br/><span style="color: #f093bc;">Run run run</span></p>
                </div>
                <div class="wp-block-column">
                  <p><strong>Hangul</strong></p>
                  <p><span style="color: #e8364c;">하나</span><br/><span style="color: #e8364c;">둘</span><br/><span style="color: #e8364c;">셋</span></p>
                  <p><span style="color: #f093bc;">Then I realize</span><br/><span style="color: #f093bc;">Run run run</span></p>
                </div>
                <div class="wp-block-column">
                  <p><strong>Translation</strong></p>
                  <p>one<br/>two and three merged</p>
                  <p>Then I realize<br/>Run run run</p>
                </div>
              </div>
            </div>
        "##;
        let package = parse_colorcodedlyrics_html(html, None).unwrap();
        let then_line = package
            .lines
            .iter()
            .find(|line| line.original == "Then I realize")
            .unwrap();
        assert_eq!(then_line.romanization.as_deref(), Some("Then I realize"));
        assert_eq!(then_line.english.as_deref(), Some("Then I realize"));
        assert_eq!(then_line.member.as_deref(), Some("Kyujin"));
    }

    #[test]
    fn preserves_uncolored_lines_inside_colored_stanzas() {
        let html = r##"
            <h1 class="entry-title">NMIXX - Heavy Serenade Lyrics</h1>
            <div class="entry-content">
              <p style="text-align:center"><span style="color: #e8364c">Jiwoo</span></p>
              <div class="wp-block-columns">
                <div class="wp-block-column">
                  <p><strong>Romanization</strong></p>
                  <p><span style="color: #e8364c;">L O V E, right?</span><br/><span style="color: #e8364c;">L O V E, right?</span><br/>(I don’t doubt it!)<br/><span style="color: #e8364c;">Be my, be my light!</span></p>
                </div>
                <div class="wp-block-column">
                  <p><strong>Hangul</strong></p>
                  <p><span style="color: #e8364c;">L O V E, right?</span><br/><span style="color: #e8364c;">L O V E, right?</span><br/>(I don’t doubt it!)<br/><span style="color: #e8364c;">Be my, be my light!</span></p>
                </div>
                <div class="wp-block-column">
                  <p><strong>Translation</strong></p>
                  <p>L O V E, right?<br/>L O V E, right?<br/>(I don’t doubt it!)<br/>Be my, be my light!</p>
                </div>
              </div>
            </div>
        "##;
        let package = parse_colorcodedlyrics_html(html, None).unwrap();
        assert_eq!(package.lines[2].original, "(I don’t doubt it!)");
        assert_eq!(
            package.lines[2].english.as_deref(),
            Some("(I don’t doubt it!)")
        );
        assert_eq!(package.lines[3].original, "Be my, be my light!");
        assert_eq!(
            package.lines[3].english.as_deref(),
            Some("Be my, be my light!")
        );
    }

    #[test]
    fn splits_nested_inline_member_runs() {
        let html = r##"
            <h1 class="entry-title">NMIXX - Heavy Serenade Lyrics</h1>
            <div class="entry-content">
              <p style="text-align:center"><span style="color: #e8364c">Jiwoo</span>, <span style="color: #fff677">BAE</span>, <span style="color: #1549c2">Sullyoon</span></p>
              <div class="wp-block-columns">
                <div class="wp-block-column">
                  <p><strong>Romanization</strong></p>
                  <p><span style="color: #e8364c;">Run <span style="color: #fff677;">run</span> <span style="color: #1549c2;">run</span></span></p>
                </div>
                <div class="wp-block-column">
                  <p><strong>Hangul</strong></p>
                  <p><span style="color: #e8364c;">Run <span style="color: #fff677;">run</span> <span style="color: #1549c2;">run</span></span></p>
                </div>
                <div class="wp-block-column">
                  <p><strong>Translation</strong></p>
                  <p>Run run run</p>
                </div>
              </div>
            </div>
        "##;
        let package = parse_colorcodedlyrics_html(html, None).unwrap();
        assert_eq!(package.lines.len(), 1);
        assert_eq!(
            package.lines[0].member.as_deref(),
            Some("Jiwoo, BAE, Sullyoon")
        );
        assert_eq!(package.lines[0].original, "Run run run");
        assert_eq!(package.lines[0].english.as_deref(), Some("Run run run"));
        let original_segments: Vec<_> = package.lines[0]
            .segments
            .iter()
            .filter(|segment| segment.language == "original")
            .collect();
        assert_eq!(original_segments.len(), 3);
        assert_eq!(original_segments[0].member.as_deref(), Some("Jiwoo"));
        assert_eq!(original_segments[1].member.as_deref(), Some("BAE"));
        assert_eq!(original_segments[2].member.as_deref(), Some("Sullyoon"));
    }

    #[test]
    fn selects_best_colorcodedlyrics_search_result() {
        let html = r#"
            <article><h2><a href="https://colorcodedlyrics.com/2026/05/01/nmixx-loud/">NMIXX - LOUD</a></h2></article>
            <article><h2><a href="https://colorcodedlyrics.com/2026/05/17/nmixx-heavy-serenade/">NMIXX - Heavy Serenade</a></h2></article>
        "#;
        let document = Html::parse_document(html);
        let link =
            select_best_colorcodedlyrics_link(&document, "NMIXX(엔믹스) Heavy Serenade").unwrap();
        assert!(link.ends_with("/nmixx-heavy-serenade/"));
    }

    #[test]
    fn parses_genius_fixture() {
        let html = r#"<h1>GROUP - Song Lyrics</h1><div data-lyrics-container="true">One: line<br/>Two: line</div>"#;
        let package = parse_genius_html(html, None).unwrap();
        assert_eq!(package.provider, "genius");
        assert_eq!(package.members.len(), 2);
    }
}
