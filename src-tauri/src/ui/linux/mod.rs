#![cfg(target_os = "linux")]

mod editor;
mod lyrics;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::rc::Rc;
use std::sync::Arc;

use gtk::prelude::*;
use gtk::{
    Application, ApplicationWindow, Box as GtkBox, Button, CheckButton, ComboBoxText, Entry,
    Label, Orientation, Paned, Revealer, RevealerTransitionType, ScrolledWindow, Separator,
    WindowPosition,
};

use crate::align::is_synced_line;
use crate::app::{format_ms, merge_members, AppContext};
use crate::models::{
    AlignmentLine, CaptionLine, MemberProfile, SongPackage, StreamSpec, VideoFormat,
    VideoMetadata, VideoPosition,
};
use crate::player::PlaybackEvents;
use crate::player::NativeLinuxPlayer;

use editor::{build_editor_panel, connect_editor_handlers, pick_member_image, resolve_video_chain, EditorWidgets};
use lyrics::{
    compute_lyric_stage_content, lyric_content_key, LyricStage, LyricStageContent,
};

const APP_ID: &str = "com.kpopmvlyrics.desktop";

pub fn run() {
    let application = Application::builder().application_id(APP_ID).build();
    application.connect_activate(|app| {
        if let Err(err) = build_main_window(app) {
            eprintln!("Failed to start K-Pop MV Lyrics: {err}");
        }
    });
    application.run();
}

struct BackgroundUpdate {
    label: &'static str,
    result: Result<Box<dyn FnOnce(&mut UiModel) + Send>, String>,
}

struct WorkerSnapshot {
    ctx: Arc<AppContext>,
    url: String,
    query: String,
    metadata: Option<VideoMetadata>,
    song: Option<SongPackage>,
    selected_format: Option<String>,
}

impl WorkerSnapshot {
    fn from_model(model: &UiModel) -> Self {
        Self {
            ctx: Arc::clone(&model.ctx),
            url: model.url.clone(),
            query: model.query.clone(),
            metadata: model.metadata.clone(),
            song: model.song.clone(),
            selected_format: model.selected_format.clone(),
        }
    }
}

struct UiModel {
    ctx: Arc<AppContext>,
    player: Rc<RefCell<NativeLinuxPlayer>>,
    url: String,
    query: String,
    metadata: Option<VideoMetadata>,
    song: Option<SongPackage>,
    captions: Vec<CaptionLine>,
    alignment: Vec<AlignmentLine>,
    formats: Vec<VideoFormat>,
    selected_format: Option<String>,
    player_loaded: bool,
    current_ms: i64,
    active_index: usize,
    show_original: bool,
    show_romanization: bool,
    show_english: bool,
    busy: Option<String>,
    message: Option<String>,
    error: Option<String>,
    pending_stream: Option<StreamSpec>,
    editor_table_dirty: bool,
}

struct MemberButton {
    stage_name: String,
    button: Button,
}

struct MemberStage {
    content_key: String,
    buttons: Vec<MemberButton>,
    last_active: RefCell<Option<String>>,
}

impl MemberStage {
    fn new() -> Self {
        Self {
            content_key: String::new(),
            buttons: Vec::new(),
            last_active: RefCell::new(None),
        }
    }

    fn set_active(&self, active_member: Option<&str>) {
        let active = active_member.map(str::to_lowercase);
        if *self.last_active.borrow() == active {
            return;
        }
        *self.last_active.borrow_mut() = active.clone();
        for entry in &self.buttons {
            let is_active = active
                .as_ref()
                .is_some_and(|name| name.eq_ignore_ascii_case(&entry.stage_name));
            let context = entry.button.style_context();
            if is_active {
                context.add_class(gtk::STYLE_CLASS_SUGGESTED_ACTION);
            } else {
                context.remove_class(gtk::STYLE_CLASS_SUGGESTED_ACTION);
            }
        }
    }
}

struct UiView {
    this: Rc<RefCell<Option<Rc<UiView>>>>,
    model: Rc<RefCell<UiModel>>,
    window: ApplicationWindow,
    status_label: Label,
    clock_label: Label,
    lyric_scroll: ScrolledWindow,
    lyric_box: GtkBox,
    member_box: GtkBox,
    quality_combo: ComboBoxText,
    settings_revealer: Revealer,
    query_entry: Entry,
    editor: EditorWidgets,
    lyric_stage: Rc<RefCell<LyricStage>>,
    member_stage: Rc<RefCell<MemberStage>>,
    member_render_key: Rc<RefCell<String>>,
    lyric_build_key: Rc<RefCell<String>>,
    format_render_key: Rc<RefCell<String>>,
    member_image_cache: Rc<RefCell<HashMap<String, String>>>,
    member_image_pending: Rc<RefCell<HashSet<String>>>,
    member_image_tx: std::sync::mpsc::Sender<(String, Option<String>)>,
    lyric_build_tx: std::sync::mpsc::Sender<(String, LyricStageContent)>,
}

