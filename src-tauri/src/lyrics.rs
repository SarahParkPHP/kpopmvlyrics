use anyhow::{anyhow, Result};
use ego_tree::NodeRef;
use html_escape::decode_html_entities;
use regex::Regex;
use reqwest::blocking::Client;
use scraper::{ElementRef, Html, Node, Selector};
use serde::Deserialize;
use strsim::jaro_winkler;

use crate::models::{LyricLine, LyricSegment, MemberProfile, Song, SongPackage};
use crate::process_util::http_client;

pub trait LyricsProvider {
    fn fetch(&self, query: &str) -> Result<SongPackage>;
}

pub struct ColorCodedLyricsProvider {
    client: Client,
}

impl Default for ColorCodedLyricsProvider {
    fn default() -> Self {
        Self {
            client: http_client("kpopmvlyrics/0.1"),
        }
    }
}

impl LyricsProvider for ColorCodedLyricsProvider {
    fn fetch(&self, query: &str) -> Result<SongPackage> {
        let mut best: Option<(f64, String)> = None;
        let mut consider = |score: f64, link: String| {
            if best
                .as_ref()
                .map(|(best_score, _)| score > *best_score)
                .unwrap_or(true)
            {
                best = Some((score, link));
            }
        };

        for search_query in lyrics_search_queries(query) {
            let search_url = format!(
                "https://colorcodedlyrics.com/?s={}",
                urlencoding(&search_query)
            );
            let search_html = self.client.get(search_url).send()?.text()?;
            let document = Html::parse_document(&search_html);
            if let Some((score, link)) = rank_best_colorcodedlyrics_link(&document, query) {
                consider(score, link);
            }
        }

        for slug in colorcodedlyrics_slug_candidates(query) {
            if let Some((link, title)) = lookup_colorcodedlyrics_by_slug(&self.client, &slug) {
                let score = score_colorcodedlyrics_match(query, &title, &link);
                consider(score, link);
            }
        }

        let (_, link) =
            best.ok_or_else(|| anyhow!("ColorCodedLyrics result not found for {query}"))?;
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
            client: http_client("kpopmvlyrics/0.1"),
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
    let (mut lines, color_members) = parse_colorcodedlyrics_content(&content_html);
    if lines.is_empty() {
        return Err(anyhow!("No ColorCodedLyrics lines parsed"));
    }
    canonicalize_line_member_names(&mut lines, &color_members);
    let (artist, title) = split_artist_title(&title);
    Ok(SongPackage {
        song: Song {
            id: None,
            title,
            artist: artist.clone(),
            group_name: Some(artist),
            source_url,
        },
        members: members_for_lines(&lines, &color_members),
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
                with_all: false,
                segments: Vec::new(),
            }
        })
        .collect()
}

