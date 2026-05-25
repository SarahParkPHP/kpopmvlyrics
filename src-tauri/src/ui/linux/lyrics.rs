use std::cell::Cell;

use gtk::prelude::*;
use gtk::{Box as GtkBox, Label, Orientation, ScrolledWindow};

use crate::app::format_ms;
use crate::models::{AlignmentLine, LyricLine, SongPackage};

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
            song.lines
                .iter()
                .map(|line| {
                    let time = alignment
                        .iter()
                        .find(|item| item.lyric_index == line.index)
                        .map(|item| item.start_ms)
                        .unwrap_or(-1);
                    format!("{}:{}", line.index, time)
                })
                .collect::<Vec<_>>()
                .join("|")
        })
        .unwrap_or_default();
    format!(
        "{lines}::{}:{}:{}",
        show_original, show_romanization, show_english
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

    let rows = song
        .lines
        .iter()
        .map(|line| {
            let timing = alignment.iter().find(|item| item.lyric_index == line.index);
            LyricRowContent {
                line_index: line.index,
                member: line.member.clone().unwrap_or_else(|| "All".to_string()),
                original_markup: show_original
                    .then(|| colored_markup(line, "original", &line.original)),
                roman_markup: show_romanization
                    .then(|| line.romanization.as_ref())
                    .flatten()
                    .map(|text| colored_markup(line, "romanization", text)),
                english_markup: show_english
                    .then(|| line.english.as_ref())
                    .flatten()
                    .map(|text| colored_markup(line, "english", text)),
                time_text: timing
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
            let empty = Label::new(Some(&message));
            empty.set_line_wrap(true);
            empty.set_xalign(0.0);
            container.pack_start(&empty, false, false, 0);
            container.show_all();
            return;
        }

        for row_content in content.rows {
            let row = GtkBox::new(Orientation::Horizontal, 12);
            let member = Label::new(Some(&row_content.member));
            member.set_width_chars(10);
            member.set_xalign(0.0);

            let text_box = GtkBox::new(Orientation::Vertical, 2);
            if let Some(markup) = &row_content.original_markup {
                text_box.pack_start(&markup_label(markup), false, false, 0);
            }
            if let Some(markup) = &row_content.roman_markup {
                text_box.pack_start(&markup_label(markup), false, false, 0);
            }
            if let Some(markup) = &row_content.english_markup {
                text_box.pack_start(&markup_label(markup), false, false, 0);
            }

            let time = Label::new(Some(&row_content.time_text));
            row.pack_start(&member, false, false, 0);
            row.pack_start(&text_box, true, true, 0);
            row.pack_start(&time, false, false, 0);
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
            if widget.line_index == active_index {
                context.add_class(gtk::STYLE_CLASS_FRAME);
                active_row = Some(widget.row.clone());
            } else {
                context.remove_class(gtk::STYLE_CLASS_FRAME);
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
    label.set_xalign(0.0);
    label.set_line_wrap(true);
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
            let target = (row_y - (page - row_h) * 0.35).clamp(0.0, (upper - page).max(0.0));
            adj.set_value(target);
        }
    });
}

fn colored_markup(line: &LyricLine, language: &str, fallback: &str) -> String {
    let segments: Vec<_> = line
        .segments
        .iter()
        .filter(|segment| segment.language == language)
        .collect();
    if segments.is_empty() {
        return glib::markup_escape_text(fallback).to_string();
    }
    segments
        .iter()
        .map(|segment| {
            let text = glib::markup_escape_text(&segment.text);
            if let Some(color) = &segment.color {
                format!("<span foreground='{color}'>{text}</span>")
            } else {
                text.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}