fn build_main_window(app: &Application) -> Result<(), String> {
    let ctx = Arc::new(AppContext::open()?);

    let (position_tx, position_rx) = std::sync::mpsc::channel::<VideoPosition>();
    let (error_tx, error_rx) = std::sync::mpsc::channel::<String>();

    let events = PlaybackEvents {
        on_position: Some(Rc::new(move |position| {
            let _ = position_tx.send(position);
        })),
        on_error: Some(Rc::new(move |message| {
            let _ = error_tx.send(message);
        })),
    };

    let player = Rc::new(RefCell::new(NativeLinuxPlayer::new(events)));
    let model = Rc::new(RefCell::new(UiModel {
        ctx,
        player: Rc::clone(&player),
        url: "https://www.youtube.com/watch?v=dQw4w9WgXcQ".to_string(),
        query: String::new(),
        metadata: None,
        song: None,
        captions: Vec::new(),
        alignment: Vec::new(),
        formats: Vec::new(),
        selected_format: None,
        player_loaded: false,
        current_ms: 0,
        active_index: 0,
        show_original: true,
        show_romanization: false,
        show_english: true,
        busy: None,
        message: None,
        error: None,
        pending_stream: None,
        editor_table_dirty: false,
    }));

    let window = ApplicationWindow::builder()
        .application(app)
        .title("K-Pop MV Lyrics")
        .default_width(1240)
        .default_height(860)
        .window_position(WindowPosition::Center)
        .build();

    let paned = Paned::new(Orientation::Vertical);
    let top = GtkBox::new(Orientation::Vertical, 8);
    top.set_margin_top(8);
    top.set_margin_bottom(8);
    top.set_margin_start(10);
    top.set_margin_end(10);

    let url_entry = Entry::new();
    url_entry.set_hexpand(true);
    url_entry.set_placeholder_text(Some("Paste a YouTube MV URL"));
    url_entry.set_text(&model.borrow().url);

    let quality_combo = ComboBoxText::new();
    quality_combo.append(None, "Auto");
    quality_combo.set_active(Some(0));

    let open_button = Button::with_label("Open");
    let stream_button = Button::with_label("Stream");
    let settings_button = Button::with_label("Settings");
    let editor_button = Button::with_label("Editor");

    let toolbar = GtkBox::new(Orientation::Horizontal, 6);
    toolbar.pack_start(&url_entry, true, true, 0);
    toolbar.pack_start(&quality_combo, false, false, 0);
    toolbar.pack_start(&open_button, false, false, 0);
    toolbar.pack_start(&stream_button, false, false, 0);
    toolbar.pack_start(&settings_button, false, false, 0);
    toolbar.pack_start(&editor_button, false, false, 0);

    let member_scroll = ScrolledWindow::new(None::<&gtk::Adjustment>, None::<&gtk::Adjustment>);
    member_scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Never);
    member_scroll.set_hexpand(true);
    let member_box = GtkBox::new(Orientation::Horizontal, 8);
    member_scroll.add(&member_box);

    let lang_box = GtkBox::new(Orientation::Horizontal, 6);
    let original_toggle = CheckButton::with_label("Original");
    original_toggle.set_active(true);
    let roman_toggle = CheckButton::with_label("Roman");
    let english_toggle = CheckButton::with_label("English");
    english_toggle.set_active(true);
    lang_box.pack_start(&original_toggle, false, false, 0);
    lang_box.pack_start(&roman_toggle, false, false, 0);
    lang_box.pack_start(&english_toggle, false, false, 0);

    let clock_label = Label::new(None);
    clock_label.set_halign(gtk::Align::End);
    clock_label.set_markup("<span size='large'><b>0:00.000</b></span>");

    let lyric_scroll = ScrolledWindow::new(None::<&gtk::Adjustment>, None::<&gtk::Adjustment>);
    lyric_scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    lyric_scroll.set_vexpand(true);
    let lyric_box = GtkBox::new(Orientation::Vertical, 10);
    lyric_scroll.add(&lyric_box);

    let play_button = Button::with_label("Start Sync");
    let pause_button = Button::with_label("Pause Sync");
    let reset_button = Button::with_label("Reset Sync");
    let controls = GtkBox::new(Orientation::Horizontal, 6);
    controls.pack_start(&play_button, false, false, 0);
    controls.pack_start(&pause_button, false, false, 0);
    controls.pack_start(&reset_button, false, false, 0);

    let video_play_button = icon_media_button("media-playback-start", "Play");
    let video_pause_button = icon_media_button("media-playback-pause", "Pause");
    let video_stop_button = icon_media_button("media-playback-stop", "Stop");
    let video_replay_button = icon_media_button("media-seek-backward", "Replay");
    let video_controls = GtkBox::new(Orientation::Horizontal, 6);
    video_controls.set_margin_top(4);
    video_controls.set_margin_bottom(4);
    video_controls.set_margin_start(6);
    video_controls.set_margin_end(6);
    video_controls.pack_start(&video_play_button, false, false, 0);
    video_controls.pack_start(&video_pause_button, false, false, 0);
    video_controls.pack_start(&video_stop_button, false, false, 0);
    video_controls.pack_start(&video_replay_button, false, false, 0);

    let status_label = Label::new(None);
    status_label.set_halign(gtk::Align::Start);
    status_label.set_xalign(0.0);
    status_label.set_line_wrap(true);

    let query_entry = Entry::new();
    query_entry.set_placeholder_text(Some("Artist and song title"));
    let fetch_lyrics_button = Button::with_label("Fetch Lyrics");
    let fetch_captions_button = Button::with_label("Fetch Captions");
    let align_button = Button::with_label("Align");
    let save_button = Button::with_label("Save");
    let settings_panel = GtkBox::new(Orientation::Vertical, 6);
    settings_panel.pack_start(&query_entry, false, false, 0);
    let settings_actions = GtkBox::new(Orientation::Horizontal, 6);
    settings_actions.pack_start(&fetch_lyrics_button, false, false, 0);
    settings_actions.pack_start(&fetch_captions_button, false, false, 0);
    settings_actions.pack_start(&align_button, false, false, 0);
    settings_actions.pack_start(&save_button, false, false, 0);
    settings_panel.pack_start(&settings_actions, false, false, 0);

    let editor_build = build_editor_panel();
    let editor_revealer = editor_build.revealer.clone();
    let settings_revealer = Revealer::new();
    settings_revealer.set_transition_type(RevealerTransitionType::SlideDown);
    settings_revealer.set_reveal_child(false);
    settings_revealer.add(&settings_panel);

    top.pack_start(&toolbar, false, false, 0);
    top.pack_start(&member_scroll, false, false, 0);
    top.pack_start(&lang_box, false, false, 0);
    top.pack_start(&clock_label, false, false, 0);
    top.pack_start(&lyric_scroll, true, true, 0);
    top.pack_start(&controls, false, false, 0);
    top.pack_start(&Separator::new(Orientation::Horizontal), false, false, 0);
    top.pack_start(&status_label, false, false, 0);
    top.pack_start(&settings_revealer, false, false, 0);
    top.pack_start(&editor_revealer, false, false, 0);

    let video_box = player.borrow().video_widget().clone();
    video_box.set_vexpand(true);

    let video_pane = GtkBox::new(Orientation::Vertical, 0);
    video_pane.pack_start(&video_controls, false, false, 0);
    video_pane.pack_start(&video_box, true, true, 0);

    paned.add1(&top);
    paned.add2(&video_pane);
    paned.set_position(320);

    window.add(&paned);

    let (work_tx, work_rx) = std::sync::mpsc::channel::<BackgroundUpdate>();
    let (member_image_tx, member_image_rx) =
        std::sync::mpsc::channel::<(String, Option<String>)>();
    let (lyric_build_tx, lyric_build_rx) =
        std::sync::mpsc::channel::<(String, LyricStageContent)>();

    let view = Rc::new(UiView {
        this: Rc::new(RefCell::new(None)),
        model: Rc::clone(&model),
        window: window.clone(),
        status_label: status_label.clone(),
        clock_label: clock_label.clone(),
        lyric_scroll: lyric_scroll.clone(),
        lyric_box: lyric_box.clone(),
        member_box: member_box.clone(),
        quality_combo: quality_combo.clone(),
        settings_revealer: settings_revealer.clone(),
        query_entry: query_entry.clone(),
        editor: EditorWidgets {
            revealer: editor_build.widgets.revealer.clone(),
            table_box: editor_build.widgets.table_box.clone(),
            render_key: Rc::new(RefCell::new(String::new())),
        },
        lyric_stage: Rc::new(RefCell::new(LyricStage::new())),
        member_stage: Rc::new(RefCell::new(MemberStage::new())),
        member_render_key: Rc::new(RefCell::new(String::new())),
        lyric_build_key: Rc::new(RefCell::new(String::new())),
        format_render_key: Rc::new(RefCell::new(String::new())),
        member_image_cache: Rc::new(RefCell::new(HashMap::new())),
        member_image_pending: Rc::new(RefCell::new(HashSet::new())),
        member_image_tx,
        lyric_build_tx,
    });
    *view.this.borrow_mut() = Some(Rc::clone(&view));

    let view_for_tick = Rc::clone(&view);
    gtk::glib::timeout_add_local(std::time::Duration::from_millis(33), move || {
        if catch_unwind(AssertUnwindSafe(|| {
            let mut needs_full_refresh = false;
            while let Ok(position) = position_rx.try_recv() {
                if let Ok(mut model) = view_for_tick.model.try_borrow_mut() {
                    model.apply_position(position.ms as i64, position.playing, position.buffering);
                }
            }
            while let Ok(message) = error_rx.try_recv() {
                if let Ok(mut model) = view_for_tick.model.try_borrow_mut() {
                    model.error = Some(message);
                }
            }
            while let Ok((url, path)) = member_image_rx.try_recv() {
                view_for_tick
                    .member_image_pending
                    .borrow_mut()
                    .remove(&url);
                if let Some(path) = path {
                    view_for_tick
                        .member_image_cache
                        .borrow_mut()
                        .insert(url, path);
                }
                *view_for_tick.member_render_key.borrow_mut() = String::new();
                needs_full_refresh = true;
            }
            while let Ok((content_key, content)) = lyric_build_rx.try_recv() {
                view_for_tick.lyric_stage.borrow_mut().apply_content(
                    &view_for_tick.lyric_box,
                    content_key,
                    content,
                );
                needs_full_refresh = true;
            }
            while let Ok(update) = work_rx.try_recv() {
                if let Ok(mut model) = view_for_tick.model.try_borrow_mut() {
                    model.set_busy(None);
                }
                match update.result {
                    Ok(apply) => {
                        if let Ok(mut model) = view_for_tick.model.try_borrow_mut() {
                            apply(&mut model);
                            model.message = Some(if update.label == "Open" {
                                "Video, lyrics, captions, and alignment complete".to_string()
                            } else {
                                format!("{} complete", update.label)
                            });
                        }
                        needs_full_refresh = true;
                    }
                    Err(err) => {
                        if let Ok(mut model) = view_for_tick.model.try_borrow_mut() {
                            model.error = Some(err);
                        }
                        needs_full_refresh = true;
                    }
                }
                let pending_spec = view_for_tick
                    .model
                    .try_borrow_mut()
                    .ok()
                    .and_then(|mut model| model.pending_stream.take());
                if let Some(spec) = pending_spec {
                    let view = Rc::clone(&view_for_tick);
                    gtk::glib::idle_add_local_once(move || {
                        spawn_player_load(view, spec);
                    });
                }
                view_for_tick.render_editor_table();
            }
            if needs_full_refresh {
                view_for_tick.refresh();
            } else {
                view_for_tick.refresh_playback();
            }
        }))
        .is_err()
        {
            eprintln!("kpopmvlyrics: UI refresh tick panicked");
        }
        gtk::glib::ControlFlow::Continue
    });

    connect_view_handlers(
        &view,
        work_tx.clone(),
        &url_entry,
        &quality_combo,
        &open_button,
        &stream_button,
        &settings_button,
        &original_toggle,
        &roman_toggle,
        &english_toggle,
        &play_button,
        &pause_button,
        &reset_button,
        &fetch_lyrics_button,
        &fetch_captions_button,
        &align_button,
        &save_button,
    );

    connect_video_handlers(
        &view,
        &video_play_button,
        &video_pause_button,
        &video_stop_button,
        &video_replay_button,
    );

    connect_editor_handlers(
        &view,
        &window,
        work_tx,
        &editor_build,
        &editor_button,
    );

    view.refresh();
    window.show_all();
    Ok(())
}

