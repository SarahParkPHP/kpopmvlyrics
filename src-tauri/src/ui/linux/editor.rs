#![cfg(target_os = "linux")]

use std::cell::RefCell;
use std::rc::Rc;

use gtk::gio;
use gtk::prelude::*;
use gtk::{
    ApplicationWindow, Box as GtkBox, Button, Grid, Label, Orientation, Revealer, ScrolledWindow,
    SpinButton, TextView,
};

use crate::app::{shift_alignment, DEFAULT_MANUAL_CAPTIONS, DEFAULT_MANUAL_LYRICS};
use crate::log::{progress, verbose, PhaseGuard};
use crate::models::{AlignmentLine, MemberProfile};

use super::{spawn_work, BackgroundUpdate, IdDropDown, UiModel, UiView, WorkerSnapshot};

pub struct EditorWidgets {
    pub revealer: Revealer,
    pub table_box: GtkBox,
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

    let table_scroll = ScrolledWindow::new();
    table_scroll.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    table_scroll.set_min_content_height(180);
    table_scroll.set_vexpand(true);
    let table_box = GtkBox::new(Orientation::Vertical, 2);
    table_scroll.set_child(Some(&table_box));
    panel.append(&table_scroll);

    let revealer = Revealer::new();
    revealer.set_reveal_child(false);
    revealer.set_child(Some(&panel));

    EditorBuild {
        revealer: revealer.clone(),
        widgets: EditorWidgets {
            revealer,
            table_box,
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
                let song_id = snapshot
                    .song
                    .as_ref()
                    .and_then(|song| song.song.id)
                    .ok_or_else(|| "Load lyrics first".to_string())?;
                let video_id = snapshot
                    .metadata
                    .as_ref()
                    .map(|meta| meta.video_id.clone())
                    .ok_or_else(|| "Resolve a YouTube URL first".to_string())?;
                snapshot
                    .ctx
                    .save_alignment_edits(song_id, &video_id, &alignment)?;
                Ok(Box::new(|_model: &mut UiModel| {}) as Box<dyn FnOnce(&mut UiModel) + Send>)
            });
        });
    }

    let _ = window;
}

impl UiView {
    pub fn render_editor_table(&self) {
        if !self.editor.revealer.reveals_child() {
            return;
        }

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

        let Ok(model) = self.model.try_borrow() else {
            return;
        };
        let key = editor_render_key(&model);
        *self.editor.render_key.borrow_mut() = key;
        clear_box(&self.editor.table_box);
        render_alignment_table(&self.editor.table_box, &model, Rc::clone(&self.model));
        drop(model);
        if let Ok(mut model) = self.model.try_borrow_mut() {
            model.editor_table_dirty = false;
        }
    }
}

fn editor_render_key(model: &UiModel) -> String {
    let lines = model
        .song
        .as_ref()
        .map(|song| {
            song.lines
                .iter()
                .map(|line| format!("{}:{}", line.index, line.member.as_deref().unwrap_or("")))
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

fn clear_box(container: &GtkBox) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}

fn render_alignment_table(container: &GtkBox, model: &UiModel, model_rc: Rc<RefCell<UiModel>>) {
    let Some(song) = &model.song else {
        container.append(&Label::new(Some("Load lyrics to edit alignment timing.")));
        return;
    };

    let header = Grid::new();
    header.set_column_spacing(8);
    header.set_row_spacing(4);
    for (col, title) in ["Line", "Member", "Start", "End", "Confidence"]
        .into_iter()
        .enumerate()
    {
        let label = Label::new(Some(title));
        label.set_markup(&format!("<b>{title}</b>"));
        header.attach(&label, col as i32, 0, 1, 1);
    }
    container.append(&header);

    for (row_idx, line) in song.lines.iter().enumerate() {
        let timing = alignment_for_line(&model.alignment, line.index);
        let grid = Grid::new();
        grid.set_column_spacing(8);
        grid.set_row_spacing(4);

        let text = Label::new(Some(&line.original));
        text.set_xalign(0.0);
        text.set_wrap(true);
        text.set_max_width_chars(40);
        grid.attach(&text, 0, 0, 1, 1);

        let member_combo = IdDropDown::new();
        member_combo.append("", "All");
        for member in &song.members {
            member_combo.append(&member.stage_name, &member.stage_name);
        }
        if let Some(name) = &line.member {
            member_combo.set_active_id(name);
        } else {
            member_combo.set_active_id("");
        }
        grid.attach(&member_combo.widget, 1, 0, 1, 1);

        let start = SpinButton::with_range(0.0, 3_600_000.0, 100.0);
        start.set_value(timing.start_ms as f64);
        grid.attach(&start, 2, 0, 1, 1);

        let end = SpinButton::with_range(0.0, 3_600_000.0, 100.0);
        end.set_value(timing.end_ms as f64);
        grid.attach(&end, 3, 0, 1, 1);

        {
            let model_rc = Rc::clone(&model_rc);
            let line_index = line.index;
            member_combo.connect_changed(move |combo| {
                let member = combo
                    .active_id()
                    .filter(|id| !id.is_empty());
                if let Ok(mut model) = model_rc.try_borrow_mut() {
                    model.set_line_member(line_index, member);
                }
            });
        }
        {
            let model_rc = Rc::clone(&model_rc);
            let line_index = line.index;
            start.connect_value_changed(move |spin| {
                if let Ok(mut model) = model_rc.try_borrow_mut() {
                    model.update_alignment(line_index, |line| {
                        line.start_ms = spin.value() as i64;
                        line.needs_review = true;
                    });
                }
            });
        }
        {
            let model_rc = Rc::clone(&model_rc);
            let line_index = line.index;
            end.connect_value_changed(move |spin| {
                if let Ok(mut model) = model_rc.try_borrow_mut() {
                    model.update_alignment(line_index, |line| {
                        line.end_ms = spin.value() as i64;
                        line.needs_review = true;
                    });
                }
            });
        }
        let confidence = Label::new(Some(&format!(
            "{}%{}",
            (timing.confidence * 100.0).round() as i32,
            if timing.needs_review { " review" } else { "" }
        )));
        grid.attach(&confidence, 4, 0, 1, 1);

        container.append(&grid);
        if row_idx + 1 < song.lines.len() {
            container.append(&gtk::Separator::new(Orientation::Horizontal));
        }
    }
}

fn alignment_for_line(alignment: &[AlignmentLine], lyric_index: usize) -> AlignmentLine {
    alignment
        .iter()
        .find(|line| line.lyric_index == lyric_index)
        .cloned()
        .unwrap_or(AlignmentLine {
            lyric_index,
            caption_index: None,
            start_ms: 0,
            end_ms: 1200,
            confidence: 0.0,
            needs_review: true,
        })
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
}
