#![cfg(target_os = "linux")]

use std::cell::RefCell;
use std::rc::Rc;

use gtk::gio;
use gtk::prelude::*;
use gtk::{
    ApplicationWindow, Box as GtkBox, Button, Label, Orientation, Revealer, ScrolledWindow,
    TextView,
};

use crate::app::{shift_alignment, DEFAULT_MANUAL_CAPTIONS, DEFAULT_MANUAL_LYRICS};
use crate::log::{progress, verbose, PhaseGuard};
use crate::models::{AlignmentLine, LyricLayer, LyricLine, MemberProfile};

use super::timeline::Timeline;
use super::{spawn_work, BackgroundUpdate, UiModel, UiView, WorkerSnapshot};

pub struct EditorWidgets {
    pub revealer: Revealer,
    pub timeline: Rc<Timeline>,
    pub render_key: Rc<RefCell<String>>,
}

pub struct EditorBuild {
    pub revealer: Revealer,
    pub widgets: EditorWidgets,
    pub import_lyrics_button: Button,
    pub import_captions_button: Button,
    pub shift_back_button: Button,
    pub shift_forward_button: Button,
    pub save_button: Button,
    pub lyrics_view: TextView,
    pub captions_view: TextView,
}

pub fn build_editor_panel() -> EditorBuild {
    let panel = GtkBox::new(Orientation::Vertical, 8);

    let import_row = GtkBox::new(Orientation::Horizontal, 8);
    import_row.set_homogeneous(true);

    let lyrics_frame = GtkBox::new(Orientation::Vertical, 4);
    lyrics_frame.set_hexpand(true);
    lyrics_frame.append(&Label::new(Some("Manual lyrics")));
    let lyrics_scroll = ScrolledWindow::new();
    lyrics_scroll.set_min_content_height(100);
    lyrics_scroll.set_vexpand(true);
    let lyrics_view = TextView::new();
    lyrics_view.buffer().set_text(DEFAULT_MANUAL_LYRICS);
    lyrics_scroll.set_child(Some(&lyrics_view));
    lyrics_frame.append(&lyrics_scroll);

    let captions_frame = GtkBox::new(Orientation::Vertical, 4);
    captions_frame.set_hexpand(true);
    captions_frame.append(&Label::new(Some("Manual captions")));
    let captions_scroll = ScrolledWindow::new();
    captions_scroll.set_min_content_height(100);
    captions_scroll.set_vexpand(true);
    let captions_view = TextView::new();
    captions_view.buffer().set_text(DEFAULT_MANUAL_CAPTIONS);
    captions_scroll.set_child(Some(&captions_view));
    captions_frame.append(&captions_scroll);

    import_row.append(&lyrics_frame);
    import_row.append(&captions_frame);
    panel.append(&import_row);

    let import_actions = GtkBox::new(Orientation::Horizontal, 6);
    let import_lyrics_button = Button::with_label("Import Lyrics");
    let import_captions_button = Button::with_label("Import Captions");
    let shift_back_button = Button::with_label("-0.5s");
    let shift_forward_button = Button::with_label("+0.5s");
    let save_button = Button::with_label("Save Alignment");
    import_actions.append(&import_lyrics_button);
    import_actions.append(&import_captions_button);
    import_actions.append(&shift_back_button);
    import_actions.append(&shift_forward_button);
    import_actions.append(&save_button);
    panel.append(&import_actions);

    let timeline = Timeline::new();
    panel.append(&timeline.root);

    let revealer = Revealer::new();
    revealer.set_reveal_child(false);
    revealer.set_child(Some(&panel));

    EditorBuild {
        revealer: revealer.clone(),
        widgets: EditorWidgets {
            revealer,
            timeline,
            render_key: Rc::new(RefCell::new(String::new())),
        },
        import_lyrics_button,
        import_captions_button,
        shift_back_button,
        shift_forward_button,
        save_button,
        lyrics_view,
        captions_view,
    }
}

