#![cfg(desktop_unix)]

use std::cell::RefCell;
use std::fs;
use std::rc::Rc;

use gtk::gio;
use gtk::prelude::*;
use gtk::{
    ApplicationWindow, Box as GtkBox, Button, Orientation, ScrolledWindow, Stack, TextView, Window,
};
use serde::Deserialize;

use crate::app::{shift_alignment, DEFAULT_MANUAL_CAPTIONS, DEFAULT_MANUAL_LYRICS};
use crate::log::{progress, verbose, PhaseGuard};
use crate::models::{
    AlignmentLine, LyricLayer, LyricLine, MemberProfile, SongPackage, VideoMetadata,
};

use super::timeline::Timeline;
use super::{
    spawn_timeline_demucs_spectrogram, spawn_timeline_spectrogram, spawn_work, BackgroundUpdate,
    UiModel, UiView, WorkerSnapshot,
};

pub struct EditorWidgets {
    pub timeline: Rc<Timeline>,
    pub render_key: Rc<RefCell<String>>,
}

pub struct EditorBuild {
    /// The editor page content, added to the main stack as the "Editor" tab.
    pub panel: GtkBox,
    pub widgets: EditorWidgets,
    pub import_lyrics_button: Button,
    pub import_captions_button: Button,
    pub shift_back_button: Button,
    pub shift_forward_button: Button,
    pub save_button: Button,
    pub import_json_button: Button,
    pub export_json_button: Button,
}

pub fn build_editor_panel() -> EditorBuild {
    let panel = GtkBox::new(Orientation::Vertical, 8);

    // Manual lyrics/captions text now live in modal dialogs (opened by the
    // Import buttons) instead of permanently occupying the editor page.
    let import_actions = GtkBox::new(Orientation::Horizontal, 6);
    let import_lyrics_button = Button::with_label("Import Lyrics");
    let import_captions_button = Button::with_label("Import Captions");
    let shift_back_button = Button::with_label("-0.5s");
    let shift_forward_button = Button::with_label("+0.5s");
    let save_button = Button::with_label("Save Alignment");
    let import_json_button = Button::with_label("Import JSON");
    let export_json_button = Button::with_label("Export JSON");
    import_actions.append(&import_lyrics_button);
    import_actions.append(&import_captions_button);
    import_actions.append(&shift_back_button);
    import_actions.append(&shift_forward_button);
    import_actions.append(&save_button);
    import_actions.append(&import_json_button);
    import_actions.append(&export_json_button);
    panel.append(&import_actions);

    let timeline = Timeline::new();
    panel.append(&timeline.root);

    EditorBuild {
        panel,
        widgets: EditorWidgets {
            timeline,
            render_key: Rc::new(RefCell::new(String::new())),
        },
        import_lyrics_button,
        import_captions_button,
        shift_back_button,
        shift_forward_button,
        save_button,
        import_json_button,
        export_json_button,
    }
}

/// Modal text-entry dialog used to paste manual lyrics or captions. Calls
/// `on_import` with the entered text when the user confirms.
fn open_text_import_dialog(
    parent: &ApplicationWindow,
    title: &str,
    initial_text: &str,
    on_import: impl Fn(String) + 'static,
) {
    let dialog = Window::builder()
        .title(title)
        .transient_for(parent)
        .modal(true)
        .default_width(560)
        .default_height(440)
        .build();

    let layout = GtkBox::new(Orientation::Vertical, 8);
    layout.set_margin_top(12);
    layout.set_margin_bottom(12);
    layout.set_margin_start(12);
    layout.set_margin_end(12);

    let scroll = ScrolledWindow::new();
    scroll.set_vexpand(true);
    let text_view = TextView::new();
    text_view.set_wrap_mode(gtk::WrapMode::WordChar);
    text_view.set_monospace(true);
    text_view.buffer().set_text(initial_text);
    scroll.set_child(Some(&text_view));
    layout.append(&scroll);

    let actions = GtkBox::new(Orientation::Horizontal, 6);
    actions.set_halign(gtk::Align::End);
    let cancel_button = Button::with_label("Cancel");
    let import_button = Button::with_label("Import");
    import_button.add_css_class("suggested-action");
    actions.append(&cancel_button);
    actions.append(&import_button);
    layout.append(&actions);

    dialog.set_child(Some(&layout));

    {
        let dialog = dialog.clone();
        cancel_button.connect_clicked(move |_| dialog.close());
    }
    {
        let dialog = dialog.clone();
        import_button.connect_clicked(move |_| {
            let buffer = text_view.buffer();
            let (start, end) = buffer.bounds();
            let text = buffer.text(&start, &end, true).to_string();
            on_import(text);
            dialog.close();
        });
    }

    dialog.present();
}

