use std::cell::Cell;

use gtk::glib;
use gtk::prelude::*;
use gtk::{Box as GtkBox, Label, Orientation};

use crate::models::{AlignmentLine, LyricLine, MemberProfile, SongPackage};

#[derive(Clone, Debug, Default)]
pub struct LyricRowContent {
    pub line_index: usize,
    pub original_markup: Option<String>,
    pub roman_markup: Option<String>,
    pub english_markup: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct LyricStageContent {
    pub rows: Vec<LyricRowContent>,
    pub empty_message: Option<String>,
}

pub fn lyric_content_key(
    song: Option<&SongPackage>,
    alignment: &[AlignmentLine],
    show_original: bool,
    show_romanization: bool,
    show_english: bool,
) -> String {
    let lines = song
        .map(|song| {
            format!(
                "{}::{}::{}::{}",
                song.song.title,
                song.song.artist,
                song.provider,
                song.lines
                    .iter()
                    .map(|line| {
                        let time = alignment
                            .iter()
                            .find(|item| item.lyric_index == line.index)
                            .map(|item| item.start_ms)
                            .unwrap_or(-1);
                        format!(
                            "{}:{}:{}:{}:{}:{}:{}:{}",
                            line.index,
                            time,
                            line.original,
                            line.romanization.as_deref().unwrap_or(""),
                            line.english.as_deref().unwrap_or(""),
                            line.member.as_deref().unwrap_or(""),
                            line.with_all,
                            line.segments
                                .iter()
                                .map(|segment| {
                                    format!(
                                        "{}:{}:{}",
                                        segment.language,
                                        segment.member.as_deref().unwrap_or(""),
                                        segment.text
                                    )
                                })
                                .collect::<Vec<_>>()
                                .join(";"),
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("|")
            )
        })
        .unwrap_or_default();
    format!("{lines}::{show_original}:{show_romanization}:{show_english}")
}

pub fn compute_lyric_stage_content(
    song: Option<SongPackage>,
    show_original: bool,
    show_romanization: bool,
    show_english: bool,
) -> LyricStageContent {
    let Some(song) = song else {
        return LyricStageContent {
            empty_message: Some(
                "Load or import lyrics, then align captions to start synced playback.".into(),
            ),
            ..Default::default()
        };
    };

    let members = &song.members;
    let rows = song
        .lines
        .iter()
        .map(|line| LyricRowContent {
            line_index: line.index,
            original_markup: show_original
                .then(|| {
                    (!line.original.trim().is_empty())
                        .then(|| colored_markup(line, "original", &line.original, members))
                })
                .flatten(),
            roman_markup: show_romanization
                .then(|| line.romanization.as_ref())
                .flatten()
                .map(|text| colored_markup(line, "romanization", text, members)),
            english_markup: show_english
                .then(|| line.english.as_ref())
                .flatten()
                .map(|text| colored_markup(line, "english", text, members)),
        })
        .collect();

    LyricStageContent {
        rows,
        empty_message: None,
    }
}

struct LyricStageWidgets {
    empty_label: Label,
    text_box: GtkBox,
    original_label: Label,
    roman_label: Label,
    english_label: Label,
}

pub struct LyricStage {
    content_key: String,
    rows: Vec<LyricRowContent>,
    empty_message: Option<String>,
    last_active: Cell<Option<usize>>,
    widgets: Option<LyricStageWidgets>,
}

impl LyricStage {
    pub fn new() -> Self {
        Self {
            content_key: String::new(),
            rows: Vec::new(),
            empty_message: None,
            last_active: Cell::new(None),
            widgets: None,
        }
    }

    pub fn content_key(&self) -> &str {
        &self.content_key
    }

    pub fn apply_content(
        &mut self,
        container: &GtkBox,
        content_key: String,
        content: LyricStageContent,
    ) {
        if self.content_key == content_key {
            return;
        }

        while let Some(child) = container.first_child() {
            container.remove(&child);
        }
        self.widgets = None;
        self.last_active.set(None);
        self.content_key = content_key;
        self.rows = content.rows;
        self.empty_message = content.empty_message;

        let panel = GtkBox::new(Orientation::Vertical, 6);
        panel.set_halign(gtk::Align::Fill);
        panel.set_valign(gtk::Align::Start);
        panel.add_css_class("lyric-current-panel");

        let empty_label = Label::new(None);
        empty_label.set_wrap(true);
        empty_label.set_xalign(0.5);
        empty_label.set_halign(gtk::Align::Center);
        empty_label.set_margin_top(24);
        empty_label.set_margin_bottom(24);
        empty_label.add_css_class("lyric-empty-message");

        let text_box = GtkBox::new(Orientation::Vertical, 8);
        text_box.set_halign(gtk::Align::Center);
        text_box.set_valign(gtk::Align::Center);
        text_box.add_css_class("lyric-current-line");

        let original_label = markup_label("");
        let roman_label = markup_label("");
        let english_label = markup_label("");

        text_box.append(&original_label);
        text_box.append(&roman_label);
        text_box.append(&english_label);

        panel.append(&empty_label);
        panel.append(&text_box);
        container.append(&panel);

        self.widgets = Some(LyricStageWidgets {
            empty_label,
            text_box,
            original_label,
            roman_label,
            english_label,
        });

        if self.empty_message.is_some() {
            self.show_empty_state();
        } else {
            self.show_line_display();
            self.render_active_line(None);
        }
    }

    pub fn set_active(&self, active_index: usize) {
        if self.empty_message.is_some() {
            return;
        }
        if self.last_active.get() == Some(active_index) {
            return;
        }
        self.last_active.set(Some(active_index));
        self.render_active_line(Some(active_index));
    }

    fn show_empty_state(&self) {
        let Some(widgets) = &self.widgets else {
            return;
        };
        if let Some(message) = &self.empty_message {
            widgets.empty_label.set_markup(&format!(
                "<span size='large' foreground='#666666'>{message}</span>"
            ));
            widgets.empty_label.set_visible(true);
        }
        widgets.text_box.set_visible(false);
    }

    fn show_line_display(&self) {
        let Some(widgets) = &self.widgets else {
            return;
        };
        widgets.empty_label.set_visible(false);
        widgets.text_box.set_visible(true);
    }

    fn render_active_line(&self, active_index: Option<usize>) {
        let Some(widgets) = &self.widgets else {
            return;
        };

        let row = active_index.and_then(|index| {
            if index == usize::MAX {
                None
            } else {
                self.rows.iter().find(|row| row.line_index == index)
            }
        });

        let Some(row) = row else {
            widgets.original_label.set_visible(false);
            widgets.roman_label.set_visible(false);
            widgets.english_label.set_visible(false);
            return;
        };

        set_markup_label(&widgets.original_label, row.original_markup.as_deref());
        set_markup_label(&widgets.roman_label, row.roman_markup.as_deref());
        set_markup_label(&widgets.english_label, row.english_markup.as_deref());
    }
}

const LYRIC_DISPLAY_PANGO_SIZE: &str = "32768"; // 32pt in Pango markup units (pt * 1024)

fn set_markup_label(label: &Label, markup: Option<&str>) {
    match markup.filter(|value| !value.is_empty()) {
        Some(markup) => {
            label.set_markup(&wrap_lyric_display_markup(markup));
            label.set_visible(true);
        }
        None => label.set_visible(false),
    }
}

fn wrap_lyric_display_markup(inner: &str) -> String {
    format!("<span size='{LYRIC_DISPLAY_PANGO_SIZE}' weight='heavy'>{inner}</span>")
}

fn configure_lyric_label(label: &Label) {
    label.add_css_class("lyric-current-text");
    label.set_xalign(0.5);
    label.set_justify(gtk::Justification::Center);
    label.set_wrap(true);
    label.set_max_width_chars(52);
}

fn markup_label(markup: &str) -> Label {
    let label = Label::new(None);
    configure_lyric_label(&label);
    if !markup.is_empty() {
        label.set_markup(&wrap_lyric_display_markup(markup));
    }
    label
}

fn colored_markup(
    line: &LyricLine,
    language: &str,
    fallback: &str,
    members: &[MemberProfile],
) -> String {
    let exact_segments: Vec<_> = line
        .segments
        .iter()
        .filter(|segment| segment.language == language)
        .collect();
    if exact_segments.is_empty() {
        return colorize_plain_text(fallback, member_color_for_line(line, members).as_deref());
    }
    let multi_segment = exact_segments.len() > 1;
    let line_color = member_color_for_line(line, members);
    exact_segments
        .iter()
        .map(|segment| {
            let color = segment
                .color
                .as_deref()
                .or_else(|| {
                    segment.member.as_ref().and_then(|name| {
                        members
                            .iter()
                            .find(|member| member.stage_name.eq_ignore_ascii_case(name))
                            .map(|member| member.color.as_str())
                    })
                })
                .or_else(|| {
                    if !multi_segment && !line.with_all {
                        line_color.as_deref()
                    } else {
                        None
                    }
                });
            segment_span(segment, color)
        })
        .collect::<Vec<_>>()
        .join("")
}

fn member_color_for_line(line: &LyricLine, members: &[MemberProfile]) -> Option<String> {
    for name in crate::lyrics::referenced_member_names(std::slice::from_ref(line)) {
        if let Some(member) = members
            .iter()
            .find(|member| member.stage_name.eq_ignore_ascii_case(&name))
        {
            return Some(member.color.clone());
        }
    }
    None
}

fn segment_span(segment: &crate::models::LyricSegment, member_color: Option<&str>) -> String {
    let color = segment
        .color
        .as_deref()
        .or(member_color)
        .map(format_markup_color);
    let text = glib::markup_escape_text(&segment.text);
    if let Some(color) = color {
        format!("<span foreground='{color}'>{text}</span>")
    } else {
        text.to_string()
    }
}

fn colorize_plain_text(text: &str, color: Option<&str>) -> String {
    let escaped = glib::markup_escape_text(text);
    if let Some(color) = color.map(format_markup_color) {
        format!("<span foreground='{color}'>{escaped}</span>")
    } else {
        format!("<span foreground='#1a1a1a'>{escaped}</span>")
    }
}

fn format_markup_color(color: &str) -> String {
    readable_on_light_background(color)
}

const MIN_CONTRAST_ON_LIGHT: f64 = 4.5;
const LIGHT_BACKGROUND: (f64, f64, f64) = (247.0 / 255.0, 247.0 / 255.0, 247.0 / 255.0);

fn readable_on_light_background(color: &str) -> String {
    let trimmed = color.trim().trim_start_matches('#');
    let Some(mut rgb) = parse_hex_rgb(trimmed) else {
        return if color.starts_with('#') {
            color.to_string()
        } else {
            format!("#{color}")
        };
    };

    let bg_lum = relative_luminance(LIGHT_BACKGROUND.0, LIGHT_BACKGROUND.1, LIGHT_BACKGROUND.2);
    for _ in 0..28 {
        let lum = relative_luminance(rgb.0, rgb.1, rgb.2);
        if contrast_ratio(lum, bg_lum) >= MIN_CONTRAST_ON_LIGHT {
            break;
        }
        rgb.0 = (rgb.0 * 0.86).max(0.0);
        rgb.1 = (rgb.1 * 0.86).max(0.0);
        rgb.2 = (rgb.2 * 0.86).max(0.0);
    }

    rgb_to_hex(rgb.0, rgb.1, rgb.2)
}

fn parse_hex_rgb(trimmed: &str) -> Option<(f64, f64, f64)> {
    let expanded = match trimmed.len() {
        3 => trimmed
            .chars()
            .map(|ch| format!("{ch}{ch}"))
            .collect::<String>(),
        6 => trimmed.to_string(),
        _ => return None,
    };
    let r = u8::from_str_radix(&expanded[0..2], 16).ok()? as f64 / 255.0;
    let g = u8::from_str_radix(&expanded[2..4], 16).ok()? as f64 / 255.0;
    let b = u8::from_str_radix(&expanded[4..6], 16).ok()? as f64 / 255.0;
    Some((r, g, b))
}

fn rgb_to_hex(r: f64, g: f64, b: f64) -> String {
    format!(
        "#{:02x}{:02x}{:02x}",
        (r.clamp(0.0, 1.0) * 255.0).round() as u8,
        (g.clamp(0.0, 1.0) * 255.0).round() as u8,
        (b.clamp(0.0, 1.0) * 255.0).round() as u8
    )
}

fn relative_luminance(r: f64, g: f64, b: f64) -> f64 {
    fn channel(value: f64) -> f64 {
        if value <= 0.03928 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    }
    0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b)
}

fn contrast_ratio(l1: f64, l2: f64) -> f64 {
    let (lighter, darker) = if l1 >= l2 { (l1, l2) } else { (l2, l1) };
    (lighter + 0.05) / (darker + 0.05)
}

#[cfg(test)]
mod tests {
    use super::{
        colored_markup, compute_lyric_stage_content, contrast_ratio, format_markup_color,
        readable_on_light_background, relative_luminance,
    };
    use crate::models::{LyricLine, LyricSegment, MemberProfile, Song, SongPackage};

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

    fn light_bg_luminance() -> f64 {
        relative_luminance(247.0 / 255.0, 247.0 / 255.0, 247.0 / 255.0)
    }

    fn meets_contrast(hex: &str) -> bool {
        let rgb = super::parse_hex_rgb(hex.trim_start_matches('#')).expect("hex");
        let lum = relative_luminance(rgb.0, rgb.1, rgb.2);
        contrast_ratio(lum, light_bg_luminance()) >= 4.5
    }

    #[test]
    fn light_member_colors_are_darkened_for_lyric_panel() {
        for light in ["#ffc0cb", "#90ee90", "#1af0af", "#ffff99", "#87cefa"] {
            let adjusted = readable_on_light_background(light);
            assert!(
                meets_contrast(&adjusted),
                "{light} -> {adjusted} should meet contrast"
            );
        }
    }

    #[test]
    fn strong_colors_stay_readable_without_over_darkening() {
        let adjusted = readable_on_light_background("#ff1744");
        assert!(adjusted.eq_ignore_ascii_case("#ff1744") || meets_contrast(&adjusted));
    }

    #[test]
    fn english_row_uses_translation_text_without_english_segments() {
        let song = SongPackage {
            song: Song {
                id: None,
                title: "S-Class".into(),
                artist: "Stray Kids".into(),
                group_name: Some("Stray Kids".into()),
                source_url: None,
            },
            members: vec![profile("Hyunjin", "#bb71ff")],
            lines: vec![LyricLine {
                id: None,
                song_id: None,
                index: 0,
                member: Some("Hyunjin".into()),
                original: "특별의 별의".into(),
                romanization: Some("teukbyeore byeore".into()),
                english: Some("The most special star".into()),
                with_all: false,
                layer: crate::models::LyricLayer::default(),
                segments: vec![
                    LyricSegment {
                        language: "original".into(),
                        text: "특별의 별의".into(),
                        member: Some("Hyunjin".into()),
                        color: Some("#bb71ff".into()),
                    },
                    LyricSegment {
                        language: "romanization".into(),
                        text: "teukbyeore byeore".into(),
                        member: Some("Hyunjin".into()),
                        color: Some("#bb71ff".into()),
                    },
                ],
            }],
            provider: "test".into(),
        };

        let content = compute_lyric_stage_content(Some(song), true, true, true);
        let row = &content.rows[0];
        let english = row.english_markup.as_deref().unwrap();
        assert!(english.contains("special star"));
        assert!(!english.contains("teukbyeore"));
    }

    #[test]
    fn referenced_members_include_comma_separated_line_tags() {
        let line = LyricLine {
            id: None,
            song_id: None,
            index: 0,
            member: Some("Hyunjin, Felix".into()),
            original: "line".into(),
            romanization: None,
            english: None,
            with_all: false,
            layer: crate::models::LyricLayer::default(),
            segments: Vec::new(),
        };
        let names = crate::lyrics::referenced_member_names(std::slice::from_ref(&line));
        assert_eq!(names, vec!["Hyunjin", "Felix"]);
    }

    #[test]
    fn unified_line_shows_korean_romanization_and_english_together() {
        let song = SongPackage {
            song: Song {
                id: None,
                title: "S-Class".into(),
                artist: "Stray Kids".into(),
                group_name: Some("Stray Kids".into()),
                source_url: None,
            },
            members: vec![profile("Hyunjin", "#bb71ff")],
            lines: vec![LyricLine {
                id: None,
                song_id: None,
                index: 16,
                member: Some("Hyunjin".into()),
                original: "특별의 별의 별의 별의 별의 별의 별의".into(),
                romanization: Some("teukbyeore byeore byeore byeore byeore byeore byeore".into()),
                english: Some("The most special star, star, star, star, star, star".into()),
                with_all: false,
                layer: crate::models::LyricLayer::default(),
                segments: vec![
                    LyricSegment {
                        language: "original".into(),
                        text: "특별의 별의 별의 별의 별의 별의 별의".into(),
                        member: Some("Hyunjin".into()),
                        color: Some("#bb71ff".into()),
                    },
                    LyricSegment {
                        language: "romanization".into(),
                        text: "teukbyeore byeore byeore byeore byeore byeore byeore".into(),
                        member: Some("Hyunjin".into()),
                        color: Some("#bb71ff".into()),
                    },
                    LyricSegment {
                        language: "english".into(),
                        text: "The most special star, star, star, star, star, star".into(),
                        member: Some("Hyunjin".into()),
                        color: Some("#bb71ff".into()),
                    },
                ],
            }],
            provider: "test".into(),
        };

        let content = compute_lyric_stage_content(Some(song.clone()), true, true, true);
        assert_eq!(content.rows.len(), 1);
        let row = &content.rows[0];
        assert_eq!(row.line_index, 16);
        assert!(row
            .original_markup
            .as_deref()
            .is_some_and(|markup| markup.contains("특별의")));
        assert!(row
            .roman_markup
            .as_deref()
            .is_some_and(|markup| markup.contains("teukbyeore")));
        assert!(row
            .english_markup
            .as_deref()
            .is_some_and(|markup| markup.contains("special star")));

        let before = super::lyric_content_key(Some(&song), &[], true, true, false);
        let after = super::lyric_content_key(Some(&song), &[], true, true, true);
        assert_ne!(before, after);
    }

    #[test]
    fn colors_multi_segment_english_line_without_bleeding_line_color() {
        let line = LyricLine {
            id: None,
            song_id: None,
            index: 0,
            member: Some("Chaeyoung, Mina".into()),
            original: String::new(),
            romanization: None,
            english: Some("Then this your song so turn it up (Turn it up for me uh uh)".into()),
            with_all: false,
            layer: crate::models::LyricLayer::default(),
            segments: vec![
                LyricSegment {
                    language: "english".into(),
                    text: "Then this your song so turn it up ".into(),
                    member: Some("Chaeyoung".into()),
                    color: Some("#ff1744".into()),
                },
                LyricSegment {
                    language: "english".into(),
                    text: "(Turn it up for me uh uh)".into(),
                    member: Some("Mina".into()),
                    color: Some("#1af0af".into()),
                },
            ],
        };
        let members = vec![profile("Chaeyoung", "ff1744"), profile("Mina", "1af0af")];
        let markup = colored_markup(&line, "english", line.english.as_deref().unwrap(), &members);
        assert!(markup.contains("foreground='#"));
        assert!(markup.matches("foreground=").count() >= 2);
        let chaeyoung_color = format_markup_color("#ff1744");
        let mina_color = format_markup_color("#1af0af");
        assert!(markup.contains(&format!("foreground='{chaeyoung_color}'")));
        assert!(markup.contains(&format!("foreground='{mina_color}'")));
    }
}