pub fn connect_editor_handlers(
    view: &Rc<UiView>,
    window: &ApplicationWindow,
    work_tx: std::sync::mpsc::Sender<BackgroundUpdate>,
    build: &EditorBuild,
    editor_button: &Button,
) {
    // Wire the timeline interactions (need the live view).
    build.widgets.timeline.connect(view);
    build.widgets.timeline.connect_seek(view);

    {
        let view = Rc::clone(view);
        let revealer = build.widgets.revealer.clone();
        editor_button.connect_clicked(move |_| {
            let open = !revealer.reveals_child();
            revealer.set_reveal_child(open);
            if open {
                view.render_editor_table();
            }
        });
    }

    {
        let view = Rc::clone(view);
        let lyrics_view = build.lyrics_view.clone();
        let work_tx = work_tx.clone();
        build.import_lyrics_button.connect_clicked(move |_| {
            let buffer = lyrics_view.buffer();
            let (start, end) = buffer.bounds();
            let text = buffer.text(&start, &end, true).to_string();
            let query = view.model.borrow().query.clone();
            let title = if query.is_empty() {
                "Imported Song".to_string()
            } else {
                query.clone()
            };
            let artist = query
                .split_whitespace()
                .next()
                .unwrap_or("Imported Group")
                .to_string();
            let view = Rc::clone(&view);
            spawn_work(work_tx.clone(), view, "Lyric import", move |snapshot| {
                let package = snapshot.ctx.import_lyrics(&text, &title, &artist)?;
                Ok(Box::new(move |model: &mut UiModel| {
                    model.song = Some(package);
                    model.editor_table_dirty = true;
                }) as Box<dyn FnOnce(&mut UiModel) + Send>)
            });
        });
    }

    {
        let view = Rc::clone(view);
        let captions_view = build.captions_view.clone();
        let work_tx = work_tx.clone();
        build.import_captions_button.connect_clicked(move |_| {
            let buffer = captions_view.buffer();
            let (start, end) = buffer.bounds();
            let text = buffer.text(&start, &end, true).to_string();
            let view = Rc::clone(&view);
            spawn_work(work_tx.clone(), view, "Caption import", move |snapshot| {
                let video_id = snapshot
                    .metadata
                    .as_ref()
                    .map(|meta| meta.video_id.clone())
                    .ok_or_else(|| "Resolve a YouTube URL first".to_string())?;
                let captions = snapshot.ctx.import_captions(&video_id, &text)?;
                Ok(Box::new(move |model: &mut UiModel| {
                    model.captions = captions;
                }) as Box<dyn FnOnce(&mut UiModel) + Send>)
            });
        });
    }

    {
        let view = Rc::clone(view);
        build.shift_back_button.connect_clicked(move |_| {
            view.refresh_mut(|model| {
                model.alignment = shift_alignment(&model.alignment, -500);
                model.editor_table_dirty = true;
            });
            view.render_editor_table();
        });
    }

    {
        let view = Rc::clone(view);
        build.shift_forward_button.connect_clicked(move |_| {
            view.refresh_mut(|model| {
                model.alignment = shift_alignment(&model.alignment, 500);
                model.editor_table_dirty = true;
            });
            view.render_editor_table();
        });
    }

    {
        let view = Rc::clone(view);
        let work_tx = work_tx.clone();
        build.save_button.connect_clicked(move |_| {
            let alignment = view.model.borrow().alignment.clone();
            let view = Rc::clone(&view);
            spawn_work(work_tx.clone(), view, "Save", move |snapshot| {
                let mut package = snapshot
                    .song
                    .clone()
                    .ok_or_else(|| "Load lyrics first".to_string())?;
                let video_id = snapshot
                    .metadata
                    .as_ref()
                    .map(|meta| meta.video_id.clone())
                    .ok_or_else(|| "Resolve a YouTube URL first".to_string())?;
                // Persist lyric edits (text, member, layer, created/deleted lines) first
                // so the song id exists, then persist the alignment timing.
                snapshot.ctx.save_lyric_lines(&mut package)?;
                let song_id = package
                    .song
                    .id
                    .ok_or_else(|| "Could not determine song id".to_string())?;
                snapshot
                    .ctx
                    .save_alignment_edits(song_id, &video_id, &alignment)?;
                Ok(Box::new(move |model: &mut UiModel| {
                    model.song = Some(package);
                }) as Box<dyn FnOnce(&mut UiModel) + Send>)
            });
        });
    }

    let _ = window;
}

impl UiView {
    /// Relayout the timeline editor when its contents changed. Named
    /// `render_editor_table` for historical call sites (the tick loop, editor open).
    pub fn render_editor_table(&self) {
        if !self.editor.revealer.reveals_child() {
            return;
        }
        let Some(view) = self.this.borrow().clone() else {
            return;
        };

        let should_render = {
            let Ok(model) = self.model.try_borrow() else {
                return;
            };
            model.editor_table_dirty
                || *self.editor.render_key.borrow() != editor_render_key(&model)
        };
        if !should_render {
            return;
        }

        {
            let Ok(model) = self.model.try_borrow() else {
                return;
            };
            *self.editor.render_key.borrow_mut() = editor_render_key(&model);
        }

        self.editor.timeline.relayout(&view);

        if let Ok(mut model) = self.model.try_borrow_mut() {
            model.editor_table_dirty = false;
        }
    }
}