pub fn connect_editor_handlers(
    view: &Rc<UiView>,
    window: &ApplicationWindow,
    work_tx: std::sync::mpsc::Sender<BackgroundUpdate>,
    build: &EditorBuild,
    main_stack: &Stack,
) {
    // Wire the timeline interactions (need the live view).
    build.widgets.timeline.connect(view);
    build.widgets.timeline.connect_seek(view);

    // Lay out the timeline whenever the Editor tab becomes visible.
    {
        let view = Rc::clone(view);
        main_stack.connect_visible_child_notify(move |stack| {
            if stack.visible_child_name().as_deref() == Some(EDITOR_PAGE) {
                view.render_editor_table();
            } else {
                *view.editor.render_key.borrow_mut() = String::new();
            }
        });
    }

    {
        let view = Rc::clone(view);
        let window = window.clone();
        let work_tx = work_tx.clone();
        build.import_lyrics_button.connect_clicked(move |_| {
            let view = Rc::clone(&view);
            let work_tx = work_tx.clone();
            open_text_import_dialog(&window, "Import lyrics", DEFAULT_MANUAL_LYRICS, move |text| {
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
        });
    }

    {
        let view = Rc::clone(view);
        let window = window.clone();
        let work_tx = work_tx.clone();
        build.import_captions_button.connect_clicked(move |_| {
            let view = Rc::clone(&view);
            let work_tx = work_tx.clone();
            open_text_import_dialog(
                &window,
                "Import captions",
                DEFAULT_MANUAL_CAPTIONS,
                move |text| {
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
                        })
                            as Box<dyn FnOnce(&mut UiModel) + Send>)
                    });
                },
            );
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

    {
        let view = Rc::clone(view);
        let window = window.clone();
        let work_tx = work_tx.clone();
        build.import_json_button.connect_clicked(move |_| {
            import_json_with_dialog(&window, Rc::clone(&view), work_tx.clone());
        });
    }

    {
        let view = Rc::clone(view);
        let window = window.clone();
        build.export_json_button.connect_clicked(move |_| {
            let snapshot = {
                let model = view.model.borrow();
                model
                    .metadata
                    .clone()
                    .zip(model.song.clone())
                    .map(|(metadata, song)| (metadata, song, model.alignment.clone()))
            };
            let Some((metadata, song, alignment)) = snapshot else {
                view.refresh_mut(|model| {
                    model.error = Some("Load lyrics and resolve a video first".to_string());
                });
                return;
            };
            export_json_with_dialog(&window, Rc::clone(&view), metadata, song, alignment);
        });
    }
}

/// Stack page name for the editor tab (shared by the switcher and visibility checks).
pub const EDITOR_PAGE: &str = "editor";
/// Stack page name for the playback tab (member photos + lyrics).
pub const PLAYBACK_PAGE: &str = "playback";

impl UiView {
    /// Whether the Editor tab is currently the visible stack page.
    pub fn editor_visible(&self) -> bool {
        self.main_stack.visible_child_name().as_deref() == Some(EDITOR_PAGE)
    }