fn parse_colorcodedlyrics_content(html: &str) -> (Vec<LyricLine>, Vec<MemberProfile>) {
    let color_span_re =
        Regex::new(r#"(?is)<span[^>]*style="[^"]*color:\s*([^;"']+)[^"]*"[^>]*>(.*?)</span>"#)
            .expect("valid regex");
    let blocks = content_blocks(html);

    let mut color_names: Vec<(String, String)> = Vec::new();
    for block_html in &blocks {
        let block_text = strip_tags(block_html);
        if looks_like_credits_block(&block_text) {
            continue;
        }
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
            && looks_like_member_legend(&spans)
            && spans.len() > color_names.len()
        {
            color_names = spans;
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
        if let Some(marker) = parse_language_marker(block_html, &block_text) {
            if !lines.is_empty() {
                break;
            }
            active_language = Some(marker);
            continue;
        }
        if block_text.to_lowercase().contains("credits")
            || block_text.to_lowercase().contains("disclaimer")
        {
            break;
        }
        if active_language.is_none() {
            continue;
        }

        for column_line in parsed_lines_from_block(block_html) {
            push_parsed_lyric_line(
                &mut lines,
                column_line,
                active_language.as_deref().unwrap_or("original"),
                &color_to_member,
            );
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

fn looks_like_credits_block(block_text: &str) -> bool {
    let lower = block_text.to_lowercase();
    lower.contains("작사")
        || lower.contains("작곡")
        || lower.contains("편곡")
        || lower.contains("lyrics/")
        || lower.contains("composer/")
        || lower.contains("arranger/")
}

fn looks_like_member_legend(spans: &[(String, String)]) -> bool {
    let unique_colors: std::collections::HashSet<&str> =
        spans.iter().map(|(color, _)| color.as_str()).collect();
    if unique_colors.len() < spans.len() {
        return false;
    }
    spans.iter().all(|(_, text)| {
        let text = clean_member_name(text);
        text.split_whitespace().count() <= 3
            && is_latin_member_name(&text)
            && looks_like_stage_name(&text)
    })
}

fn looks_like_stage_name(text: &str) -> bool {
    text.split_whitespace().any(|word| {
        word.chars()
            .find(|ch| ch.is_alphabetic())
            .map(|ch| ch.is_uppercase())
            .unwrap_or(false)
    })
}

fn is_latin_member_name(text: &str) -> bool {
    let stripped: String = text
        .chars()
        .filter(|ch| {
            !ch.is_whitespace()
                && *ch != ','
                && *ch != '.'
                && *ch != '-'
                && *ch != '_'
                && *ch != '&'
        })
        .collect();
    !stripped.is_empty()
        && stripped
            .chars()
            .all(|ch| ch.is_ascii_alphabetic() || ch.is_ascii_digit())
}

pub fn referenced_member_names(lines: &[LyricLine]) -> Vec<String> {
    let mut names = Vec::new();
    for line in lines {
        if let Some(member) = &line.member {
            push_member_names(&mut names, member);
        }
        for segment in &line.segments {
            if let Some(member) = &segment.member {
                push_member_names(&mut names, member);
            }
        }
    }
    names
}

fn member_aliases(member: &MemberProfile) -> Vec<String> {
    let mut aliases = vec![member.stage_name.clone()];
    if let Some(real_name) = &member.real_name {
        aliases.push(real_name.clone());
    }
    match member.stage_name.to_ascii_uppercase().as_str() {
        "HAN" => aliases.push("한".into()),
        "FELIX" => aliases.extend(["필릭스".to_string(), "필".to_string()]),
        _ => {}
    }
    aliases
}

pub fn member_reference_matches(raw: &str, member: &MemberProfile, roster: &[MemberProfile]) -> bool {
    let raw = raw.trim();
    if raw.is_empty() {
        return false;
    }
    if raw.eq_ignore_ascii_case("all") {
        return false;
    }
    if member_aliases(member)
        .iter()
        .any(|alias| alias.eq_ignore_ascii_case(raw))
    {
        return true;
    }
    resolve_member_name(raw, roster)
        .is_some_and(|resolved| resolved.eq_ignore_ascii_case(&member.stage_name))
}

pub fn canonical_member_name(raw: &str, roster: &[MemberProfile]) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() || raw.eq_ignore_ascii_case("all") {
        return None;
    }
    if let Some(resolved) = resolve_member_name(raw, roster) {
        return Some(resolved);
    }
    roster
        .iter()
        .find(|member| member_reference_matches(raw, member, roster))
        .map(|member| member.stage_name.clone())
}

pub fn canonical_referenced_members(lines: &[LyricLine], roster: &[MemberProfile]) -> Vec<String> {
    let mut names = Vec::new();
    for raw in referenced_member_names(lines) {
        if let Some(name) = canonical_member_name(&raw, roster) {
            if !names
                .iter()
                .any(|existing: &String| existing.eq_ignore_ascii_case(&name))
            {
                names.push(name);
            }
        }
    }
    names
}

pub fn canonicalize_line_member_names(lines: &mut [LyricLine], roster: &[MemberProfile]) {
    for line in lines.iter_mut() {
        if let Some(member) = line.member.clone() {
            let canonical: Vec<String> = member
                .split(',')
                .filter_map(|part| canonical_member_name(part, roster))
                .collect();
            line.member = (!canonical.is_empty()).then(|| canonical.join(", "));
        }
        for segment in &mut line.segments {
            if let Some(member) = segment.member.clone() {
                segment.member = canonical_member_name(&member, roster);
            }
        }
    }
}

fn push_member_names(names: &mut Vec<String>, raw: &str) {
    for part in raw.split(',') {
        let name = clean_member_name(part);
        if name.is_empty() || name.eq_ignore_ascii_case("all") {
            continue;
        }
        if names
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(&name))
        {
            continue;
        }
        names.push(name);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberHighlight {
    pub primary: Vec<String>,
    pub backing: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberLineTag {
    pub parts: Vec<String>,
    pub with_all: bool,
}

pub fn strip_member_line_tag(text: &str) -> (String, MemberLineTag) {
    static TAG_RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = TAG_RE.get_or_init(|| Regex::new(r"(?i)^\[\s*([^\]]+?)\s*\]\s*").expect("valid regex"));
    let trimmed = text.trim();
    let Some(captures) = re.captures(trimmed) else {
        return (
            trimmed.to_string(),
            MemberLineTag {
                parts: Vec::new(),
                with_all: false,
            },
        );
    };
    let parts: Vec<String> = captures
        .get(1)
        .map(|value| value.as_str())
        .unwrap_or_default()
        .split('/')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    let cleaned = re.replace(trimmed, "").trim().to_string();
    let with_all = parts.iter().any(|part| part.eq_ignore_ascii_case("all"));
    (cleaned, MemberLineTag { parts, with_all })
}

pub fn strip_member_singer_tag(text: &str) -> (String, Option<String>, bool) {
    let (cleaned, tag) = strip_member_line_tag(text);
    if tag.parts.is_empty() {
        return (cleaned, None, false);
    }
    let primary = tag.parts.first().cloned();
    (cleaned, primary, tag.with_all)
}

pub fn normalize_line_member_tags(line: &mut LyricLine) {
    let mut with_all = line.with_all;
    let mut named_parts = Vec::new();

    let mut apply_tag = |text: &str| -> String {
        let (cleaned, tag) = strip_member_line_tag(text);
        if tag.with_all {
            with_all = true;
            if line.member.is_none() {
                line.member = tag.parts.first().cloned();
            }
        } else if tag.parts.len() >= 2 {
            for part in tag.parts {
                if !named_parts
                    .iter()
                    .any(|existing: &String| existing.eq_ignore_ascii_case(&part))
                {
                    named_parts.push(part);
                }
            }
        }
        cleaned
    };

    line.original = apply_tag(&line.original);
    if let Some(romanization) = line.romanization.take() {
        line.romanization = Some(apply_tag(&romanization)).filter(|text| !text.is_empty());
    }
    if let Some(english) = line.english.take() {
        line.english = Some(apply_tag(&english)).filter(|text| !text.is_empty());
    }
    for segment in &mut line.segments {
        segment.text = apply_tag(&segment.text);
    }

    line.with_all = with_all;
    if !named_parts.is_empty() {
        line.member = Some(named_parts.join(", "));
    }
}

fn member_initials(name: &str) -> String {
    name.split(|ch: char| ch.is_whitespace() || ch == '.')
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.chars().find(|ch| ch.is_alphabetic()))
        .map(|ch| ch.to_ascii_uppercase())
        .collect()
}

fn is_consonant(ch: char) -> bool {
    matches!(
        ch.to_ascii_lowercase(),
        'b' | 'c' | 'd' | 'f' | 'g' | 'h' | 'j' | 'k' | 'l' | 'm' | 'n' | 'p' | 'q' | 'r'
            | 's' | 't' | 'v' | 'w' | 'x' | 'y' | 'z'
    )
}

fn single_word_two_char_abbrev(name: &str) -> String {
    let letters: Vec<char> = name.chars().filter(|ch| ch.is_alphabetic()).collect();
    if letters.len() < 2 {
        return letters
            .first()
            .map(|ch| ch.to_ascii_uppercase().to_string())
            .unwrap_or_default();
    }
    let first = letters[0].to_ascii_uppercase();
    for ch in letters.iter().skip(1) {
        if is_consonant(*ch) {
            return format!("{first}{}", ch.to_ascii_uppercase());
        }
    }
    format!("{first}{}", letters[1].to_ascii_uppercase())
}

fn member_abbreviations(name: &str) -> Vec<String> {
    let mut abbrevs = vec![name.to_string()];
    let initials = member_initials(name);
    if !initials.is_empty() {
        abbrevs.push(initials);
    }
    if name.split_whitespace().count() == 1 {
        abbrevs.push(single_word_two_char_abbrev(name));
        if let Some(ch) = name.chars().next() {
            abbrevs.push(ch.to_string());
            abbrevs.push(ch.to_ascii_uppercase().to_string());
        }
    }
    abbrevs.sort_by_key(|value| value.len());
    abbrevs.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    abbrevs
}

pub fn resolve_member_name(tag: &str, roster: &[MemberProfile]) -> Option<String> {
    let tag = tag.trim();
    if tag.is_empty() {
        return None;
    }
    if let Some(member) = roster
        .iter()
        .find(|member| member.stage_name.eq_ignore_ascii_case(tag))
    {
        return Some(member.stage_name.clone());
    }

    let mut matches: Vec<&MemberProfile> = roster
        .iter()
        .filter(|member| {
            member_abbreviations(&member.stage_name)
                .iter()
                .any(|abbrev| abbrev.eq_ignore_ascii_case(tag))
        })
        .collect();

    if matches.len() > 1 && tag.len() == 1 {
        matches.retain(|member| {
            member.stage_name.split_whitespace().count() == 1
                && member.stage_name.chars().count() <= 4
        });
    }

    if matches.len() == 1 {
        return Some(matches[0].stage_name.clone());
    }

    None
}

fn push_highlight_name(names: &mut Vec<String>, roster: &[MemberProfile], raw: &str) {
    let resolved = resolve_member_name(raw, roster).unwrap_or_else(|| raw.to_string());
    if names
        .iter()
        .any(|name| name.eq_ignore_ascii_case(&resolved))
    {
        return;
    }
    names.push(resolved);
}

pub fn member_highlight_for_line(line: &LyricLine, roster: &[MemberProfile]) -> MemberHighlight {
    if line.with_all {
        let mut primary = Vec::new();
        if let Some(member) = &line.member {
            push_highlight_name(&mut primary, roster, member);
        }
        let backing = roster
            .iter()
            .map(|member| member.stage_name.clone())
            .filter(|name| !primary.iter().any(|primary| primary.eq_ignore_ascii_case(name)))
            .collect();
        return MemberHighlight { primary, backing };
    }

    MemberHighlight {
        primary: referenced_member_names(std::slice::from_ref(line))
            .into_iter()
            .map(|name| resolve_member_name(&name, roster).unwrap_or(name))
            .collect(),
        backing: Vec::new(),
    }
}

pub fn members_for_lines(lines: &[LyricLine], legend: &[MemberProfile]) -> Vec<MemberProfile> {
    let referenced = referenced_member_names(lines);
    if referenced.is_empty() {
        return members_from_lines(lines);
    }
    if legend.is_empty() {
        return members_from_lines(lines);
    }

    legend
        .iter()
        .filter(|member| {
            referenced
                .iter()
                .any(|raw| member_reference_matches(raw, member, legend))
        })
        .cloned()
        .collect()
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

    let english_only = romanization.is_empty() && hangul.is_empty() && !translation.is_empty();
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
            let source_line = korean.or(roman).or(english);
            let original = if english_only {
                String::new()
            } else {
                source_line
                    .map(|line| line.text.trim().to_string())
                    .unwrap_or_default()
            };
            if (original.is_empty() && !english_only) || looks_like_metadata(&original) {
                if english_only {
                    if english
                        .is_none_or(|line| line.text.trim().is_empty() || looks_like_metadata(&line.text))
                    {
                        continue;
                    }
                } else {
                    continue;
                }
            }
            let member = aggregate_members(korean.or(roman).or(english), color_to_member);
            let with_all = source_line.is_some_and(parsed_column_is_all_members);
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
                with_all,
                segments: lyric_segments(roman, korean, english, color_to_member),
            });
            if let Some(line) = lines.last_mut() {
                inherit_members_on_segments(line);
                normalize_line_member_tags(line);
            }
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
            if let Some(parsed) = parse_language_marker(&paragraph.html(), &marker) {
                language = Some(parsed);
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
        } else if !line.text.trim().is_empty() {
            output.push(LyricSegment {
                language: language.to_string(),
                text: line.text.clone(),
                member: None,
                color: None,
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
        if !line.text.trim().is_empty() {
            output.push(LyricSegment {
                language: language.to_string(),
                text: line.text.clone(),
                member: None,
                color: None,
            });
        }
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

fn inherit_members_on_segments(line: &mut LyricLine) {
    let Some(line_member) = line.member.clone() else {
        return;
    };
    let reference_members: Vec<(String, String)> = line
        .segments
        .iter()
        .filter(|segment| segment.language == "original" || segment.language == "romanization")
        .filter_map(|segment| {
            segment
                .member
                .as_ref()
                .and_then(|member| segment.color.as_ref().map(|color| (member.clone(), color.clone())))
        })
        .collect();
    for segment in &mut line.segments {
        if segment.member.is_some() {
            continue;
        }
        if segment.language == "english" {
            if reference_members.len() == 1 {
                let (member, color) = &reference_members[0];
                segment.member = Some(member.clone());
                if segment.color.is_none() {
                    segment.color = Some(color.clone());
                }
            } else if !line_member.contains(',') {
                segment.member = Some(line_member.clone());
            }
        }
    }
}

fn select_best_colorcodedlyrics_link(document: &Html, query: &str) -> Option<String> {
    rank_best_colorcodedlyrics_link(document, query).map(|(_, href)| href)
}

fn rank_best_colorcodedlyrics_link(document: &Html, query: &str) -> Option<(f64, String)> {
    let selector = Selector::parse("h2 a, article a, .entry-title a").ok()?;

    document
        .select(&selector)
        .filter_map(|node| {
            let href = node.value().attr("href")?;
            if !href.contains("colorcodedlyrics.com") {
                return None;
            }
            let text = node.text().collect::<Vec<_>>().join(" ");
            let score = score_colorcodedlyrics_match(query, &text, href);
            Some((score, href.to_string()))
        })
        .max_by(|left, right| {
            left.0
                .partial_cmp(&right.0)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
}

fn score_colorcodedlyrics_match(query: &str, title: &str, href: &str) -> f64 {
    let query_key = search_key(query);
    let text_key = search_key(title);
    let href_key = search_key(href);
    let candidate_key = format!("{text_key} {href_key}");
    let mut score = jaro_winkler(&query_key, &text_key);
    let song_tokens = song_search_tokens(query);
    if !song_tokens.is_empty() {
        let song_hits = song_tokens
            .iter()
            .filter(|token| token_matches_candidate(token, &candidate_key))
            .count() as f64;
        let song_coverage = song_hits / song_tokens.len() as f64;
        score += song_coverage * 3.0;
        if song_hits == 0.0 {
            score -= 1.5;
        }
    }
    score
}

#[derive(Debug, Deserialize)]
struct ColorCodedLyricsPost {
    link: String,
    title: ColorCodedLyricsPostTitle,
}

#[derive(Debug, Deserialize)]
struct ColorCodedLyricsPostTitle {
    rendered: String,
}

fn lookup_colorcodedlyrics_by_slug(client: &Client, slug: &str) -> Option<(String, String)> {
    let url = format!(
        "https://colorcodedlyrics.com/wp-json/wp/v2/posts?slug={}&_fields=link,title&per_page=1",
        urlencoding(slug)
    );
    let posts: Vec<ColorCodedLyricsPost> = client.get(url).send().ok()?.json().ok()?;
    let post = posts.first()?;
    Some((post.link.clone(), post.title.rendered.clone()))
}

fn slugify_colorcodedlyrics(value: &str) -> String {
    search_key(value)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-")
}

fn push_slug_candidate(slugs: &mut Vec<String>, value: &str) {
    let slug = slugify_colorcodedlyrics(value);
    if slug.is_empty() || slugs.iter().any(|existing| existing == &slug) {
        return;
    }
    slugs.push(slug);
}

fn colorcodedlyrics_slug_candidates(query: &str) -> Vec<String> {
    let mut slugs = Vec::new();
    push_slug_candidate(&mut slugs, query);

    let (artist, title) = split_video_query(query);
    if let Some(artist) = artist {
        push_slug_candidate(&mut slugs, &format!("{artist} {title}"));
        push_slug_candidate(&mut slugs, &format!("{artist} {}", strip_parentheticals(&title)));
        for phrase in parenthetical_phrases(&title) {
            push_slug_candidate(&mut slugs, &format!("{artist} {phrase}"));
        }
        return slugs;
    }

    let words: Vec<_> = query.split_whitespace().collect();
    if words.len() >= 2 {
        let artist = words[0];
        let title = words[1..].join(" ");
        push_slug_candidate(&mut slugs, &format!("{artist} {title}"));
    }

    slugs
}

fn lyrics_search_queries(query: &str) -> Vec<String> {
    let mut queries = vec![query.trim().to_string()];
    let (artist, title) = split_video_query(query);
    let Some(artist) = artist else {
        return dedupe_queries(queries);
    };

    for romanization in parenthetical_phrases(&title) {
        queries.push(format!("{artist} {romanization}"));
        for suffix in ["teuk", "teug"] {
            queries.push(format!("{artist} {romanization} {suffix}"));
        }
    }

    if title.contains(|ch: char| ('\u{AC00}'..='\u{D7A3}').contains(&ch)) {
        for suffix in ["teuk", "teug"] {
            queries.push(format!("{artist} {suffix}"));
        }
        let hangul_title = strip_parentheticals(&title);
        if !hangul_title.is_empty() {
            queries.push(format!("{artist} {hangul_title}"));
        }
    }

    dedupe_queries(queries)
}

fn dedupe_queries(queries: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    queries
        .into_iter()
        .filter(|query| {
            let key = search_key(query);
            !key.is_empty() && seen.insert(key)
        })
        .collect()
}

fn split_video_query(query: &str) -> (Option<String>, String) {
    let cleaned = query.trim();
    if let Some((artist, title)) = cleaned
        .split_once(" – ")
        .or_else(|| cleaned.split_once(" - "))
    {
        return (Some(artist.trim().to_string()), title.trim().to_string());
    }
    if let Some(caps) = Regex::new(r"^(.+?\([^)]+\))\s+(.+)$")
        .ok()
        .and_then(|re| re.captures(cleaned))
    {
        return (
            Some(caps[1].trim().to_string()),
            caps[2].trim().to_string(),
        );
    }
    if let Some(caps) = Regex::new(r"^(.+?)\s+([\p{Hangul}].*)$")
        .ok()
        .and_then(|re| re.captures(cleaned))
    {
        return (
            Some(caps[1].trim().to_string()),
            caps[2].trim().to_string(),
        );
    }
    (None, cleaned.to_string())
}

fn parenthetical_phrases(value: &str) -> Vec<String> {
    Regex::new(r"\(([^)]+)\)")
        .ok()
        .map(|re| {
            re.captures_iter(value)
                .filter_map(|cap| cap.get(1).map(|part| part.as_str().trim().to_string()))
                .filter(|part| !part.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn strip_parentheticals(value: &str) -> String {
    Regex::new(r"\([^)]*\)")
        .ok()
        .map(|re| re.replace_all(value, " ").to_string())
        .unwrap_or_else(|| value.to_string())
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn song_search_tokens(query: &str) -> Vec<String> {
    let (_, title) = split_video_query(query);
    let mut tokens = Vec::new();
    for phrase in parenthetical_phrases(&title) {
        push_search_token(&mut tokens, &search_key(&phrase));
        push_search_token(&mut tokens, &phrase.to_lowercase().replace(' ', "-"));
    }
    for token in search_key(&strip_parentheticals(&title)).split_whitespace() {
        if token.len() > 1 {
            push_search_token(&mut tokens, token);
        }
    }
    tokens
}

fn push_search_token(tokens: &mut Vec<String>, token: &str) {
    let token = token.trim();
    if token.is_empty() {
        return;
    }
    if tokens
        .iter()
        .any(|existing| existing.eq_ignore_ascii_case(token))
    {
        return;
    }
    tokens.push(token.to_string());
}

fn token_matches_candidate(token: &str, candidate: &str) -> bool {
    if candidate.contains(token) {
        return true;
    }
    let compact = token.replace([' ', '-', '_'], "");
    if compact.len() > 2 && candidate.replace([' ', '-', '_'], "").contains(&compact) {
        return true;
    }
    let hyphenated = token.replace(' ', "-");
    candidate.contains(&hyphenated)
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

fn parse_language_marker(block_html: &str, block_text: &str) -> Option<String> {
    if let Some(language) = normalize_language_marker(&block_text.trim().to_lowercase()) {
        return Some(language);
    }
    let class_marker_re = Regex::new(
        r#"(?is)<span[^>]*has-very-light-gray-color[^>]*>(.*?)</span>"#,
    )
    .ok()?;
    for cap in class_marker_re.captures_iter(block_html) {
        let marker = strip_tags(cap.get(1)?.as_str()).trim().to_lowercase();
        if let Some(language) = normalize_language_marker(&marker) {
            return Some(language);
        }
    }
    None
}

fn normalize_language_marker(marker: &str) -> Option<String> {
    match marker {
        "english" | "translation" => Some("english".into()),
        "romanization" => Some("romanization".into()),
        "hangul" | "korean" => Some("korean".into()),
        _ => None,
    }
}

pub fn lyric_language_toggles(lines: &[LyricLine]) -> (bool, bool, bool) {
    let has_original = lines.iter().any(|line| !line.original.trim().is_empty());
    let has_romanization = lines.iter().any(|line| {
        line.romanization
            .as_ref()
            .is_some_and(|text| !text.trim().is_empty())
    });
    let has_english = lines.iter().any(|line| {
        line.english
            .as_ref()
            .is_some_and(|text| !text.trim().is_empty())
    });
    (
        has_original,
        has_romanization,
        has_english,
    )
}

fn push_parsed_lyric_line(
    lines: &mut Vec<LyricLine>,
    column_line: ParsedColumnLine,
    language: &str,
    color_to_member: &std::collections::HashMap<String, String>,
) {
    let text = column_line.text.trim();
    if text.is_empty() || looks_like_metadata(text) {
        return;
    }

    let member = aggregate_members(Some(&column_line), color_to_member);
    let with_all = parsed_column_is_all_members(&column_line);
    let mut segments = Vec::new();
    push_lyric_segments(
        &mut segments,
        language,
        Some(&column_line),
        color_to_member,
    );

    let mut line = LyricLine {
        id: None,
        song_id: None,
        index: lines.len(),
        member,
        original: String::new(),
        romanization: None,
        english: None,
        with_all,
        segments,
    };
    match language {
        "english" => line.english = Some(text.to_string()),
        "romanization" => line.romanization = Some(text.to_string()),
        _ => line.original = text.to_string(),
    }
    lines.push(line);
    if let Some(line) = lines.last_mut() {
        normalize_line_member_tags(line);
    }
}

fn parsed_column_is_all_members(line: &ParsedColumnLine) -> bool {
    if line.segments.is_empty() {
        return line.color.is_none();
    }
    line.segments.iter().all(|segment| segment.color.is_none())
}

fn push_lyric_line(
    lines: &mut Vec<LyricLine>,
    member: Option<String>,
    text: String,
    language: Option<&str>,
) {
    let text = text.trim();
    if text.is_empty() || looks_like_metadata(text) {
        return;
    }
    let mut line = LyricLine {
        id: None,
        song_id: None,
        index: lines.len(),
        member,
        original: String::new(),
        romanization: None,
        english: None,
        with_all: false,
        segments: Vec::new(),
    };
    match language {
        Some("english") => line.english = Some(text.to_string()),
        Some("romanization") => line.romanization = Some(text.to_string()),
        _ => line.original = text.to_string(),
    }
    lines.push(line);
    if let Some(line) = lines.last_mut() {
        normalize_line_member_tags(line);
    }
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
        canonicalize_line_member_names, colorcodedlyrics_slug_candidates, ColorCodedLyricsProvider,
        lyric_language_toggles, lyrics_search_queries, member_highlight_for_line, members_for_lines,
        normalize_line_member_tags, parse_colorcodedlyrics_html, parse_genius_html,
        parse_manual_lyrics, rank_best_colorcodedlyrics_link, resolve_member_name,
        score_colorcodedlyrics_match, select_best_colorcodedlyrics_link, song_search_tokens,
        strip_member_line_tag, strip_member_singer_tag, LyricsProvider,
    };
    use crate::models::{LyricLine, MemberProfile};
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
        assert_eq!(
            package.lines[0].english.as_deref(),
            Some("On my chest")
        );
        assert!(package.lines[0].original.is_empty());
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
        assert_eq!(
            package.lines[0].romanization.as_deref(),
            Some("Baby say goodbye if you see her")
        );
        assert!(package.lines[0].original.is_empty());
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
        assert_eq!(
            package
                .members
                .iter()
                .map(|member| member.stage_name.as_str())
                .collect::<Vec<_>>(),
            vec!["Haewon", "Jiwoo"]
        );
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
    fn builds_slug_candidate_for_twice_this_is_for() {
        let slugs = colorcodedlyrics_slug_candidates("TWICE THIS IS FOR");
        assert!(slugs.iter().any(|slug| slug == "twice-this-is-for"));
    }

    #[test]
    fn scores_twice_this_is_for_slug_match_high() {
        let score = score_colorcodedlyrics_match(
            "TWICE THIS IS FOR",
            "TWICE - THIS IS FOR",
            "https://colorcodedlyrics.com/2025/07/11/twice-this-is-for/",
        );
        assert!(score > 0.8);
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
    fn prefers_s_class_over_other_stray_kids_results() {
        let html = r#"
            <article><h2><a href="https://colorcodedlyrics.com/2025/08/22/stray-kids-mess-eongmang/">Stray Kids - MESS (엉망)</a></h2></article>
            <article><h2><a href="https://colorcodedlyrics.com/2023/06/01/stray-kids-s-class-teug/">Stray Kids - S-Class (특)</a></h2></article>
        "#;
        let document = Html::parse_document(html);
        let link =
            select_best_colorcodedlyrics_link(&document, "Stray Kids 특(S-Class)").unwrap();
        assert!(link.ends_with("/stray-kids-s-class-teug/"));
    }

    #[test]
    fn builds_teuk_search_variants_for_hangul_titles() {
        let queries = lyrics_search_queries("Stray Kids 특(S-Class)");
        assert!(queries.iter().any(|query| query.contains("S-Class")));
        assert!(queries.iter().any(|query| query.contains("teuk")));
    }

    #[test]
    fn song_search_tokens_include_parenthetical_title() {
        let tokens = song_search_tokens("Stray Kids 특(S-Class)");
        assert!(tokens.iter().any(|token| token.contains("class")));
    }

    #[test]
    fn ranks_s_class_above_mess_when_both_present() {
        let html = r#"
            <article><h2><a href="https://colorcodedlyrics.com/2025/08/22/stray-kids-mess-eongmang/">Stray Kids - MESS (엉망)</a></h2></article>
            <article><h2><a href="https://colorcodedlyrics.com/2023/06/01/stray-kids-s-class-teug/">Stray Kids - S-Class (특)</a></h2></article>
        "#;
        let document = Html::parse_document(html);
        let mess = rank_best_colorcodedlyrics_link(&document, "Stray Kids 특(S-Class)").unwrap();
        let html = r#"
            <article><h2><a href="https://colorcodedlyrics.com/2025/08/22/stray-kids-mess-eongmang/">Stray Kids - MESS (엉망)</a></h2></article>
        "#;
        let document = Html::parse_document(html);
        let only_mess =
            rank_best_colorcodedlyrics_link(&document, "Stray Kids 특(S-Class)").unwrap();
        assert!(mess.0 > only_mess.0);
        assert!(mess.1.ends_with("/stray-kids-s-class-teug/"));
    }

    #[test]
    fn ignores_credits_block_when_selecting_member_legend() {
        let html = r##"
            <h1 class="entry-title">Stray Kids - S-Class Lyrics</h1>
            <div class="entry-content">
              <p><span style="color: #111">방찬 (3RACHA)</span>, <span style="color: #222">창빈 (3RACHA)</span>, <span style="color: #333">한 (3RACHA)</span></p>
              <p style="text-align:center"><span style="color: #a">Bang Chan</span>, <span style="color: #b">Lee Know</span>, <span style="color: #c">Changbin</span>, <span style="color: #d">Hyunjin</span>, <span style="color: #e">HAN</span>, <span style="color: #f">Felix</span>, <span style="color: #g">Seungmin</span>, <span style="color: #h">I.N</span></p>
              <div class="wp-block-columns">
                <div class="wp-block-column">
                  <p><strong>Romanization</strong></p>
                  <p><span style="color: #a">Counting stars</span><br/><span style="color: #b">Everyday</span></p>
                </div>
                <div class="wp-block-column">
                  <p><strong>Hangul</strong></p>
                  <p><span style="color: #a">카운팅 스타</span><br/><span style="color: #b">에브리데이</span></p>
                </div>
              </div>
            </div>
        "##;
        let package = parse_colorcodedlyrics_html(html, None).unwrap();
        assert_eq!(package.members.len(), 2);
        assert!(package
            .members
            .iter()
            .all(|member| member.stage_name == "Bang Chan" || member.stage_name == "Lee Know"));
        assert!(!package
            .members
            .iter()
            .any(|member| member.stage_name.contains("방찬")));
    }

    #[test]
    fn strips_member_singer_all_tag_from_line_text() {
        let (cleaned, tag, with_all) =
            strip_member_singer_tag("[LK/All] bitkkal ppeonjjeok bitkkal ppeonjjeok");
        assert!(with_all);
        assert_eq!(tag.as_deref(), Some("LK"));
        assert_eq!(cleaned, "bitkkal ppeonjjeok bitkkal ppeonjjeok");
    }

    #[test]
    fn strips_duet_member_tag_from_line_text() {
        let (cleaned, tag) = strip_member_line_tag("[H/FL] yeogi modeun goseul balkhyeo");
        assert!(!tag.with_all);
        assert_eq!(tag.parts, vec!["H", "FL"]);
        assert_eq!(cleaned, "yeogi modeun goseul balkhyeo");
    }

    #[test]
    fn resolves_common_member_abbreviations() {
        let roster = vec![
            profile("Bang Chan", "#5067f3"),
            profile("Lee Know", "#3ece2b"),
            profile("HAN", "#e1fa44"),
            profile("Felix", "#fa231c"),
            profile("Hyunjin", "#bb71ff"),
        ];
        assert_eq!(resolve_member_name("LK", &roster).as_deref(), Some("Lee Know"));
        assert_eq!(resolve_member_name("H", &roster).as_deref(), Some("HAN"));
        assert_eq!(resolve_member_name("FL", &roster).as_deref(), Some("Felix"));
    }

    #[test]
    fn member_highlight_includes_both_duet_singers() {
        let roster = vec![
            profile("HAN", "#e1fa44"),
            profile("Felix", "#fa231c"),
            profile("Lee Know", "#3ece2b"),
        ];
        let mut line = LyricLine {
            id: None,
            song_id: None,
            index: 0,
            member: None,
            original: "여기 모든 곳을 밝혀".into(),
            romanization: Some("[H/FL] yeogi modeun goseul balkhyeo".into()),
            english: Some("Shine a light here everywhere".into()),
            with_all: false,
            segments: Vec::new(),
        };
        normalize_line_member_tags(&mut line);
        assert_eq!(line.member.as_deref(), Some("H, FL"));
        assert_eq!(
            line.romanization.as_deref(),
            Some("yeogi modeun goseul balkhyeo")
        );

        let highlight = member_highlight_for_line(&line, &roster);
        assert_eq!(highlight.primary, vec!["HAN", "Felix"]);
        assert!(highlight.backing.is_empty());
    }

    #[test]
    fn member_highlight_marks_roster_as_backing_for_all_lines() {
        let roster = vec![
            profile("Lee Know", "#3ece2b"),
            profile("Bang Chan", "#5067f3"),
            profile("Felix", "#f771e5"),
        ];
        let mut line = LyricLine {
            id: None,
            song_id: None,
            index: 0,
            member: Some("Lee Know".into()),
            original: "빛깔 뻔쩍".into(),
            romanization: Some("[LK/All] bitkkal ppeonjjeok".into()),
            english: Some("Flashy, flashy".into()),
            with_all: false,
            segments: Vec::new(),
        };
        normalize_line_member_tags(&mut line);
        assert!(line.with_all);
        assert_eq!(line.romanization.as_deref(), Some("bitkkal ppeonjjeok"));

        let highlight = member_highlight_for_line(&line, &roster);
        assert_eq!(highlight.primary, vec!["Lee Know"]);
        assert_eq!(highlight.backing, vec!["Bang Chan", "Felix"]);
    }

    #[test]
    fn members_for_lines_deduplicates_abbreviated_singer_tags() {
        let legend = vec![
            profile("Bang Chan", "#5067f3"),
            profile("Lee Know", "#3ece2b"),
            profile("Changbin", "#ff6a00"),
            profile("Hyunjin", "#bb71ff"),
            profile("HAN", "#e1fa44"),
            profile("Felix", "#fa231c"),
            profile("Seungmin", "#0099ff"),
            profile("I.N", "#ff69b4"),
        ];
        let mut lines = vec![
            LyricLine {
                id: None,
                song_id: None,
                index: 0,
                member: Some("Bang Chan".into()),
                original: "카운팅 스타".into(),
                romanization: Some("Counting stars".into()),
                english: None,
                with_all: false,
                segments: Vec::new(),
            },
            LyricLine {
                id: None,
                song_id: None,
                index: 1,
                member: Some("Lee Know".into()),
                original: "에브리데이".into(),
                romanization: Some("Everyday".into()),
                english: None,
                with_all: false,
                segments: Vec::new(),
            },
        ];
        let mut duet = LyricLine {
            id: None,
            song_id: None,
            index: 2,
            member: None,
            original: "여기 모든 곳을 밝혀".into(),
            romanization: Some("[H/FL] yeogi modeun goseul balkhyeo".into()),
            english: Some("Shine a light here everywhere".into()),
            with_all: false,
            segments: Vec::new(),
        };
        normalize_line_member_tags(&mut duet);
        lines.push(duet);

        canonicalize_line_member_names(&mut lines, &legend);
        let members = members_for_lines(&lines, &legend);
        let names: Vec<_> = members
            .iter()
            .map(|member| member.stage_name.as_str())
            .collect();

        assert_eq!(names, vec!["Bang Chan", "Lee Know", "HAN", "Felix"]);
        assert_eq!(lines[2].member.as_deref(), Some("HAN, Felix"));
        assert!(!names.contains(&"H"));
        assert!(!names.contains(&"FL"));
        assert!(!names.contains(&"한"));
        assert!(!names.contains(&"필릭스"));
    }

    fn profile(name: &str, color: &str) -> MemberProfile {
        MemberProfile {
            id: None,
            stage_name: name.to_string(),
            real_name: None,
            color: color.to_string(),
            image_url: None,
            local_image_path: None,
            provider: None,
        }
    }

    #[test]
    fn parses_english_only_colorcodedlyrics_section() {
        let html = r##"
            <h1 class="entry-title">TWICE - THIS IS FOR</h1>
            <div class="entry-content">
              <p style="text-align: center"><span style="color: #00ccff">Nayeon</span>, <span style="color: #ffb1b8">Momo</span></p>
              <p class="has-text-align-center"><strong><span class="has-inline-color has-very-light-gray-color">English</span></strong></p>
              <p><span style="color: #ff1744">This is for all my ladies</span><br/><span style="color: #00ccff">One time for all my ladies</span></p>
            </div>
        "##;
        let package = parse_colorcodedlyrics_html(html, None).unwrap();
        assert_eq!(package.lines.len(), 2);
        assert!(package.lines.iter().all(|line| line.original.trim().is_empty()));
        assert_eq!(
            package.lines[0].english.as_deref(),
            Some("This is for all my ladies")
        );
        assert_eq!(
            package.lines[1].english.as_deref(),
            Some("One time for all my ladies")
        );
        let (show_original, show_romanization, show_english) =
            lyric_language_toggles(&package.lines);
        assert!(!show_original);
        assert!(!show_romanization);
        assert!(show_english);
    }

    #[test]
    fn keeps_uncolored_adlibs_between_colored_english_lines() {
        let html = r##"
            <h1 class="entry-title">TWICE - THIS IS FOR</h1>
            <div class="entry-content">
              <p style="text-align: center"><span style="color: #996de7">Sana</span>, <span style="color: #ffb74d">Jihyo</span></p>
              <p class="has-text-align-center"><strong><span class="has-inline-color has-very-light-gray-color">English</span></strong></p>
              <p>Beep beep beep<br /><span style="color: #996de7">I'm outside your door so let's go don't let that</span><br />Beep beep beep<br /><span style="color: #996de7">Have you feeling low when you're grown you got the</span><br />Key key keys <span style="color: #ffb74d">(You got it)</span></p>
            </div>
        "##;
        let package = parse_colorcodedlyrics_html(html, None).unwrap();
        let english: Vec<_> = package
            .lines
            .iter()
            .filter_map(|line| line.english.as_deref())
            .collect();
        assert_eq!(english.len(), 5);
        assert_eq!(english[0], "Beep beep beep");
        assert_eq!(
            english[1],
            "I'm outside your door so let's go don't let that"
        );
        assert_eq!(english[2], "Beep beep beep");
        assert_eq!(
            english[3],
            "Have you feeling low when you're grown you got the"
        );
        assert_eq!(english[4], "Key key keys (You got it)");
        assert_eq!(package.lines[1].member.as_deref(), Some("Sana"));
        assert!(package.lines[0].with_all);
        assert!(!package.lines[1].with_all);
        let key_line = package.lines.iter().find(|line| {
            line.english
                .as_deref()
                .is_some_and(|text| text.starts_with("Key key keys"))
        });
        assert!(key_line.is_some());
        let key_line = key_line.unwrap();
        let english_segments: Vec<_> = key_line
            .segments
            .iter()
            .filter(|segment| segment.language == "english")
            .collect();
        assert_eq!(english_segments.len(), 2);
        assert_eq!(english_segments[0].member, None);
        assert_eq!(english_segments[1].member.as_deref(), Some("Jihyo"));
    }

    #[test]
    fn parses_multi_color_english_adlibs_on_one_line() {
        let html = r##"
            <h1 class="entry-title">TWICE - THIS IS FOR</h1>
            <div class="entry-content">
              <p style="text-align: center"><span style="color: #ff1744">Chaeyoung</span>, <span style="color: #1af0af">Mina</span>, <span style="color: #ffb1b8">Momo</span>, <span style="color: #396ad8">Tzuyu</span></p>
              <p class="has-text-align-center"><strong><span class="has-inline-color has-very-light-gray-color">English</span></strong></p>
              <p><span style="color: #ff1744"><span style="color: #1af0af">(Hahaha) </span>This is for all my ladies</span></p>
              <p><span style="color: #ff1744">Who don't get hyped enough <span style="color: #1af0af">(Hey ladies)</span></span></p>
              <p><span style="color: #ff1744">Then this your song so turn it up </span><span style="color: #1af0af">(Turn it up for me uh uh)</span></p>
              <p><span style="color: #396ad8">Something about that water tastes like fun <span style="color: #ffb1b8">(Yeah yeah)</span></span></p>
            </div>
        "##;
        let package = parse_colorcodedlyrics_html(html, None).unwrap();
        let hahaha = package
            .lines
            .iter()
            .find(|line| {
                line.english
                    .as_deref()
                    .is_some_and(|text| text.contains("Hahaha"))
            })
            .expect("hahaha line");
        let segments: Vec<_> = hahaha
            .segments
            .iter()
            .filter(|segment| segment.language == "english")
            .collect();
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].member.as_deref(), Some("Mina"));
        assert_eq!(segments[1].member.as_deref(), Some("Chaeyoung"));

        let turn_it_up = package
            .lines
            .iter()
            .find(|line| {
                line.english
                    .as_deref()
                    .is_some_and(|text| text.contains("Turn it up for me"))
            })
            .expect("turn it up line");
        let segments: Vec<_> = turn_it_up
            .segments
            .iter()
            .filter(|segment| segment.language == "english")
            .collect();
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].member.as_deref(), Some("Chaeyoung"));
        assert_eq!(segments[1].member.as_deref(), Some("Mina"));

        let yeah = package
            .lines
            .iter()
            .find(|line| {
                line.english
                    .as_deref()
                    .is_some_and(|text| text.contains("Yeah yeah"))
            })
            .expect("yeah yeah line");
        let segments: Vec<_> = yeah
            .segments
            .iter()
            .filter(|segment| segment.language == "english")
            .collect();
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].member.as_deref(), Some("Tzuyu"));
        assert_eq!(segments[1].member.as_deref(), Some("Momo"));
    }

    #[test]
    #[ignore = "uses live ColorCodedLyrics endpoints"]
    fn fetches_twice_this_is_for_from_slug_fallback() {
        let package = ColorCodedLyricsProvider::default()
            .fetch("TWICE THIS IS FOR")
            .unwrap();
        assert_eq!(package.song.artist, "TWICE");
        assert!(package
            .song
            .source_url
            .as_deref()
            .is_some_and(|url| url.contains("twice-this-is-for")));
        assert!(!package.lines.is_empty());
        assert!(package.lines.iter().all(|line| line.original.trim().is_empty()));
        assert!(package
            .lines
            .iter()
            .all(|line| line.english.as_ref().is_some_and(|text| !text.trim().is_empty())));
        assert!(package.lines.iter().any(|line| {
            line.english.as_deref().is_some_and(|text| {
                text.to_lowercase().contains("outside your door")
            })
        }));
        assert!(package
            .lines
            .iter()
            .filter(|line| line.english.as_ref().is_some_and(|text| text.contains("Beep beep beep")))
            .count()
            >= 2);
        assert_eq!(package.members.len(), 9);
        let (show_original, _, show_english) = lyric_language_toggles(&package.lines);
        assert!(!show_original);
        assert!(show_english);
    }

    #[test]
    fn parses_genius_fixture() {
        let html = r#"<h1>GROUP - Song Lyrics</h1><div data-lyrics-container="true">One: line<br/>Two: line</div>"#;
        let package = parse_genius_html(html, None).unwrap();
        assert_eq!(package.provider, "genius");
        assert_eq!(package.members.len(), 2);
    }
}