/// Cheap fingerprint of everything the timeline layout depends on (excluding zoom,
/// which is applied directly): line set, member, layer, and each clip's timing.
fn editor_render_key(model: &UiModel) -> String {
    let lines = model
        .song
        .as_ref()
        .map(|song| {
            song.lines
                .iter()
                .map(|line| {
                    format!(
                        "{}:{}:{}",
                        line.index,
                        line.member.as_deref().unwrap_or(""),
                        line.layer.as_str()
                    )
                })
                .collect::<Vec<_>>()
                .join("|")
        })
        .unwrap_or_default();
    let alignment = model
        .alignment
        .iter()
        .map(|line| {
            format!(
                "{}:{}:{}:{}",
                line.lyric_index, line.start_ms, line.end_ms, line.needs_review
            )
        })
        .collect::<Vec<_>>()
        .join("|");
    format!("{lines}::{alignment}")
}

pub fn pick_member_image(
    window: &ApplicationWindow,
    view: Rc<UiView>,
    member: MemberProfile,
    group_name: String,
) {
    let dialog = gtk::FileDialog::builder()
        .title("Choose member image")
        .accept_label("_Open")
        .modal(true)
        .build();

    let filter = gtk::FileFilter::new();
    filter.set_name(Some("Images"));
    filter.add_mime_type("image/jpeg");
    filter.add_mime_type("image/png");
    filter.add_mime_type("image/gif");
    filter.add_mime_type("image/webp");
    let filters = gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&filter);
    dialog.set_filters(Some(&filters));
    dialog.set_default_filter(Some(&filter));

    dialog.open(
        Some(window),
        None::<&gio::Cancellable>,
        move |result| {
            if let Ok(file) = result {
                if let Some(path) = file.path() {
                    let mut updated = member.clone();
                    updated.local_image_path = Some(path.to_string_lossy().into_owned());
                    view.refresh_mut(|model| {
                        if let Err(err) = model.ctx.save_member_override(&group_name, &updated) {
                            model.error = Some(err);
                            return;
                        }
                        if let Some(song) = &mut model.song {
                            for item in &mut song.members {
                                if item.stage_name == updated.stage_name {
                                    *item = updated.clone();
                                }
                            }
                        }
                        model.editor_table_dirty = true;
                    });
                }
            }
        },
    );
}

pub fn resolve_video_chain(
    snapshot: WorkerSnapshot,
    report_progress: impl Fn(f64),
) -> Result<Box<dyn FnOnce(&mut UiModel) + Send>, String> {
    verbose(format!("open url={}", snapshot.url));
    report_progress(0.08);
    progress("open metadata", 0.08);
    let metadata = {
        let _phase = PhaseGuard::begin("resolve_video_metadata");
        snapshot.ctx.resolve_video_metadata(&snapshot.url)?
    };
    let query = crate::app::query_from_metadata(&metadata);
    verbose(format!("open video_id={} query={query}", metadata.video_id));
    report_progress(0.22);
    progress("open formats", 0.22);
    let formats = {
        let _phase = PhaseGuard::begin("list_video_formats");
        snapshot
            .ctx
            .list_video_formats(&snapshot.url)
            .unwrap_or_default()
    };
    verbose(format!("open formats count={}", formats.len()));

    let mut song = None;
    let mut captions = Vec::new();
    let mut alignment = Vec::new();
    let mut align_summary = None;

    if !query.is_empty() {
        report_progress(0.38);
        progress("open fetch_lyrics", 0.38);
        let package = {
            let _phase = PhaseGuard::begin("fetch_lyrics");
            snapshot.ctx.fetch_lyrics(&query)?
        };
        verbose(format!(
            "open lyrics lines={} song_id={:?}",
            package.lines.len(),
            package.song.id
        ));
        report_progress(0.58);
        progress("open lyrics done", 0.58);
        let video_id = metadata.video_id.clone();
        if let Some(song_id) = package.song.id {
            report_progress(0.68);
            progress("open align start", 0.68);
            let result = {
                let _phase = PhaseGuard::begin("align_lyrics_with_progress");
                snapshot.ctx.align_lyrics_with_progress(song_id, &video_id, |p| {
                    progress("open align", p);
                    report_progress(p);
                })?
            };
            verbose(format!(
                "open align done lines={} captions={} summary={}",
                result.alignment.len(),
                result.captions.len(),
                result.summary
            ));
            alignment = result.alignment;
            captions = result.captions;
            align_summary = Some(result.summary);
        } else {
            verbose("open skipped align: song has no id");
        }
        song = Some(package);
    } else {
        verbose("open skipped lyrics: empty query from metadata");
    }

    report_progress(0.86);
    progress("open resolve_stream", 0.86);
    let stream_spec = {
        let _phase = PhaseGuard::begin("resolve_stream");
        snapshot.ctx.resolve_stream(
            &snapshot.url,
            snapshot.selected_format.as_deref(),
        )?
    };
    verbose(format!("open stream resolved: {stream_spec:?}"));
    report_progress(0.94);
    progress("open complete", 0.94);

    Ok(Box::new(move |model: &mut UiModel| {
        model.metadata = Some(metadata);
        model.query = query;
        model.formats = formats;
        if let Some(package) = &song {
            let (show_original, show_romanization, show_english) =
                crate::lyrics::lyric_language_toggles(&package.lines);
            model.show_original = show_original;
            model.show_romanization = show_romanization;
            model.show_english = show_english;
        }
        model.song = song;
        model.captions = captions;
        model.alignment = alignment;
        model.player_loaded = false;
        model.current_ms = 0;
        model.active_index = 0;
        model.pending_stream = Some(stream_spec);
        model.editor_table_dirty = true;
        if let Some(summary) = align_summary {
            model.message = Some(summary);
        }
    }))
}