impl UiModel {
    fn apply_position(&mut self, current_ms: i64, playing: bool, buffering: bool) {
        self.current_ms = current_ms;
        self.active_index = active_lyric_index(&self.alignment, current_ms);
        if buffering {
            self.message = Some("Buffering video".to_string());
        } else if playing {
            self.message = Some("Sync running".to_string());
        } else if self.message.as_deref() == Some("Buffering video")
            || self.message.as_deref() == Some("Sync running")
        {
            self.message = Some("Video ready".to_string());
        }
    }

    fn set_busy(&mut self, label: Option<&str>) {
        self.busy = label.map(str::to_string);
        if label.is_some() {
            self.error = None;
            self.message = None;
        }
    }

    fn clone_for_thread(&self) -> WorkerSnapshot {
        WorkerSnapshot::from_model(self)
    }
}

impl UiView {
    fn refresh(&self) {
        let Ok(model) = self.model.try_borrow() else {
            return;
        };
        self.clock_label.set_markup(&format!(
            "<span size='large'><b>{}</b></span>",
            format_ms(model.current_ms)
        ));
        self.query_entry.set_text(&model.query);
        render_status(&self.status_label, &model);
        self.schedule_lyric_build(&model);
        drop(model);

        if let Ok(model) = self.model.try_borrow() {
            render_members(self, &model);
            render_formats(self, &model);
            self.sync_active_line(&model);
        }
    }

