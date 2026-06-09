use anyhow::Result;
use reqwest::blocking::Client;
use scraper::{Html, Selector};

use crate::log::verbose;
use crate::models::MemberProfile;
use crate::process_util::http_client;

pub trait MemberProfileProvider {
    fn search(&self, group_name: &str) -> Result<Vec<MemberProfile>>;
}

pub struct KpoppingProvider {
    client: Client,
}

impl Default for KpoppingProvider {
    fn default() -> Self {
        Self {
            client: http_client("kpopmvlyrics/0.1"),
        }
    }
}

impl MemberProfileProvider for KpoppingProvider {
    fn search(&self, group_name: &str) -> Result<Vec<MemberProfile>> {
        let group_slug = kpopping_group_slug(group_name);
        let profile_url = format!("https://kpopping.com/profiles/group/{group_slug}");
        verbose(format!(
            "kpopping fetch group={group_name:?} slug={group_slug} url={profile_url}"
        ));
        let response = self.client.get(&profile_url).send()?;
        let status = response.status();
        let html = response.text()?;
        let mut profiles = extract_kpopping_members(&html);
        verbose(format!(
            "kpopping status={status} html_bytes={} json_members={}",
            html.len(),
            profiles.len()
        ));
        if profiles.is_empty() {
            profiles = extract_profiles(&html, "kpopping", "https://kpopping.com");
            verbose(format!("kpopping html-fallback members={}", profiles.len()));
        }
        let with_images = profiles
            .iter()
            .filter(|profile| profile.image_url.is_some())
            .count();
        verbose(format!(
            "kpopping returning members={} with_images={with_images}",
            profiles.len()
        ));
        Ok(profiles)
    }
}

pub struct KpopFandomProvider {
    client: Client,
}

impl Default for KpopFandomProvider {
    fn default() -> Self {
        Self {
            client: http_client("kpopmvlyrics/0.1"),
        }
    }
}

impl MemberProfileProvider for KpopFandomProvider {
    fn search(&self, group_name: &str) -> Result<Vec<MemberProfile>> {
        let url = format!(
            "https://kpop.fandom.com/wiki/Special:Search?query={}",
            encode(group_name)
        );
        let html = self.client.get(url).send()?.text()?;
        Ok(extract_profiles(
            &html,
            "kpop-fandom",
            "https://kpop.fandom.com",
        ))
    }
}

fn extract_profiles(html: &str, provider: &str, base_url: &str) -> Vec<MemberProfile> {
    let document = Html::parse_document(html);
    let selector = Selector::parse("a, img").unwrap();
    let mut profiles = Vec::new();
    let palette = [
        "#e84855", "#2f80ed", "#27ae60", "#f2994a", "#9b51e0", "#00a6a6",
    ];
    for node in document.select(&selector).take(80) {
        let name = node
            .value()
            .attr("title")
            .or_else(|| node.value().attr("alt"))
            .map(str::trim)
            .filter(|name| name.len() > 1 && name.len() < 40);
        let Some(name) = name else { continue };
        if profiles
            .iter()
            .any(|profile: &MemberProfile| profile.stage_name.eq_ignore_ascii_case(name))
        {
            continue;
        }
        profiles.push(MemberProfile {
            id: None,
            stage_name: name.to_string(),
            real_name: None,
            color: palette[profiles.len() % palette.len()].to_string(),
            image_url: node
                .value()
                .attr("src")
                .or_else(|| node.value().attr("data-src"))
                .and_then(|src| normalize_image_url(src, base_url)),
            local_image_path: None,
            provider: Some(provider.to_string()),
        });
        if profiles.len() >= 12 {
            break;
        }
    }
    profiles
}

fn encode(query: &str) -> String {
    url::form_urlencoded::byte_serialize(query.as_bytes()).collect()
}

fn kpopping_group_slug(group_name: &str) -> String {
    // kpopping's group paths are romanized, hyphenated, and ASCII-only. Lyric
    // titles routinely carry the Korean name (e.g. "MEOVV (미야오)") or
    // punctuation (e.g. "(G)I-DLE", "fromis_9"), so keep only ASCII
    // alphanumerics per word and drop any word that empties out. This replaces
    // an LE SSERAFIM-specific Korean strip that left every other group's
    // "(한글)" suffix in the slug, yielding a 404 and no member photos.
    group_name
        .split_whitespace()
        .filter_map(|word| {
            let ascii: String = word
                .chars()
                .filter(|ch| ch.is_ascii_alphanumeric())
                .collect();
            (!ascii.is_empty()).then_some(ascii)
        })
        .collect::<Vec<_>>()
        .join("-")
        .to_uppercase()
}

