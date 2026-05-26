use std::cell::Cell;

use gtk::prelude::*;
use gtk::{Box as GtkBox, Label, Orientation, ScrolledWindow};

use crate::align::has_playback_timing;
use crate::app::format_ms;
use crate::models::{AlignmentLine, LyricLine, MemberProfile, SongPackage};

#[derive(Clone, Debug, Default)]
pub struct LyricRowContent {
    pub line_index: usize,
    pub member: String,
    pub original_markup: Option<String>,
    pub roman_markup: Option<String>,
    pub english_markup: Option<String>,
    pub time_text: String,
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
    format!(
        "{lines}::{show_original}:{show_romanization}:{show_english}"
    )
}

pub fn compute_lyric_stage_content(
    song: Option<SongPackage>,
    alignment: &[AlignmentLine],
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
        .map(|line| {
            let timing = alignment.iter().find(|item| item.lyric_index == line.index);
            LyricRowContent {
                line_index: line.index,
                member: line.member.clone().unwrap_or_else(|| "All".to_string()),
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
                time_text: timing
                    .filter(|item| has_playback_timing(item))
                    .map(|item| format_ms(item.start_ms))
                    .unwrap_or_else(|| "Unaligned".to_string()),
            }
        })
        .collect();

    LyricStageContent {
        rows,
        empty_message: None,
    }
}

pub struct LyricStage {
    content_key: String,
    rows: Vec<LyricRowWidget>,
    last_active: Cell<Option<usize>>,
}

struct LyricRowWidget {
    line_index: usize,
    row: GtkBox,
}

impl LyricStage {
    pub fn new() -> Self {
        Self {
            content_key: String::new(),
            rows: Vec::new(),
            last_active: Cell::new(None),
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

        for child in container.children() {
            container.remove(&child);
        }
        self.rows.clear();
        self.last_active.set(None);
        self.content_key = content_key;

        if let Some(message) = content.empty_message {
            let empty = Label::new(None);
            empty.set_markup(&format!(
                "<span size='large' foreground='#666666'>{message}</span>"
            ));
            empty.set_line_wrap(true);
            empty.set_xalign(0.5);
            empty.set_halign(gtk::Align::Center);
            empty.set_margin_top(48);
            empty.set_margin_bottom(48);
            container.pack_start(&empty, true, true, 0);
            container.show_all();
            return;
        }

        for row_content in content.rows {
            let row = GtkBox::new(Orientation::Vertical, 4);
            row.set_halign(gtk::Align::Fill);
            row.style_context().add_class("lyric-line");

            let text_box = GtkBox::new(Orientation::Vertical, 6);
            text_box.set_halign(gtk::Align::Center);

            if let Some(markup) = &row_content.original_markup {
                text_box.pack_start(&markup_label(markup), false, false, 0);
            }
            if let Some(markup) = &row_content.roman_markup {
                text_box.pack_start(&markup_label(markup), false, false, 0);
            }
            if let Some(markup) = &row_content.english_markup {
                text_box.pack_start(&markup_label(markup), false, false, 0);
            }

            row.pack_start(&text_box, false, false, 0);
            container.pack_start(&row, false, false, 0);
            self.rows.push(LyricRowWidget {
                line_index: row_content.line_index,
                row,
            });
        }
        container.show_all();
    }

    pub fn set_active(&self, active_index: usize, scroll: &ScrolledWindow) {
        if self.last_active.get() == Some(active_index) {
            return;
        }
        self.last_active.set(Some(active_index));

        let mut active_row: Option<GtkBox> = None;
        for widget in &self.rows {
            let context = widget.row.style_context();
            let is_active = active_index != usize::MAX && widget.line_index == active_index;
            if is_active {
                context.add_class("lyric-line-active");
                widget.row.set_opacity(1.0);
                active_row = Some(widget.row.clone());
            } else {
                context.remove_class("lyric-line-active");
                widget.row.set_opacity(0.34);
            }
        }

        if let Some(row) = active_row {
            scroll_to_row(scroll, &row);
        }
    }
}

fn markup_label(markup: &str) -> Label {
    let label = Label::new(None);
    label.set_markup(markup);
    label.set_xalign(0.5);
    label.set_justify(gtk::Justification::Center);
    label.set_line_wrap(true);
    label.set_max_width_chars(48);
    label
}

fn scroll_to_row(scroll: &ScrolledWindow, row: &GtkBox) {
    let adj = scroll.vadjustment();
    gtk::glib::idle_add_local_once({
        let scroll = scroll.clone();
        let row = row.clone();
        move || {
            let allocation = row.allocation();
            let row_y = allocation.y() as f64;
            let row_h = allocation.height() as f64;
            let page = scroll.allocation().height().max(1) as f64;
            let upper = adj.upper();
            let target = (row_y - (page - row_h) * 0.42).clamp(0.0, (upper - page).max(0.0));
            adj.set_value(target);
        }
    });
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
        escaped.to_string()
    }
}

fn format_markup_color(color: &str) -> String {
    let trimmed = color.trim();
    if trimmed.starts_with('#') {
        trimmed.to_string()
    } else {
        format!("#{trimmed}")
    }
}

#[cfg(test)]
mod tests {
    use super::compute_lyric_stage_content;
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

        let content = compute_lyric_stage_content(Some(song), &[], true, true, true);
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
                romanization: Some(
                    "teukbyeore byeore byeore byeore byeore byeore byeore".into(),
                ),
                english: Some("The most special star, star, star, star, star, star".into()),
                with_all: false,
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

        let content = compute_lyric_stage_content(Some(song.clone()), &[], true, true, true);
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
        use super::colored_markup;

        let line = LyricLine {
            id: None,
            song_id: None,
            index: 0,
            member: Some("Chaeyoung, Mina".into()),
            original: String::new(),
            romanization: None,
            english: Some("Then this your song so turn it up (Turn it up for me uh uh)".into()),
            with_all: false,
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
        let members = vec![
            profile("Chaeyoung", "ff1744"),
            profile("Mina", "1af0af"),
        ];
        let markup = colored_markup(&line, "english", line.english.as_deref().unwrap(), &members);
        assert!(markup.contains("foreground='#ff1744'"));
        assert!(markup.contains("foreground='#1af0af'"));
        assert!(markup.matches("foreground=").count() >= 2);
    }
}