    fn refresh_playback(&self) {
        let Ok(model) = self.model.try_borrow() else {
            return;
        };
        self.clock_label.set_markup(&format!(
            "<span size='large'><b>{}</b></span>",
            format_ms(model.current_ms)
        ));
        render_status(&self.status_label, &model);
        self.sync_active_line(&model);
    }

    fn sync_active_line(&self, model: &UiModel) {
        self.lyric_stage
            .borrow()
            .set_active(model.active_index, &self.lyric_scroll);
        let active_member = model
            .song
            .as_ref()
            .and_then(|song| {
                song.lines
                    .iter()
                    .find(|line| line.index == model.active_index)
                    .and_then(|line| line.member.as_deref())
            });
        self.member_stage.borrow().set_active(active_member);
    }

    fn schedule_lyric_build(&self, model: &UiModel) {
        let content_key = lyric_content_key(
            model.song.as_ref(),
            &model.alignment,
            model.show_original,
            model.show_romanization,
            model.show_english,
        );
        if content_key == *self.lyric_build_key.borrow()
            || content_key == self.lyric_stage.borrow().content_key()
        {
            return;
        }
        *self.lyric_build_key.borrow_mut() = content_key.clone();

        let song = model.song.clone();
        let alignment = model.alignment.clone();
        let show_original = model.show_original;
        let show_romanization = model.show_romanization;
        let show_english = model.show_english;
        let tx = self.lyric_build_tx.clone();
        std::thread::spawn(move || {
            let content = compute_lyric_stage_content(
                song,
                &alignment,
                show_original,
                show_romanization,
                show_english,
            );
            let _ = tx.send((content_key, content));
        });
    }