fn extract_kpopping_members(html: &str) -> Vec<MemberProfile> {
    // kpopping embeds an idol roster as JSON, sometimes escaped inside a JS
    // string (\"name\":...) and sometimes raw ("name":...). Rather than depend
    // on a fixed key order (the old regex required id→name→position→image and
    // silently produced zero members the moment kpopping reordered keys), scan
    // each flat object and pull name/image/position independently, tolerating
    // both quote styles and escaped slashes in URLs.
    let object_re = regex::Regex::new(r#"\{[^{}]*\}"#).expect("valid regex");
    let name_re =
        regex::Regex::new(r#"\\?"name\\?"\s*:\s*\\?"([^"\\]+)\\?""#).expect("valid regex");
    let image_re = regex::Regex::new(r#"\\?"image\\?"\s*:\s*\\?"(https?:(?:[^"\\]+|\\/)+)\\?""#)
        .expect("valid regex");
    let position_re =
        regex::Regex::new(r#"\\?"position\\?"\s*:\s*\\?"([^"\\]*)\\?""#).expect("valid regex");
    let palette = [
        "#e84855", "#2f80ed", "#27ae60", "#f2994a", "#9b51e0", "#00a6a6",
    ];
    let mut profiles = Vec::new();
    for object in object_re.find_iter(html) {
        let chunk = object.as_str();
        let Some(name) = name_re
            .captures(chunk)
            .and_then(|caps| caps.get(1))
            .map(|value| value.as_str())
        else {
            continue;
        };
        let Some(image) = image_re
            .captures(chunk)
            .and_then(|caps| caps.get(1))
            .map(|value| value.as_str())
        else {
            continue;
        };
        let stage_name = short_stage_name(name);
        if stage_name.is_empty()
            || profiles.iter().any(|profile: &MemberProfile| {
                profile.stage_name.eq_ignore_ascii_case(&stage_name)
            })
        {
            continue;
        }
        let real_name = position_re
            .captures(chunk)
            .and_then(|caps| caps.get(1))
            .map(|value| value.as_str())
            .filter(|value| !value.is_empty())
            .map(|value| value.to_string());
        profiles.push(MemberProfile {
            id: None,
            stage_name,
            real_name,
            color: palette[profiles.len() % palette.len()].to_string(),
            image_url: Some(image.replace("\\/", "/")),
            local_image_path: None,
            provider: Some("kpopping".to_string()),
        });
    }
    profiles
}

fn short_stage_name(name: &str) -> String {
    name.replace("Kim ", "")
        .replace("Huh ", "")
        .replace("Hong ", "")
        .replace("Miyawaki ", "")
        .replace("Nakamura ", "")
}

fn normalize_image_url(src: &str, base_url: &str) -> Option<String> {
    let src = src.trim();
    if src.is_empty() || src.starts_with("data:") {
        return None;
    }
    if src.starts_with("//") {
        return Some(format!("https:{src}"));
    }
    if src.starts_with("http://") || src.starts_with("https://") {
        return Some(src.to_string());
    }
    if src.starts_with('/') {
        return Some(format!("{base_url}{src}"));
    }
    Some(format!("{base_url}/{src}"))
}

#[cfg(test)]
mod tests {
    use super::{extract_kpopping_members, kpopping_group_slug};

    #[test]
    fn group_slug_strips_korean_and_punctuation() {
        // colorcodedheaven titles arrive as "NAME (한글)"; the Korean parenthetical
        // must not leak into the slug or kpopping 404s and no photos are fetched.
        assert_eq!(kpopping_group_slug("MEOVV (미야오)"), "MEOVV");
        assert_eq!(kpopping_group_slug("LE SSERAFIM (르세라핌)"), "LE-SSERAFIM");
        assert_eq!(kpopping_group_slug("(G)I-DLE"), "GIDLE");
        assert_eq!(kpopping_group_slug("fromis_9"), "FROMIS9");
        assert_eq!(kpopping_group_slug("NMIXX"), "NMIXX");
    }

    #[test]
    fn parses_kpopping_member_json() {
        let html = r#"{\"members\":[{\"id\":\"1\",\"name\":\"Kim Chaewon\",\"koreanName\":\"김채원\",\"position\":\"Leader\",\"birthday\":\"2000-08-01\",\"nationality\":\"\",\"image\":\"https://cdn.example/idols/Kim-Chaewon/profile.jpg?v=1\",\"slug\":\"Kim-Chaewon\"}]}"#;
        let members = extract_kpopping_members(html);
        assert_eq!(members[0].stage_name, "Chaewon");
        assert_eq!(
            members[0].image_url.as_deref(),
            Some("https://cdn.example/idols/Kim-Chaewon/profile.jpg?v=1")
        );
    }

    #[test]
    fn parses_kpopping_member_json_with_reordered_keys() {
        // image before name, and no position key — the old order-locked regex
        // produced zero members for layouts like this.
        let html = r#"[{\"image\":\"https://cdn.example/a/profile.jpg\",\"slug\":\"a\",\"name\":\"Yunjin\"}]"#;
        let members = extract_kpopping_members(html);
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].stage_name, "Yunjin");
        assert_eq!(
            members[0].image_url.as_deref(),
            Some("https://cdn.example/a/profile.jpg")
        );
    }

    #[test]
    fn parses_kpopping_member_raw_json_with_escaped_slashes() {
        // Unescaped (raw) JSON, with escaped forward slashes in the URL.
        let html = r#"{"members":[{"name":"Sakura","position":"","image":"https:\/\/cdn.example\/idols\/Sakura.jpg"}]}"#;
        let members = extract_kpopping_members(html);
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].stage_name, "Sakura");
        assert_eq!(
            members[0].image_url.as_deref(),
            Some("https://cdn.example/idols/Sakura.jpg")
        );
    }
}