impl UiModel {
    pub fn set_line_member(&mut self, line_index: usize, member: Option<String>) {
        let Some(song) = &mut self.song else {
            return;
        };
        for line in &mut song.lines {
            if line.index == line_index {
                line.member = member.clone().filter(|name| !name.is_empty());
            }
        }
    }

    pub fn update_alignment(&mut self, lyric_index: usize, update: impl FnOnce(&mut AlignmentLine)) {
        if let Some(line) = self
            .alignment
            .iter_mut()
            .find(|line| line.lyric_index == lyric_index)
        {
            update(line);
            return;
        }
        let mut line = AlignmentLine {
            lyric_index,
            caption_index: None,
            start_ms: 0,
            end_ms: 1200,
            confidence: 0.0,
            needs_review: true,
        };
        update(&mut line);
        self.alignment.push(line);
    }

    pub fn set_line_layer(&mut self, line_index: usize, layer: LyricLayer) {
        if let Some(song) = &mut self.song {
            if let Some(line) = song.lines.iter_mut().find(|line| line.index == line_index) {
                line.layer = layer;
            }
        }
    }

    pub fn set_line_text(
        &mut self,
        line_index: usize,
        original: String,
        romanization: Option<String>,
        english: Option<String>,
    ) {
        if let Some(song) = &mut self.song {
            if let Some(line) = song.lines.iter_mut().find(|line| line.index == line_index) {
                line.original = original;
                line.romanization = romanization.filter(|text| !text.is_empty());
                line.english = english.filter(|text| !text.is_empty());
            }
        }
    }

    /// Create a new lyric line on `layer` spanning `[start_ms, start_ms + DEFAULT_CLIP_MS]`.
    /// Returns the new line's stable `index` (max existing + 1), or `None` if no song loaded.
    pub fn add_lyric_line(&mut self, layer: LyricLayer, start_ms: i64) -> Option<usize> {
        let song = self.song.as_mut()?;
        let new_index = song
            .lines
            .iter()
            .map(|line| line.index)
            .max()
            .map(|max| max + 1)
            .unwrap_or(0);
        let start_ms = start_ms.max(0);
        song.lines.push(LyricLine {
            id: None,
            song_id: song.song.id,
            index: new_index,
            member: None,
            original: String::new(),
            romanization: None,
            english: None,
            with_all: false,
            layer,
            segments: Vec::new(),
        });
        self.alignment.push(AlignmentLine {
            lyric_index: new_index,
            caption_index: None,
            start_ms,
            end_ms: start_ms + DEFAULT_CLIP_MS,
            confidence: 1.0,
            needs_review: true,
        });
        Some(new_index)
    }

    pub fn delete_lyric_line(&mut self, line_index: usize) {
        if let Some(song) = &mut self.song {
            song.lines.retain(|line| line.index != line_index);
        }
        self.alignment.retain(|line| line.lyric_index != line_index);
    }
}

/// Default length of a freshly created clip.
pub const DEFAULT_CLIP_MS: i64 = 1_500;