    fn refresh_mut<F>(&self, update: F)
    where
        F: FnOnce(&mut UiModel),
    {
        if let Ok(mut model) = self.model.try_borrow_mut() {
            update(&mut model);
        }
        self.refresh();
    }
}

fn connect_view_handlers(
    view: &Rc<UiView>,
    work_tx: std::sync::mpsc::Sender<BackgroundUpdate>,
    url_entry: &Entry,
    quality_combo: &ComboBoxText,
    open_button: &Button,
    stream_button: &Button,
    settings_button: &Button,
    original_toggle: &CheckButton,
    roman_toggle: &CheckButton,
    english_toggle: &CheckButton,
    play_button: &Button,
    pause_button: &Button,
    reset_button: &Button,
    fetch_lyrics_button: &Button,
    fetch_captions_button: &Button,
    align_button: &Button,
    save_button: &Button,
) {
    {
        let view = Rc::clone(view);
        let url_entry = url_entry.clone();
        let quality_combo = quality_combo.clone();
        let work_tx = work_tx.clone();
        open_button.connect_clicked(move |_| {
            let format_id = selected_format_id(&quality_combo);
            view.refresh_mut(|model| {
                model.url = url_entry.text().to_string();
                model.selected_format = format_id;
                model.captions.clear();
                model.alignment.clear();
                model.player_loaded = false;
                model.current_ms = 0;
                model.active_index = 0;
            });
            let view = Rc::clone(&view);
            spawn_work(work_tx.clone(), view, "Open", resolve_video_chain);
        });
    }

    {
        let view = Rc::clone(view);
        let url_entry = url_entry.clone();
        let quality_combo = quality_combo.clone();
        let stream_work_tx = work_tx.clone();
        stream_button.connect_clicked(move |_| {
            let format_id = selected_format_id(&quality_combo);
            view.refresh_mut(|model| {
                model.url = url_entry.text().to_string();
                model.selected_format = format_id;
            });
            let view = Rc::clone(&view);
            spawn_work(stream_work_tx.clone(), view, "Stream", move |snapshot| {
                let spec = snapshot.ctx.resolve_stream(
                    &snapshot.url,
                    snapshot.selected_format.as_deref(),
                )?;
                Ok(Box::new(move |model: &mut UiModel| {
                    model.pending_stream = Some(spec);
                }) as Box<dyn FnOnce(&mut UiModel) + Send>)
            });
        });
    }

    {
        let view = Rc::clone(view);
        settings_button.connect_clicked(move |_| {
            let revealed = view.settings_revealer.reveals_child();
            view.settings_revealer.set_reveal_child(!revealed);
        });
    }

    for (toggle, field) in [
        (original_toggle, LanguageField::Original),
        (roman_toggle, LanguageField::Romanization),
        (english_toggle, LanguageField::English),
    ] {
        let view = Rc::clone(view);
        toggle.connect_toggled(move |button| {
            let active = button.is_active();
            view.refresh_mut(|model| match field {
                LanguageField::Original => model.show_original = active,
                LanguageField::Romanization => model.show_romanization = active,
                LanguageField::English => model.show_english = active,
            });
        });
    }

    {
        let view = Rc::clone(view);
        play_button.connect_clicked(move |_| {
            let view = Rc::clone(&view);
            let start_ms = view
                .model
                .try_borrow()
                .ok()
                .and_then(|model| {
                    model
                        .alignment
                        .first()
                        .map(|line| line.start_ms.max(0) as u64)
                })
                .unwrap_or(0);
            spawn_player_work(view, move |player| {
                player.seek(start_ms)?;
                player.play()
            });
        });
    }

    {
        let view = Rc::clone(view);
        pause_button.connect_clicked(move |_| {
            let view = Rc::clone(&view);
            spawn_player_work(view, |player| player.pause());
        });
    }

    {
        let view = Rc::clone(view);
        reset_button.connect_clicked(move |_| {
            let view = Rc::clone(&view);
            spawn_player_work(view.clone(), |player| {
                player.pause()?;
                player.seek(0)
            });
            view.refresh_mut(|model| {
                model.current_ms = 0;
                model.active_index = 0;
            });
        });
    }

    {
        let view = Rc::clone(view);
        let query_entry = view.query_entry.clone();
        let work_tx = work_tx.clone();
        fetch_lyrics_button.connect_clicked(move |_| {
            view.refresh_mut(|model| model.query = query_entry.text().to_string());
            let view = Rc::clone(&view);
            spawn_work(work_tx.clone(), view, "Lyrics", move |snapshot| {
                let mut package = snapshot.ctx.fetch_lyrics(&snapshot.query)?;
                if let Some(group) = package.song.group_name.clone() {
                    if let Ok(profiles) = snapshot.ctx.search_member_profiles(&group) {
                        package.members = merge_members(&package.members, &profiles);
                    }
                }
                Ok(Box::new(move |model: &mut UiModel| {
                    model.song = Some(package);
                    model.editor_table_dirty = true;
                }) as Box<dyn FnOnce(&mut UiModel) + Send>)
            });
        });
    }

    {
        let view = Rc::clone(view);
        let work_tx = work_tx.clone();
        fetch_captions_button.connect_clicked(move |_| {
            let view = Rc::clone(&view);
            spawn_work(work_tx.clone(), view, "Captions", |snapshot| {
                let video_id = snapshot
                    .metadata
                    .as_ref()
                    .map(|meta| meta.video_id.clone())
                    .ok_or_else(|| "Resolve a YouTube URL first".to_string())?;
                let captions = snapshot.ctx.fetch_captions(&video_id)?;
                Ok(Box::new(move |model: &mut UiModel| {
                    model.captions = captions;
                }) as Box<dyn FnOnce(&mut UiModel) + Send>)
            });
        });
    }

    {
        let view = Rc::clone(view);
        let work_tx = work_tx.clone();
        align_button.connect_clicked(move |_| {
            let view = Rc::clone(&view);
            spawn_work(work_tx.clone(), view, "Alignment", |snapshot| {
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
                let result = snapshot.ctx.align_lyrics(song_id, &video_id)?;
                Ok(Box::new(move |model: &mut UiModel| {
                    model.captions = result.captions;
                    model.alignment = result.alignment;
                    model.editor_table_dirty = true;
                }) as Box<dyn FnOnce(&mut UiModel) + Send>)
            });
        });
    }

    {
        let view = Rc::clone(view);
        let work_tx = work_tx.clone();
        save_button.connect_clicked(move |_| {
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
}

enum LanguageField {
    Original,
    Romanization,
    English,
}

fn spawn_work<F>(
    work_tx: std::sync::mpsc::Sender<BackgroundUpdate>,
    view: Rc<UiView>,
    label: &'static str,
    work: F,
)
where
    F: FnOnce(WorkerSnapshot) -> Result<Box<dyn FnOnce(&mut UiModel) + Send>, String>
        + Send
        + 'static,
{
    view.refresh_mut(|model| model.set_busy(Some(label)));
    let snapshot = view.model.borrow().clone_for_thread();
    std::thread::spawn(move || {
        let result = work(snapshot);
        let _ = work_tx.send(BackgroundUpdate { label, result });
    });
}

fn spawn_player_load(view: Rc<UiView>, spec: StreamSpec) {
    let player = match view.model.try_borrow() {
        Ok(model) => Rc::clone(&model.player),
        Err(_) => {
            if let Ok(mut model) = view.model.try_borrow_mut() {
                model.pending_stream = Some(spec);
            }
            return;
        }
    };

    let load_result = catch_unwind(AssertUnwindSafe(|| {
        player
            .try_borrow_mut()
            .map_err(|_| "Video player is busy".to_string())
            .and_then(|mut player| player.load(spec))
    }));

    match load_result {
        Ok(Ok(())) => {
            if let Ok(mut model) = view.model.try_borrow_mut() {
                model.player_loaded = true;
                model.error = None;
            }
        }
        Ok(Err(err)) => {
            if let Ok(mut model) = view.model.try_borrow_mut() {
                model.error = Some(err);
            }
        }
        Err(payload) => {
            let message = panic_payload_message(payload);
            eprintln!("kpopmvlyrics: video load panicked: {message}");
            if let Ok(mut model) = view.model.try_borrow_mut() {
                model.error = Some(format!("Video player failed: {message}"));
            }
        }
    }

    gtk::glib::idle_add_local_once(move || {
        view.refresh();
    });
}

fn spawn_player_work<F>(view: Rc<UiView>, work: F)
where
    F: FnOnce(&mut NativeLinuxPlayer) -> Result<(), String>,
{
    let result = view
        .model
        .try_borrow()
        .ok()
        .map(|model| Rc::clone(&model.player))
        .map(|player| {
            player
                .try_borrow_mut()
                .map_err(|_| "Video player is busy".to_string())
                .and_then(|mut player| work(&mut player))
        });

    if let Some(Ok(())) = result.as_ref() {
        if let Ok(mut model) = view.model.try_borrow_mut() {
            model.error = None;
        }
    }

    if let Some(Err(err)) = result {
        if let Ok(mut model) = view.model.try_borrow_mut() {
            model.error = Some(err);
        }
    }

    view.refresh();
}

fn panic_payload_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        return (*message).to_string();
    }
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }
    "unknown panic".to_string()
}