    /// Relayout the timeline editor when its contents changed. Named
    /// `render_editor_table` for historical call sites (the tick loop, editor open).
    pub fn render_editor_table(&self) {
        if !self.editor_visible() {
            return;
        }
        let Some(view) = self.this.borrow().clone() else {
            return;
        };

        let should_render = {
            let Ok(model) = self.model.try_borrow() else {
                return;
            };
            self.editor
                .timeline
                .set_spectrogram(model.timeline_spectrogram.clone());
            self.editor
                .timeline
                .set_demucs_spectrogram(model.timeline_demucs_spectrogram.clone());
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

    dialog.open(Some(window), None::<&gio::Cancellable>, move |result| {
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
    });
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
                snapshot
                    .ctx
                    .align_lyrics_with_progress(song_id, &video_id, |p| {
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
        snapshot
            .ctx
            .resolve_stream(&snapshot.url, snapshot.selected_format.as_deref())?
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
        model.timeline_spectrogram = None;
        model.pending_spectrogram_video_id = None;
        model.timeline_demucs_spectrogram = None;
        model.pending_demucs_spectrogram_video_id = None;
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

    pub fn update_alignment(
        &mut self,
        lyric_index: usize,
        update: impl FnOnce(&mut AlignmentLine),
    ) {
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

fn import_json_with_dialog(
    window: &ApplicationWindow,
    view: Rc<UiView>,
    work_tx: std::sync::mpsc::Sender<BackgroundUpdate>,
) {
    let dialog = gtk::FileDialog::builder()
        .title("Import JSON")
        .accept_label("_Import")
        .modal(true)
        .build();

    dialog.open(
        Some(window),
        None::<&gio::Cancellable>,
        move |result| match result {
            Ok(file) => {
                let Some(path) = file.path() else {
                    view.refresh_mut(|model| {
                        model.error = Some("Could not determine import path".to_string());
                    });
                    return;
                };
                let fallback_metadata = view.model.borrow().metadata.clone();
                let import_result = fs::read_to_string(&path)
                    .map_err(|err| err.to_string())
                    .and_then(|text| parse_json_import(&text, fallback_metadata.as_ref()));
                let should_load_video = import_result
                    .as_ref()
                    .is_ok_and(|(metadata, _, _)| !metadata.original_url.trim().is_empty());
                view.refresh_mut(|model| match import_result {
                    Ok((metadata, song, alignment)) => {
                        let (show_original, show_romanization, show_english) =
                            crate::lyrics::lyric_language_toggles(&song.lines);
                        model.url = metadata.original_url.clone();
                        model.metadata = Some(metadata);
                        model.song = Some(song);
                        model.captions.clear();
                        model.alignment = alignment;
                        model.selected_format = None;
                        model.pending_stream = None;
                        model.pending_seek_ms = Some(0);
                        model.pending_autoplay = false;
                        model.timeline_spectrogram = None;
                        model.pending_spectrogram_video_id = None;
                        model.timeline_demucs_spectrogram = None;
                        model.pending_demucs_spectrogram_video_id = None;
                        model.player_loaded = false;
                        model.current_ms = 0;
                        model.active_index = 0;
                        model.show_original = show_original;
                        model.show_romanization = show_romanization;
                        model.show_english = show_english;
                        model.editor_table_dirty = true;
                        model.message = Some(format!("JSON imported from {}", path.display()));
                        model.error = None;
                    }
                    Err(err) => {
                        model.error = Some(format!("JSON import failed: {err}"));
                    }
                });
                if should_load_video {
                    spawn_timeline_spectrogram(work_tx.clone(), Rc::clone(&view));
                    spawn_timeline_demucs_spectrogram(work_tx.clone(), Rc::clone(&view));
                    spawn_work(work_tx.clone(), Rc::clone(&view), "Stream", move |snapshot| {
                        let spec = snapshot.ctx.resolve_stream(&snapshot.url, None)?;
                        Ok(Box::new(move |model: &mut UiModel| {
                            model.pending_stream = Some(spec);
                        }) as Box<dyn FnOnce(&mut UiModel) + Send>)
                    });
                }
            }
            Err(err) => {
                if !err.matches(gtk::DialogError::Dismissed) {
                    view.refresh_mut(|model| {
                        model.error = Some(format!("JSON import failed: {err}"));
                    });
                }
            }
        },
    );
}

fn export_json_with_dialog(
    window: &ApplicationWindow,
    view: Rc<UiView>,
    metadata: VideoMetadata,
    song: SongPackage,
    alignment: Vec<AlignmentLine>,
) {
    let dialog = gtk::FileDialog::builder()
        .title("Export JSON")
        .accept_label("_Export")
        .modal(true)
        .initial_name(export_filename(&song, &metadata).as_str())
        .build();

    dialog.save(
        Some(window),
        None::<&gio::Cancellable>,
        move |result| match result {
            Ok(file) => {
                let Some(path) = file.path() else {
                    view.refresh_mut(|model| {
                        model.error = Some("Could not determine export path".to_string());
                    });
                    return;
                };
                let payload = build_json_export(&metadata, &song, &alignment);
                let write_result = serde_json::to_string_pretty(&payload)
                    .map(|json| format!("{json}\n"))
                    .map_err(|err| err.to_string())
                    .and_then(|json| fs::write(&path, json).map_err(|err| err.to_string()));
                view.refresh_mut(|model| match write_result {
                    Ok(()) => {
                        model.message = Some(format!("JSON exported to {}", path.display()));
                        model.error = None;
                    }
                    Err(err) => {
                        model.error = Some(format!("JSON export failed: {err}"));
                    }
                });
            }
            Err(err) => {
                if !err.matches(gtk::DialogError::Dismissed) {
                    view.refresh_mut(|model| {
                        model.error = Some(format!("JSON export failed: {err}"));
                    });
                }
            }
        },
    );
}

fn build_json_export(
    metadata: &VideoMetadata,
    song: &SongPackage,
    alignment: &[AlignmentLine],
) -> serde_json::Value {
    crate::export::build_export_json(metadata, song, alignment)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct JsonImportPayload {
    video: JsonImportVideo,
    #[serde(default)]
    members: Vec<JsonImportMember>,
    lyrics: Vec<JsonImportLyric>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct JsonImportVideo {
    video_id: String,
    platform: Option<String>,
    url: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct JsonImportMember {
    name: String,
    color: Option<String>,
    image_url: Option<String>,
    local_image_path: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct JsonImportLyric {
    index: Option<usize>,
    start_ms: Option<i64>,
    end_ms: Option<i64>,
    layer: Option<String>,
    member: Option<String>,
    original: String,
    romanization: Option<String>,
    english: Option<String>,
}

fn parse_json_import(
    raw: &str,
    fallback_metadata: Option<&VideoMetadata>,
) -> Result<(VideoMetadata, SongPackage, Vec<AlignmentLine>), String> {
    let payload: JsonImportPayload =
        serde_json::from_str(raw).map_err(|err| format!("Invalid JSON: {err}"))?;
    if payload.lyrics.is_empty() {
        return Err("Import JSON has no lyrics".to_string());
    }

    let JsonImportVideo {
        video_id,
        platform,
        url,
    } = payload.video;
    let original_url = url
        .filter(|url| !url.trim().is_empty())
        .or_else(|| {
            let platform = platform.as_deref().unwrap_or("youtube");
            if !video_id.trim().is_empty()
                && (platform == "youtube" || platform == "youtu.be" || platform.ends_with("youtube.com"))
            {
                Some(format!("https://www.youtube.com/watch?v={video_id}"))
            } else {
                None
            }
        })
        .ok_or_else(|| "Import JSON has no video URL".to_string())?;
    let title = fallback_metadata
        .and_then(|metadata| metadata.title.clone())
        .unwrap_or_else(|| "Imported JSON".to_string());
    let artist = fallback_metadata
        .and_then(|metadata| metadata.artist_hint.clone())
        .unwrap_or_else(|| "Imported Artist".to_string());
    let metadata = VideoMetadata {
        video_id,
        title: Some(title.clone()),
        artist_hint: Some(artist.clone()),
        original_url,
    };

    let members = payload
        .members
        .into_iter()
        .filter(|member| !member.name.trim().is_empty())
        .map(|member| MemberProfile {
            id: None,
            stage_name: member.name,
            real_name: None,
            color: member.color.unwrap_or_else(|| "#5f7c8a".to_string()),
            image_url: member.image_url,
            local_image_path: member.local_image_path,
            provider: Some("json".to_string()),
        })
        .collect::<Vec<_>>();

    let mut alignment = Vec::new();
    let lines = payload
        .lyrics
        .into_iter()
        .enumerate()
        .map(|(position, lyric)| {
            let index = lyric.index.unwrap_or(position);
            if let (Some(start_ms), Some(end_ms)) = (lyric.start_ms, lyric.end_ms) {
                let start_ms = start_ms.max(0);
                alignment.push(AlignmentLine {
                    lyric_index: index,
                    caption_index: None,
                    start_ms,
                    end_ms: end_ms.max(start_ms + 1),
                    confidence: 1.0,
                    needs_review: false,
                });
            }
            LyricLine {
                id: None,
                song_id: None,
                index,
                member: lyric.member,
                original: lyric.original,
                romanization: lyric.romanization,
                english: lyric.english,
                with_all: false,
                layer: lyric
                    .layer
                    .as_deref()
                    .and_then(LyricLayer::from_str)
                    .unwrap_or_default(),
                segments: Vec::new(),
            }
        })
        .collect::<Vec<_>>();

    Ok((
        metadata,
        SongPackage {
            song: crate::models::Song {
                id: None,
                title,
                artist: artist.clone(),
                group_name: Some(artist),
                source_url: None,
            },
            lines,
            members,
            provider: "json".to_string(),
        },
        alignment,
    ))
}

fn export_filename(song: &SongPackage, metadata: &VideoMetadata) -> String {
    format!(
        "{}-{}-{}.json",
        safe_filename(&song.song.artist),
        safe_filename(&song.song.title),
        safe_filename(&metadata.video_id)
    )
}

fn safe_filename(value: &str) -> String {
    let cleaned = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .chars()
        .take(60)
        .collect::<String>();
    if cleaned.is_empty() {
        "export".to_string()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod export_tests {
    use super::{build_json_export, parse_json_import};
    use crate::models::{
        AlignmentLine, LyricLayer, LyricLine, MemberProfile, Song, SongPackage, VideoMetadata,
    };

    #[test]
    fn builds_requested_json_export_shape() {
        let metadata = VideoMetadata {
            video_id: "abc123".into(),
            title: Some("Song".into()),
            artist_hint: Some("Group".into()),
            original_url: "https://www.youtube.com/watch?v=abc123".into(),
        };
        let song = SongPackage {
            song: Song {
                id: Some(1),
                title: "Song".into(),
                artist: "Group".into(),
                group_name: Some("Group".into()),
                source_url: None,
            },
            provider: "fixture".into(),
            members: vec![MemberProfile {
                id: None,
                stage_name: "Nayeon".into(),
                real_name: None,
                color: "#e84855".into(),
                image_url: Some("https://example.com/nayeon.jpg".into()),
                local_image_path: None,
                provider: None,
            }],
            lines: vec![LyricLine {
                id: Some(1),
                song_id: Some(1),
                index: 0,
                member: Some("Nayeon".into()),
                original: "annyeong".into(),
                romanization: Some("annyeong".into()),
                english: Some("hello".into()),
                with_all: false,
                layer: LyricLayer::Backing,
                segments: Vec::new(),
            }],
        };
        let alignment = vec![AlignmentLine {
            lyric_index: 0,
            caption_index: Some(0),
            start_ms: 1000,
            end_ms: 2400,
            confidence: 1.0,
            needs_review: false,
        }];

        let payload = build_json_export(&metadata, &song, &alignment);

        assert_eq!(payload["video"]["platform"], "youtube");
        assert_eq!(payload["video"]["videoId"], "abc123");
        assert_eq!(payload["members"][0]["color"], "#e84855");
        assert_eq!(
            payload["members"][0]["imageUrl"],
            "https://example.com/nayeon.jpg"
        );
        assert_eq!(payload["lyrics"][0]["startMs"], 1000);
        assert_eq!(payload["lyrics"][0]["endMs"], 2400);
        assert_eq!(payload["lyrics"][0]["layer"], "backing");
        assert_eq!(payload["lyrics"][0]["member"], "Nayeon");
        assert_eq!(payload["lyrics"][0]["original"], "annyeong");
        assert_eq!(payload["lyrics"][0]["romanization"], "annyeong");
        assert_eq!(payload["lyrics"][0]["english"], "hello");
    }

    #[test]
    fn imports_export_json_shape_into_song_and_alignment() {
        let fallback = VideoMetadata {
            video_id: "fallback".into(),
            title: Some("Song".into()),
            artist_hint: Some("Group".into()),
            original_url: "https://www.youtube.com/watch?v=fallback".into(),
        };
        let raw = r##"{
            "version": 1,
            "video": {
                "platform": "youtube",
                "videoId": "abc123",
                "url": "https://www.youtube.com/watch?v=abc123"
            },
            "members": [
                {
                    "name": "Nayeon",
                    "color": "#e84855",
                    "imageUrl": "https://example.com/nayeon.jpg",
                    "localImagePath": null
                }
            ],
            "lyrics": [
                {
                    "index": 0,
                    "startMs": 1000,
                    "endMs": 2400,
                    "layer": "backing",
                    "member": "Nayeon",
                    "original": "annyeong",
                    "romanization": "annyeong",
                    "english": "hello"
                }
            ]
        }"##;

        let (metadata, song, alignment) = parse_json_import(raw, Some(&fallback)).unwrap();

        assert_eq!(metadata.video_id, "abc123");
        assert_eq!(metadata.original_url, "https://www.youtube.com/watch?v=abc123");
        assert_eq!(song.song.title, "Song");
        assert_eq!(song.members[0].stage_name, "Nayeon");
        assert_eq!(song.lines[0].layer, LyricLayer::Backing);
        assert_eq!(song.lines[0].english.as_deref(), Some("hello"));
        assert_eq!(alignment[0].lyric_index, 0);
        assert_eq!(alignment[0].start_ms, 1000);
        assert_eq!(alignment[0].end_ms, 2400);
    }
}