fn selected_format_id(combo: &ComboBoxText) -> Option<String> {
    combo
        .active_id()
        .filter(|id| id.as_str() != "auto")
        .map(|id| id.to_string())
}

fn clear_children(container: &GtkBox) {
    for child in container.children() {
        container.remove(&child);
    }
}

const NO_ACTIVE_LYRIC: usize = usize::MAX;

fn active_lyric_index(alignment: &[AlignmentLine], current_ms: i64) -> usize {
    let synced: Vec<&AlignmentLine> = alignment.iter().filter(|line| is_synced_line(line)).collect();

    if synced.is_empty() {
        return NO_ACTIVE_LYRIC;
    }

    if let Some(active) = synced
        .iter()
        .find(|line| current_ms >= line.start_ms && current_ms <= line.end_ms)
    {
        return active.lyric_index;
    }

    if let Some(active) = synced
        .iter()
        .filter(|line| current_ms >= line.start_ms)
        .max_by_key(|line| line.start_ms)
    {
        return active.lyric_index;
    }

    NO_ACTIVE_LYRIC
}

fn render_status(label: &Label, model: &UiModel) {
    let text = if let Some(busy) = &model.busy {
        format!("{busy} running…")
    } else if let Some(error) = &model.error {
        format!("Error: {error}")
    } else if let Some(message) = &model.message {
        message.clone()
    } else if model.song.is_none() {
        "Load a YouTube URL to fetch lyrics and start synced playback.".to_string()
    } else {
        String::new()
    };
    label.set_text(&text);
}

fn format_render_key(model: &UiModel) -> String {
    let formats = model
        .formats
        .iter()
        .map(|format| format.format_id.as_str())
        .collect::<Vec<_>>()
        .join("|");
    format!(
        "{formats}::{}",
        model.selected_format.as_deref().unwrap_or("auto")
    )
}

fn render_formats(view: &UiView, model: &UiModel) {
    let key = format_render_key(model);
    if *view.format_render_key.borrow() == key {
        return;
    }
    *view.format_render_key.borrow_mut() = key;

    let combo = &view.quality_combo;
    combo.remove_all();
    combo.append(None, "Auto");
    for format in &model.formats {
        combo.append(Some(&format.format_id), &format.label);
    }
    if let Some(selected) = &model.selected_format {
        combo.set_active_id(Some(selected));
    } else {
        combo.set_active(Some(0));
    }
}

fn connect_video_handlers(
    view: &Rc<UiView>,
    play_button: &Button,
    pause_button: &Button,
    stop_button: &Button,
    replay_button: &Button,
) {
    {
        let view = Rc::clone(view);
        play_button.connect_clicked(move |_| {
            spawn_player_work(Rc::clone(&view), |player| player.play());
        });
    }
    {
        let view = Rc::clone(view);
        pause_button.connect_clicked(move |_| {
            spawn_player_work(Rc::clone(&view), |player| player.pause());
        });
    }
    {
        let view = Rc::clone(view);
        stop_button.connect_clicked(move |_| {
            spawn_player_work(Rc::clone(&view), |player| {
                player.pause()?;
                let _ = player.seek(0);
                Ok(())
            });
        });
    }
    {
        let view = Rc::clone(view);
        replay_button.connect_clicked(move |_| {
            spawn_player_work(Rc::clone(&view), |player| {
                let _ = player.seek(0);
                player.play()
            });
        });
    }
}

fn icon_media_button(icon_name: &str, label: &str) -> Button {
    let button = Button::new();
    let row = GtkBox::new(Orientation::Horizontal, 4);
    if gtk::IconTheme::default()
        .is_some_and(|theme| theme.has_icon(icon_name))
    {
        let icon = gtk::Image::from_icon_name(Some(icon_name), gtk::IconSize::Button);
        row.pack_start(&icon, false, false, 0);
    }
    row.pack_start(&Label::new(Some(label)), false, false, 0);
    button.add(&row);
    button.set_tooltip_text(Some(label));
    button
}

fn member_content_key(model: &UiModel, image_cache: &HashMap<String, String>) -> String {
    model
        .song
        .as_ref()
        .map(|song| {
            song.members
                .iter()
                .map(|member| {
                    format!(
                        "{}:{}",
                        member.stage_name,
                        member_image_path(member, image_cache).unwrap_or_default()
                    )
                })
                .collect::<Vec<_>>()
                .join("|")
        })
        .unwrap_or_default()
}

fn member_image_path(member: &MemberProfile, image_cache: &HashMap<String, String>) -> Option<String> {
    member.local_image_path.clone().or_else(|| {
        member
            .image_url
            .as_ref()
            .and_then(|url| image_cache.get(url).cloned())
    })
}

fn prefetch_member_images(view: &UiView, members: &[MemberProfile]) {
    for member in members {
        if member.local_image_path.is_some() {
            continue;
        }
        let Some(url) = member.image_url.clone() else {
            continue;
        };
        if view.member_image_cache.borrow().contains_key(&url) {
            continue;
        }
        if !view.member_image_pending.borrow_mut().insert(url.clone()) {
            continue;
        }
        let tx = view.member_image_tx.clone();
        std::thread::spawn(move || {
            let path = cache_member_image_from_url(&url);
            let _ = tx.send((url, path));
        });
    }
}

fn cache_member_image_from_url(url: &str) -> Option<String> {
    use reqwest::blocking::Client;
    use std::collections::hash_map::DefaultHasher;
    use std::path::PathBuf;

    let mut hasher = DefaultHasher::new();
    url.hash(&mut hasher);
    let stem = format!("{:016x}", hasher.finish());

    let dir: PathBuf = dirs::data_dir()?.join("kpopmvlyrics").join("member-images");
    std::fs::create_dir_all(&dir).ok()?;

    for ext in ["jpg", "png", "webp"] {
        let path = dir.join(format!("{stem}.{ext}"));
        if path.is_file() {
            return Some(path.to_string_lossy().into_owned());
        }
    }

    let response = Client::builder()
        .user_agent("kpopmvlyrics/0.1")
        .build()
        .ok()?
        .get(url)
        .send()
        .ok()?;
    let bytes = response.bytes().ok()?;
    if bytes.is_empty() {
        return None;
    }

    let ext = if url.contains(".png") {
        "png"
    } else if url.contains(".webp") {
        "webp"
    } else {
        "jpg"
    };
    let path = dir.join(format!("{stem}.{ext}"));
    std::fs::write(&path, &bytes).ok()?;
    Some(path.to_string_lossy().into_owned())
}

const MEMBER_AVATAR_SIZE: i32 = 56;

fn scaled_avatar_pixbuf(pixbuf: &gtk::gdk_pixbuf::Pixbuf, size: i32) -> gtk::gdk_pixbuf::Pixbuf {
    let width = pixbuf.width().max(1) as f64;
    let height = pixbuf.height().max(1) as f64;
    let scale = (size as f64 / width).min(size as f64 / height);
    let target_w = (width * scale).round().max(1.0) as i32;
    let target_h = (height * scale).round().max(1.0) as i32;
    pixbuf
        .scale_simple(
            target_w,
            target_h,
            gtk::gdk_pixbuf::InterpType::Bilinear,
        )
        .unwrap_or_else(|| pixbuf.clone())
}

fn member_avatar_widget(member: &MemberProfile, image_cache: &HashMap<String, String>) -> gtk::Widget {
    let frame = GtkBox::new(Orientation::Vertical, 0);
    frame.set_size_request(MEMBER_AVATAR_SIZE, MEMBER_AVATAR_SIZE);
    frame.set_halign(gtk::Align::Center);

    if let Some(path) = member_image_path(member, image_cache) {
        if let Ok(pixbuf) = gtk::gdk_pixbuf::Pixbuf::from_file(&path) {
            let scaled = scaled_avatar_pixbuf(&pixbuf, MEMBER_AVATAR_SIZE);
            let image = gtk::Image::from_pixbuf(Some(&scaled));
            image.set_size_request(MEMBER_AVATAR_SIZE, MEMBER_AVATAR_SIZE);
            frame.pack_start(&image, true, true, 0);
            return frame.upcast();
        }
    }

    let placeholder = Label::new(Some(&initials(&member.stage_name)));
    placeholder.set_size_request(MEMBER_AVATAR_SIZE, MEMBER_AVATAR_SIZE);
    frame.pack_start(&placeholder, true, true, 0);
    frame.upcast()
}

fn render_members(view: &UiView, model: &UiModel) {
    let image_cache = view.member_image_cache.borrow().clone();
    let key = member_content_key(model, &image_cache);
    if *view.member_render_key.borrow() == key {
        return;
    }
    *view.member_render_key.borrow_mut() = key.clone();

    let container = &view.member_box;
    clear_children(container);
    view.member_stage.borrow_mut().content_key = key;
    view.member_stage.borrow_mut().buttons.clear();
    view.member_stage.borrow_mut().last_active.replace(None);

    let Some(song) = &model.song else {
        let empty = Label::new(Some("Members appear after lyrics are loaded"));
        empty.set_opacity(0.7);
        container.pack_start(&empty, false, false, 0);
        container.show_all();
        return;
    };

    prefetch_member_images(view, &song.members);

    let Some(view_rc) = view.this.try_borrow().ok().and_then(|this| this.clone()) else {
        return;
    };

    let mut stage_buttons = Vec::new();
    for member in &song.members {
        let button = Button::new();
        button.set_size_request(80, 96);
        let stage_name = member.stage_name.clone();

        let inner = GtkBox::new(Orientation::Vertical, 4);
        inner.set_halign(gtk::Align::Center);

        inner.pack_start(&member_avatar_widget(member, &image_cache), false, false, 0);

        let name = Label::new(Some(&stage_name));
        inner.pack_start(&name, false, false, 0);
        button.add(&inner);

        let member = member.clone();
        let group_name = song
            .song
            .group_name
            .clone()
            .unwrap_or_else(|| song.song.artist.clone());
        let window = view.window.clone();
        let view_for_click = Rc::clone(&view_rc);
        button.connect_clicked(move |_| {
            pick_member_image(
                &window,
                Rc::clone(&view_for_click),
                member.clone(),
                group_name.clone(),
            );
        });

        container.pack_start(&button, false, false, 0);
        stage_buttons.push(MemberButton {
            stage_name,
            button,
        });
    }
    view.member_stage.borrow_mut().buttons = stage_buttons;
    container.show_all();
}

fn initials(name: &str) -> String {
    name.split_whitespace()
        .filter(|part| !part.is_empty())
        .take(2)
        .filter_map(|part| part.chars().next())
        .map(|ch| ch.to_uppercase().collect::<String>())
        .collect()
}
