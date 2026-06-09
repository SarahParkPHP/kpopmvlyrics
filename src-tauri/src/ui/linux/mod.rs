#![cfg(desktop_unix)]

mod editor;
mod lyrics;
mod metadata;
mod timeline;
mod video_overlay;

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::rc::Rc;
use std::sync::Arc;

use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;
use gtk::{
    Application, ApplicationWindow, Box as GtkBox, Button, CheckButton, CssProvider, Entry,
    EventControllerKey, Frame, Label, Orientation, Paned, ScrolledWindow, SpinButton, Stack,
    StackSwitcher,
};

use crate::align::has_playback_timing;
use crate::app::{apply_member_profiles, format_ms, AppContext};
use crate::asr::AsrModelSize;
use crate::models::{
    AlignmentLine, AudioSpectrogram, CaptionLine, MemberProfile, SongPackage, StreamSpec,
    VideoFormat, VideoMetadata, VideoPosition,
};
use crate::player::NativeLinuxPlayer;
use crate::player::PlaybackEvents;

use editor::{
    build_editor_panel, connect_editor_handlers, pick_member_image, resolve_video_chain,
    EditorWidgets, EDITOR_PAGE, PLAYBACK_PAGE, SETTINGS_PAGE,
};
use metadata::{MetadataPanel, METADATA_PAGE};
use lyrics::{compute_lyric_stage_content, lyric_content_key, LyricStage, LyricStageContent};
use video_overlay::{build_video_overlay, VideoOverlay};

const APP_ID: &str = "com.kpopmvlyrics.desktop";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ThemePreference {
    System,
    Light,
    Dark,
}

impl ThemePreference {
    fn from_storage(value: &str) -> Self {
        match value {
            "light" => Self::Light,
            "dark" => Self::Dark,
            _ => Self::System,
        }
    }

    fn as_storage(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::System => "System",
            Self::Light => "Light",
            Self::Dark => "Dark",
        }
    }
}

/// `gtk::DropDown` driven by a parallel `Vec<String>` of option IDs so we can
/// keep the "select by id, ignore display label" pattern that the deprecated
/// `ComboBoxText` used to provide.
pub(crate) struct IdDropDown {
    pub widget: gtk::DropDown,
    ids: Rc<RefCell<Vec<String>>>,
    model: gtk::StringList,
}

impl IdDropDown {
    pub fn new() -> Self {
        let model = gtk::StringList::new(&[]);
        let widget = gtk::DropDown::new(Some(model.clone()), gtk::Expression::NONE);
        Self {
            widget,
            ids: Rc::new(RefCell::new(Vec::new())),
            model,
        }
    }

    pub fn clear(&self) {
        while self.model.n_items() > 0 {
            self.model.remove(0);
        }
        self.ids.borrow_mut().clear();
    }

    /// `id` is the stable key used to set/read selection; `label` is what the
    /// user sees in the menu.
    pub fn append(&self, id: &str, label: &str) {
        self.model.append(label);
        self.ids.borrow_mut().push(id.to_string());
    }

    pub fn set_active_id(&self, id: &str) {
        if let Some(idx) = self.ids.borrow().iter().position(|x| x == id) {
            self.widget.set_selected(idx as u32);
        }
    }

    pub fn active_id(&self) -> Option<String> {
        let idx = self.widget.selected();
        if idx == gtk::INVALID_LIST_POSITION {
            return None;
        }
        self.ids.borrow().get(idx as usize).cloned()
    }

    pub fn connect_changed<F>(&self, callback: F) -> glib::SignalHandlerId
    where
        F: Fn(&IdDropDown) + 'static,
    {
        let ids = Rc::clone(&self.ids);
        let widget = self.widget.clone();
        let model = self.model.clone();
        self.widget.connect_selected_notify(move |_| {
            let proxy = IdDropDown {
                widget: widget.clone(),
                ids: Rc::clone(&ids),
                model: model.clone(),
            };
            callback(&proxy);
        })
    }
}

impl Clone for IdDropDown {
    fn clone(&self) -> Self {
        Self {
            widget: self.widget.clone(),
            ids: Rc::clone(&self.ids),
            model: self.model.clone(),
        }
    }
}

pub fn run(args: Vec<String>) {
    let application = Application::builder().application_id(APP_ID).build();
    application.connect_activate(|app| {
        if let Err(err) = build_main_window(app) {
            eprintln!("Failed to start K-Pop MV Lyrics: {err}");
        }
    });
    application.run_with_args(&args);
}

pub(super) struct BackgroundUpdate {
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
    duration_ms: Option<u64>,
    volume: f64,
    /// Playback speed as a multiplier (1.0 = normal). Reset to 1.0 on each load.
    playback_rate: f64,
    active_index: usize,
    show_original: bool,
    show_romanization: bool,
    show_english: bool,
    asr_model_size: AsrModelSize,
    asr_demucs_enabled: bool,
    theme_preference: ThemePreference,
    busy: Option<String>,
    message: Option<String>,
    error: Option<String>,
    pending_stream: Option<StreamSpec>,
    pending_seek_ms: Option<u64>,
    active_seek_ms: Option<u64>,
    pending_autoplay: bool,
    timeline_spectrogram: Option<AudioSpectrogram>,
    pending_spectrogram_video_id: Option<String>,
    timeline_demucs_spectrogram: Option<AudioSpectrogram>,
    pending_demucs_spectrogram_video_id: Option<String>,
    open_progress: Option<f64>,
    editor_table_dirty: bool,
}

struct MemberButton {
    stage_name: String,
    color: String,
    border_wrap: GtkBox,
    portrait_frame: GtkBox,
    name_label: Label,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct MemberHighlightState {
    primary: Vec<String>,
    backing: Vec<String>,
}

struct MemberStage {
    content_key: String,
    buttons: Vec<MemberButton>,
    last_highlight: RefCell<Option<MemberHighlightState>>,
}

impl MemberStage {
    fn new() -> Self {
        Self {
            content_key: String::new(),
            buttons: Vec::new(),
            last_highlight: RefCell::new(None),
        }
    }

    fn set_member_highlight(&self, highlight: &crate::lyrics::MemberHighlight) {
        let mut primary: Vec<String> = highlight
            .primary
            .iter()
            .map(|name| name.to_lowercase())
            .collect();
        primary.sort();
        primary.dedup();
        let mut backing: Vec<String> = highlight
            .backing
            .iter()
            .map(|name| name.to_lowercase())
            .collect();
        backing.sort();
        backing.dedup();
        let state = MemberHighlightState {
            primary: primary.clone(),
            backing: backing.clone(),
        };
        if *self.last_highlight.borrow() == Some(state.clone()) {
            return;
        }
        *self.last_highlight.borrow_mut() = Some(state);
        for entry in &self.buttons {
            let stage = entry.stage_name.to_lowercase();
            let visual = if primary.iter().any(|name| name.eq_ignore_ascii_case(&stage)) {
                MemberVisualState::Primary
            } else if backing.iter().any(|name| name.eq_ignore_ascii_case(&stage)) {
                MemberVisualState::Backing
            } else {
                MemberVisualState::Inactive
            };
            apply_member_visual(entry, visual);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MemberVisualState {
    Primary,
    Backing,
    Inactive,
}

fn apply_member_visual(entry: &MemberButton, state: MemberVisualState) {
    entry.border_wrap.remove_css_class("active");
    entry.border_wrap.remove_css_class("backing");
    match state {
        MemberVisualState::Primary => {
            entry.border_wrap.add_css_class("active");
            entry.portrait_frame.set_opacity(1.0);
            entry.name_label.set_opacity(1.0);
            apply_member_name_style(&entry.name_label, &entry.color, true);
        }
        MemberVisualState::Backing => {
            entry.border_wrap.add_css_class("backing");
            entry.portrait_frame.set_opacity(0.72);
            entry.name_label.set_opacity(0.78);
            apply_member_name_style(&entry.name_label, &entry.color, true);
        }
        MemberVisualState::Inactive => {
            entry.portrait_frame.set_opacity(0.42);
            entry.name_label.set_opacity(0.55);
            apply_member_name_style(&entry.name_label, &entry.color, false);
        }
    }
}

pub(super) struct UiView {
    this: Rc<RefCell<Option<Rc<UiView>>>>,
    model: Rc<RefCell<UiModel>>,
    window: ApplicationWindow,
    url_entry: Entry,
    url_progress_provider: Rc<CssProvider>,
    member_card_css_provider: Rc<CssProvider>,
    status_label: Label,
    clock_label: Label,
    lyric_box: GtkBox,
    member_box: GtkBox,
    main_stack: Stack,
    quality_combo: IdDropDown,
    speed_spin: SpinButton,
    asr_model_combo: IdDropDown,
    theme_combo: IdDropDown,
    query_entry: Entry,
    original_toggle: CheckButton,
    roman_toggle: CheckButton,
    english_toggle: CheckButton,
    editor: EditorWidgets,
    metadata: Rc<MetadataPanel>,
    lyric_stage: Rc<RefCell<LyricStage>>,
    member_stage: Rc<RefCell<MemberStage>>,
    member_render_key: Rc<RefCell<String>>,
    lyric_build_key: Rc<RefCell<String>>,
    format_render_key: Rc<RefCell<String>>,
    suppress_quality_reload: Rc<Cell<bool>>,
    suppress_asr_model_reload: Rc<Cell<bool>>,
    suppress_theme_reload: Rc<Cell<bool>>,
    suppress_rate_reload: Rc<Cell<bool>>,
    video_overlay: Rc<VideoOverlay>,
    member_image_cache: Rc<RefCell<HashMap<String, String>>>,
    member_image_pending: Rc<RefCell<HashSet<String>>>,
    member_image_tx: std::sync::mpsc::Sender<(String, Option<String>)>,
    lyric_build_tx: std::sync::mpsc::Sender<(String, LyricStageContent)>,
}

fn build_main_window(app: &Application) -> Result<(), String> {
    load_stage_css();
    let ctx = Arc::new(AppContext::open()?);
    let initial_asr_model = ctx.asr_model_size();
    let initial_asr_provider = initial_asr_model.provider_id();
    let initial_asr_demucs_enabled = ctx.asr_demucs_enabled();
    let initial_theme = ThemePreference::from_storage(&ctx.theme_preference());

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
        duration_ms: None,
        volume: 1.0,
        playback_rate: 1.0,
        active_index: 0,
        show_original: true,
        show_romanization: false,
        show_english: true,
        asr_model_size: initial_asr_model,
        asr_demucs_enabled: initial_asr_demucs_enabled,
        theme_preference: initial_theme,
        busy: None,
        message: None,
        error: None,
        pending_stream: None,
        pending_seek_ms: None,
        active_seek_ms: None,
        pending_autoplay: false,
        timeline_spectrogram: None,
        pending_spectrogram_video_id: None,
        timeline_demucs_spectrogram: None,
        pending_demucs_spectrogram_video_id: None,
        open_progress: None,
        editor_table_dirty: false,
    }));

    let window = ApplicationWindow::builder()
        .application(app)
        .title("K-Pop MV Lyrics")
        .default_width(1280)
        .default_height(920)
        .build();
    apply_theme_preference(&window, initial_theme);

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

    let url_progress_provider = Rc::new(CssProvider::new());
    let member_card_css_provider = Rc::new(CssProvider::new());
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &*url_progress_provider,
            gtk::STYLE_PROVIDER_PRIORITY_USER,
        );
        gtk::style_context_add_provider_for_display(
            &display,
            &*member_card_css_provider,
            gtk::STYLE_PROVIDER_PRIORITY_USER,
        );
    }

    let (open_progress_tx, open_progress_rx) = std::sync::mpsc::channel::<f64>();

    let quality_combo = IdDropDown::new();
    quality_combo.append("auto", "Auto");
    quality_combo.set_active_id("auto");

    let open_button = Button::with_label("Open");
    let stream_button = Button::with_label("Stream");

    // Playback speed selector (percentage), placed between quality and Open.
    let speed_spin = SpinButton::with_range(25.0, 300.0, 5.0);
    speed_spin.set_value(100.0);
    speed_spin.set_climb_rate(5.0);
    speed_spin.set_tooltip_text(Some("Playback speed (%)"));
    let speed_box = GtkBox::new(Orientation::Horizontal, 4);
    let speed_label = Label::new(Some("Speed"));
    speed_label.add_css_class("toolbar-label");
    speed_box.append(&speed_label);
    speed_box.append(&speed_spin);
    let speed_percent = Label::new(Some("%"));
    speed_box.append(&speed_percent);

    let toolbar = GtkBox::new(Orientation::Horizontal, 6);
    toolbar.append(&url_entry);
    toolbar.append(&quality_combo.widget);
    toolbar.append(&speed_box);
    toolbar.append(&open_button);

    let member_scroll = ScrolledWindow::new();
    member_scroll.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Never);
    member_scroll.set_hexpand(true);
    member_scroll.set_min_content_height(MEMBER_STRIP_HEIGHT);
    member_scroll.add_css_class("member-strip");
    let member_box = GtkBox::new(Orientation::Horizontal, 10);
    member_box.set_homogeneous(true);
    member_box.set_margin_start(6);
    member_box.set_margin_end(6);
    member_scroll.set_child(Some(&member_box));

    let lang_box = GtkBox::new(Orientation::Horizontal, 6);
    lang_box.set_margin_start(8);
    lang_box.set_margin_end(8);
    lang_box.set_margin_top(6);
    lang_box.set_margin_bottom(4);
    let original_toggle = CheckButton::with_label("Original");
    original_toggle.set_active(true);
    let roman_toggle = CheckButton::with_label("Roman");
    let english_toggle = CheckButton::with_label("English");
    english_toggle.set_active(true);
    lang_box.append(&original_toggle);
    lang_box.append(&roman_toggle);
    lang_box.append(&english_toggle);

    let clock_label = Label::new(None);
    clock_label.set_halign(gtk::Align::End);
    clock_label.set_valign(gtk::Align::Center);
    clock_label.set_margin_end(8);
    clock_label.set_markup("<span size='large'><b>0:00.000</b></span>");

    let stage_toolbar = GtkBox::new(Orientation::Horizontal, 8);
    stage_toolbar.append(&lang_box);
    let toolbar_spacer = GtkBox::new(Orientation::Horizontal, 0);
    toolbar_spacer.set_hexpand(true);
    stage_toolbar.append(&toolbar_spacer);
    stage_toolbar.append(&clock_label);

    let lyric_box = GtkBox::new(Orientation::Vertical, 4);
    lyric_box.set_valign(gtk::Align::Start);
    lyric_box.set_margin_start(8);
    lyric_box.set_margin_end(8);
    lyric_box.set_margin_bottom(8);

    let lyric_frame = Frame::new(None);
    lyric_frame.add_css_class("lyric-stage-panel");
    lyric_frame.set_vexpand(true);
    lyric_frame.set_child(Some(&lyric_box));

    let play_button = Button::with_label("Start Sync");
    let pause_button = Button::with_label("Pause Sync");
    let reset_button = Button::with_label("Reset Sync");

    let status_label = Label::new(None);
    status_label.set_halign(gtk::Align::Start);
    status_label.set_xalign(0.0);
    status_label.set_wrap(true);

    let query_entry = Entry::new();
    query_entry.set_placeholder_text(Some("Artist and song title"));
    let fetch_lyrics_button = Button::with_label("Fetch Lyrics");
    let fetch_captions_button = Button::with_label("Fetch Captions");
    let align_button = Button::with_label("Align");
    let save_button = Button::with_label("Save");
    let settings_panel = GtkBox::new(Orientation::Vertical, 6);
    settings_panel.append(&query_entry);

    let asr_model_row = GtkBox::new(Orientation::Horizontal, 8);
    asr_model_row.append(&Label::new(Some("ASR model")));
    let asr_model_combo = IdDropDown::new();
    asr_model_combo.append(
        AsrModelSize::Disabled.as_storage(),
        AsrModelSize::Disabled.label(),
    );
    asr_model_combo.append(
        AsrModelSize::Small.as_storage(),
        AsrModelSize::Small.label(),
    );
    asr_model_combo.append(
        AsrModelSize::Large.as_storage(),
        AsrModelSize::Large.label(),
    );
    asr_model_combo.append(
        AsrModelSize::OpenAiGpt4oTranscribe.as_storage(),
        AsrModelSize::OpenAiGpt4oTranscribe.label(),
    );
    asr_model_combo.append(
        AsrModelSize::OpenAiWhisper1.as_storage(),
        AsrModelSize::OpenAiWhisper1.label(),
    );
    asr_model_combo.append(
        AsrModelSize::ElevenLabsScribeV2.as_storage(),
        AsrModelSize::ElevenLabsScribeV2.label(),
    );
    asr_model_combo.append(
        AsrModelSize::MistralVoxtralMini.as_storage(),
        AsrModelSize::MistralVoxtralMini.label(),
    );
    asr_model_combo.append(
        AsrModelSize::GeminiFlash.as_storage(),
        AsrModelSize::GeminiFlash.label(),
    );
    asr_model_combo.append(
        AsrModelSize::SonioxAsyncV4.as_storage(),
        AsrModelSize::SonioxAsyncV4.label(),
    );
    asr_model_combo.append(
        AsrModelSize::AlibabaQwenFlash.as_storage(),
        AsrModelSize::AlibabaQwenFlash.label(),
    );
    asr_model_combo.set_active_id(initial_asr_model.as_storage());
    asr_model_row.append(&asr_model_combo.widget);

    let asr_demucs_toggle = CheckButton::with_label("Demucs vocals");
    asr_demucs_toggle.set_active(initial_asr_demucs_enabled);

    let theme_row = GtkBox::new(Orientation::Horizontal, 8);
    theme_row.append(&Label::new(Some("Theme")));
    let theme_combo = IdDropDown::new();
    for theme in [
        ThemePreference::System,
        ThemePreference::Light,
        ThemePreference::Dark,
    ] {
        theme_combo.append(theme.as_storage(), theme.label());
    }
    theme_combo.set_active_id(initial_theme.as_storage());
    theme_row.append(&theme_combo.widget);

    let asr_api_key_row = GtkBox::new(Orientation::Horizontal, 8);
    asr_api_key_row.append(&Label::new(Some("ASR API key")));
    let asr_api_key_entry = Entry::new();
    asr_api_key_entry.set_hexpand(true);
    asr_api_key_entry.set_visibility(false);
    asr_api_key_entry.set_placeholder_text(Some("Saved for the selected external provider"));
    if let Some(provider) = initial_asr_provider {
        asr_api_key_entry.set_text(&model.borrow().ctx.asr_api_key(provider));
    }
    asr_api_key_row.append(&asr_api_key_entry);

    let asr_base_url_row = GtkBox::new(Orientation::Horizontal, 8);
    asr_base_url_row.append(&Label::new(Some("ASR API URL")));
    let asr_base_url_entry = Entry::new();
    asr_base_url_entry.set_hexpand(true);
    asr_base_url_entry.set_placeholder_text(Some("Optional endpoint/region override"));
    if let Some(provider) = initial_asr_provider {
        asr_base_url_entry.set_text(&model.borrow().ctx.asr_base_url(provider));
    }
    asr_base_url_row.append(&asr_base_url_entry);

    // All settings controls share a FlowBox so they collapse onto a single line
    // when the pane is wide enough and wrap to additional lines only when space
    // runs out, instead of being permanently stacked in fixed rows.
    let controls_flow = gtk::FlowBox::new();
    controls_flow.set_orientation(Orientation::Horizontal);
    controls_flow.set_selection_mode(gtk::SelectionMode::None);
    controls_flow.set_min_children_per_line(1);
    controls_flow.set_max_children_per_line(32);
    controls_flow.set_column_spacing(6);
    controls_flow.set_row_spacing(6);
    controls_flow.set_homogeneous(false);
    controls_flow.set_halign(gtk::Align::Start);
    controls_flow.append(&theme_row);
    controls_flow.append(&asr_model_row);
    controls_flow.append(&asr_demucs_toggle);
    controls_flow.append(&fetch_lyrics_button);
    controls_flow.append(&fetch_captions_button);
    controls_flow.append(&align_button);
    controls_flow.append(&save_button);
    controls_flow.append(&stream_button);
    controls_flow.append(&play_button);
    controls_flow.append(&pause_button);
    controls_flow.append(&reset_button);
    settings_panel.append(&controls_flow);
    settings_panel.append(&asr_api_key_row);
    settings_panel.append(&asr_base_url_row);
    settings_panel.append(&status_label);

    let editor_build = build_editor_panel();
    editor_build.panel.set_vexpand(true);

    let metadata_panel = MetadataPanel::new();

    // Settings is now its own tab; let long content scroll instead of stretching
    // the pane.
    let settings_scroll = ScrolledWindow::new();
    settings_scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    settings_scroll.set_vexpand(true);
    settings_panel.set_margin_top(6);
    settings_scroll.set_child(Some(&settings_panel));

    // Playback page: member portraits + language/clock toolbar + lyrics stage.
    let playback_page = GtkBox::new(Orientation::Vertical, 8);
    playback_page.append(&member_scroll);
    playback_page.append(&stage_toolbar);
    playback_page.append(&lyric_frame);

    // Tabs: "Playback", "Editor" and "Settings" replace each other in this stack,
    // so each takes over the content area instead of stacking.
    let main_stack = Stack::new();
    main_stack.set_vexpand(true);
    main_stack.set_margin_start(10);
    main_stack.set_margin_end(10);
    main_stack.add_titled(&playback_page, Some(PLAYBACK_PAGE), "Playback");
    main_stack.add_titled(&editor_build.panel, Some(EDITOR_PAGE), "Editor");
    main_stack.add_titled(&metadata_panel.root, Some(METADATA_PAGE), "Metadata");
    main_stack.add_titled(&settings_scroll, Some(SETTINGS_PAGE), "Settings");

    let stack_switcher = StackSwitcher::new();
    stack_switcher.set_stack(Some(&main_stack));
    stack_switcher.set_halign(gtk::Align::Center);

    // Header stays pinned at the very top: the URL bar + controls and the tab
    // switcher never move when the content/video split is dragged.
    top.append(&toolbar);
    top.append(&stack_switcher);

    let video_box = player.borrow().video_widget().clone();
    video_box.set_vexpand(true);
    let video_overlay = Rc::new(build_video_overlay(video_box.upcast_ref()));

    let video_pane = GtkBox::new(Orientation::Vertical, 0);
    video_overlay.overlay.set_hexpand(true);
    video_overlay.overlay.set_vexpand(true);
    video_pane.append(&video_overlay.overlay);

    // The resizable split sits below the pinned header: content (tabs) on top,
    // video on the bottom.
    paned.set_start_child(Some(&main_stack));
    paned.set_end_child(Some(&video_pane));
    paned.set_position(420);
    paned.set_vexpand(true);

    let root = GtkBox::new(Orientation::Vertical, 0);
    root.append(&top);
    root.append(&paned);
    window.set_child(Some(&root));

    let (work_tx, work_rx) = std::sync::mpsc::channel::<BackgroundUpdate>();
    let (member_image_tx, member_image_rx) = std::sync::mpsc::channel::<(String, Option<String>)>();
    let (lyric_build_tx, lyric_build_rx) =
        std::sync::mpsc::channel::<(String, LyricStageContent)>();

    let view = Rc::new(UiView {
        this: Rc::new(RefCell::new(None)),
        model: Rc::clone(&model),
        window: window.clone(),
        url_entry: url_entry.clone(),
        url_progress_provider: Rc::clone(&url_progress_provider),
        member_card_css_provider: Rc::clone(&member_card_css_provider),
        status_label: status_label.clone(),
        clock_label: clock_label.clone(),
        lyric_box: lyric_box.clone(),
        member_box: member_box.clone(),
        main_stack: main_stack.clone(),
        quality_combo: quality_combo.clone(),
        speed_spin: speed_spin.clone(),
        asr_model_combo: asr_model_combo.clone(),
        theme_combo: theme_combo.clone(),
        // NB: IdDropDown::clone copies the same underlying StringList + ids so
        // updates from anywhere reflect everywhere - it's a cheap Rc-style clone.
        query_entry: query_entry.clone(),
        original_toggle: original_toggle.clone(),
        roman_toggle: roman_toggle.clone(),
        english_toggle: english_toggle.clone(),
        editor: EditorWidgets {
            timeline: Rc::clone(&editor_build.widgets.timeline),
            render_key: Rc::new(RefCell::new(String::new())),
        },
        metadata: Rc::clone(&metadata_panel),
        lyric_stage: Rc::new(RefCell::new(LyricStage::new())),
        member_stage: Rc::new(RefCell::new(MemberStage::new())),
        member_render_key: Rc::new(RefCell::new(String::new())),
        lyric_build_key: Rc::new(RefCell::new(String::new())),
        format_render_key: Rc::new(RefCell::new(String::new())),
        suppress_quality_reload: Rc::new(Cell::new(false)),
        suppress_asr_model_reload: Rc::new(Cell::new(false)),
        suppress_theme_reload: Rc::new(Cell::new(false)),
        suppress_rate_reload: Rc::new(Cell::new(false)),
        video_overlay: Rc::clone(&video_overlay),
        member_image_cache: Rc::new(RefCell::new(HashMap::new())),
        member_image_pending: Rc::new(RefCell::new(HashSet::new())),
        member_image_tx,
        lyric_build_tx,
    });
    *view.this.borrow_mut() = Some(Rc::clone(&view));

    let work_tx_for_tick = work_tx.clone();
    let view_for_tick = Rc::clone(&view);
    glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
        if catch_unwind(AssertUnwindSafe(|| {
            let mut needs_full_refresh = false;
            while let Ok(position) = position_rx.try_recv() {
                if let Ok(mut model) = view_for_tick.model.try_borrow_mut() {
                    model.apply_position(
                        position.ms as i64,
                        position.duration_ms,
                        position.playing,
                        position.buffering,
                    );
                }
            }
            while let Ok(message) = error_rx.try_recv() {
                if let Ok(mut model) = view_for_tick.model.try_borrow_mut() {
                    model.error = Some(message);
                }
            }
            while let Ok(progress) = open_progress_rx.try_recv() {
                if let Ok(mut model) = view_for_tick.model.try_borrow_mut() {
                    model.open_progress = Some(progress);
                }
                apply_url_entry_progress(&view_for_tick, Some(progress));
            }
            while let Ok((url, path)) = member_image_rx.try_recv() {
                view_for_tick.member_image_pending.borrow_mut().remove(&url);
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
                let is_open = update.label == "Open";
                let is_background_members = update.label == "Members";
                if !is_background_members {
                    if let Ok(mut model) = view_for_tick.model.try_borrow_mut() {
                        model.set_busy(None);
                    }
                }
                match update.result {
                    Ok(apply) => {
                        if let Ok(mut model) = view_for_tick.model.try_borrow_mut() {
                            apply(&mut model);
                            if update.label != "Alignment"
                                && !is_background_members
                                && update.label != "Open"
                                && update.label != "Spectrogram"
                                && update.label != "Vocals spectrogram"
                            {
                                model.message = Some(format!("{} complete", update.label));
                            }
                            if is_open {
                                model.open_progress = Some(0.96);
                            }
                        }
                        if is_open {
                            apply_url_entry_progress(&view_for_tick, Some(0.96));
                            spawn_timeline_spectrogram(
                                work_tx_for_tick.clone(),
                                Rc::clone(&view_for_tick),
                            );
                            spawn_timeline_demucs_spectrogram(
                                work_tx_for_tick.clone(),
                                Rc::clone(&view_for_tick),
                            );
                            let group = view_for_tick.model.try_borrow().ok().and_then(|model| {
                                model
                                    .song
                                    .as_ref()
                                    .and_then(|song| song.song.group_name.clone())
                            });
                            if let Some(group) = group {
                                spawn_member_profiles_in_background(
                                    work_tx_for_tick.clone(),
                                    Rc::clone(&view_for_tick),
                                    group,
                                );
                            }
                        }
                        if is_background_members {
                            *view_for_tick.member_render_key.borrow_mut() = String::new();
                        }
                        needs_full_refresh = true;
                    }
                    Err(err) => {
                        if !is_background_members {
                            if let Ok(mut model) = view_for_tick.model.try_borrow_mut() {
                                model.error = Some(err);
                                if is_open {
                                    model.open_progress = None;
                                }
                            }
                            if is_open {
                                apply_url_entry_progress(&view_for_tick, None);
                            }
                            needs_full_refresh = true;
                        }
                    }
                }
                let pending_spec = view_for_tick
                    .model
                    .try_borrow_mut()
                    .ok()
                    .and_then(|mut model| model.pending_stream.take());
                if let Some(spec) = pending_spec {
                    let view = Rc::clone(&view_for_tick);
                    glib::idle_add_local_once(move || {
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
        glib::ControlFlow::Continue
    });

    connect_view_handlers(
        &view,
        work_tx.clone(),
        open_progress_tx,
        &url_entry,
        &quality_combo,
        &asr_model_combo,
        &theme_combo,
        &asr_demucs_toggle,
        &asr_api_key_entry,
        &asr_base_url_entry,
        &open_button,
        &stream_button,
        &speed_spin,
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

    video_overlay.connect_handlers(&view);

    metadata_panel.connect(&view, &window, work_tx.clone());

    connect_editor_handlers(&view, &window, work_tx, &editor_build, &main_stack);

    {
        let view = Rc::clone(&view);
        let key_controller = EventControllerKey::new();
        let window_for_key = window.clone();
        key_controller.connect_key_pressed(move |_, keyval, _keycode, _state| {
            if keyval != gdk::Key::space {
                return glib::Propagation::Proceed;
            }
            if focus_is_text_widget(&window_for_key) {
                return glib::Propagation::Proceed;
            }
            spawn_toggle_play_pause(Rc::clone(&view));
            glib::Propagation::Stop
        });
        window.add_controller(key_controller);
    }

    view.refresh();
    window.present();
    Ok(())
}

impl UiModel {
    fn apply_position(
        &mut self,
        current_ms: i64,
        duration_ms: Option<u64>,
        playing: bool,
        buffering: bool,
    ) {
        if let Some(target_ms) = self.active_seek_ms {
            let observed_ms = current_ms.max(0) as u64;
            self.duration_ms = duration_ms.or(self.duration_ms);
            if observed_ms.abs_diff(target_ms) > 1_500 {
                self.message = if buffering {
                    Some("Buffering video".to_string())
                } else {
                    Some("Seeking video".to_string())
                };
                return;
            }
            self.active_seek_ms = None;
        }
        self.current_ms = current_ms;
        if duration_ms.is_some() {
            self.duration_ms = duration_ms;
        }
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

    fn begin_seek(&mut self, ms: u64) {
        self.current_ms = ms as i64;
        self.active_index = active_lyric_index(&self.alignment, ms as i64);
        self.active_seek_ms = Some(ms);
        self.message = Some("Seeking video".to_string());
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
        self.sync_language_toggles(&model);
        self.sync_asr_model_combo(&model);
        self.sync_theme_combo(&model);
        self.sync_speed_spin(&model);
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
        self.video_overlay
            .update_seek_bar(model.current_ms, model.duration_ms);
        if self.editor_visible() {
            self.editor.timeline.set_playhead(model.current_ms);
        }
        self.sync_active_line(&model);
    }

    fn sync_active_line(&self, model: &UiModel) {
        self.lyric_stage.borrow().set_active(model.active_index);
        let highlight = model
            .song
            .as_ref()
            .and_then(|song| {
                song.lines
                    .iter()
                    .find(|line| line.index == model.active_index)
                    .map(|line| crate::lyrics::member_highlight_for_line(line, &song.members))
            })
            .unwrap_or(crate::lyrics::MemberHighlight {
                primary: Vec::new(),
                backing: Vec::new(),
            });
        self.member_stage.borrow().set_member_highlight(&highlight);
    }

    fn sync_language_toggles(&self, model: &UiModel) {
        if self.original_toggle.is_active() != model.show_original {
            self.original_toggle.set_active(model.show_original);
        }
        if self.roman_toggle.is_active() != model.show_romanization {
            self.roman_toggle.set_active(model.show_romanization);
        }
        if self.english_toggle.is_active() != model.show_english {
            self.english_toggle.set_active(model.show_english);
        }
    }

    fn sync_speed_spin(&self, model: &UiModel) {
        if self.suppress_rate_reload.get() {
            return;
        }
        let target = model.playback_rate * 100.0;
        if (self.speed_spin.value() - target).abs() > 0.5 {
            self.suppress_rate_reload.set(true);
            self.speed_spin.set_value(target);
            self.suppress_rate_reload.set(false);
        }
    }

    fn sync_asr_model_combo(&self, model: &UiModel) {
        if self.suppress_asr_model_reload.get() {
            return;
        }
        let target = model.asr_model_size.as_storage();
        if self.asr_model_combo.active_id().as_deref() != Some(target) {
            self.suppress_asr_model_reload.set(true);
            self.asr_model_combo.set_active_id(target);
            self.suppress_asr_model_reload.set(false);
        }
    }

    fn sync_theme_combo(&self, model: &UiModel) {
        if self.suppress_theme_reload.get() {
            return;
        }
        let target = model.theme_preference.as_storage();
        if self.theme_combo.active_id().as_deref() != Some(target) {
            self.suppress_theme_reload.set(true);
            self.theme_combo.set_active_id(target);
            self.suppress_theme_reload.set(false);
        }
    }

    fn apply_song_language_toggles(model: &mut UiModel, lines: &[crate::models::LyricLine]) {
        let (show_original, show_romanization, show_english) =
            crate::lyrics::lyric_language_toggles(lines);
        model.show_original = show_original;
        model.show_romanization = show_romanization;
        model.show_english = show_english;
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
        let show_original = model.show_original;
        let show_romanization = model.show_romanization;
        let show_english = model.show_english;
        let tx = self.lyric_build_tx.clone();
        std::thread::spawn(move || {
            let content =
                compute_lyric_stage_content(song, show_original, show_romanization, show_english);
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
    open_progress_tx: std::sync::mpsc::Sender<f64>,
    url_entry: &Entry,
    quality_combo: &IdDropDown,
    asr_model_combo: &IdDropDown,
    theme_combo: &IdDropDown,
    asr_demucs_toggle: &CheckButton,
    asr_api_key_entry: &Entry,
    asr_base_url_entry: &Entry,
    open_button: &Button,
    stream_button: &Button,
    speed_spin: &SpinButton,
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
                model.timeline_spectrogram = None;
                model.pending_spectrogram_video_id = None;
                model.timeline_demucs_spectrogram = None;
                model.pending_demucs_spectrogram_video_id = None;
                model.playback_rate = 1.0;
            });
            let view = Rc::clone(&view);
            spawn_open_work(work_tx.clone(), open_progress_tx.clone(), view);
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
                let spec = snapshot
                    .ctx
                    .resolve_stream(&snapshot.url, snapshot.selected_format.as_deref())?;
                Ok(Box::new(move |model: &mut UiModel| {
                    model.pending_stream = Some(spec);
                }) as Box<dyn FnOnce(&mut UiModel) + Send>)
            });
        });
    }

    {
        let view = Rc::clone(view);
        let suppress = Rc::clone(&view.suppress_theme_reload);
        let window = view.window.clone();
        theme_combo.connect_changed(move |combo| {
            if suppress.get() {
                return;
            }
            let Some(id) = combo.active_id() else {
                return;
            };
            let theme = ThemePreference::from_storage(&id);
            apply_theme_preference(&window, theme);
            view.refresh_mut(|model| {
                model.theme_preference = theme;
                if let Err(err) = model.ctx.set_theme_preference(theme.as_storage()) {
                    model.error = Some(err);
                } else {
                    model.message = Some(format!("Theme set to {}", theme.label()));
                }
            });
        });
    }

    {
        let view = Rc::clone(view);
        let suppress = Rc::clone(&view.suppress_asr_model_reload);
        let asr_api_key_entry = asr_api_key_entry.clone();
        let asr_base_url_entry = asr_base_url_entry.clone();
        asr_model_combo.connect_changed(move |combo| {
            if suppress.get() {
                return;
            }
            let Some(id) = combo.active_id() else {
                return;
            };
            let size = AsrModelSize::from_storage(&id);
            view.refresh_mut(|model| {
                model.asr_model_size = size;
                if let Err(err) = model.ctx.set_asr_model_size(size) {
                    model.error = Some(err);
                } else {
                    if let Some(provider) = size.provider_id() {
                        asr_api_key_entry.set_text(&model.ctx.asr_api_key(provider));
                        asr_base_url_entry.set_text(&model.ctx.asr_base_url(provider));
                    } else {
                        asr_api_key_entry.set_text("");
                        asr_base_url_entry.set_text("");
                    }
                    model.message = Some(format!("ASR model set to {}", size.label()));
                }
            });
        });
    }

    {
        let view = Rc::clone(view);
        asr_demucs_toggle.connect_toggled(move |toggle| {
            let enabled = toggle.is_active();
            view.refresh_mut(|model| {
                model.asr_demucs_enabled = enabled;
                if let Err(err) = model.ctx.set_asr_demucs_enabled(enabled) {
                    model.error = Some(err);
                } else {
                    model.message = Some(if enabled {
                        "Demucs vocal separation enabled".to_string()
                    } else {
                        "Demucs vocal separation disabled".to_string()
                    });
                }
            });
        });
    }

    {
        let view = Rc::clone(view);
        asr_api_key_entry.connect_changed(move |entry| {
            let value = entry.text().to_string();
            view.refresh_mut(|model| {
                let Some(provider) = model.asr_model_size.provider_id() else {
                    return;
                };
                if let Err(err) = model.ctx.set_asr_api_key(provider, &value) {
                    model.error = Some(err);
                } else {
                    model.message =
                        Some(format!("Saved {} API key", model.asr_model_size.backend()));
                }
            });
        });
    }

    {
        let view = Rc::clone(view);
        asr_base_url_entry.connect_changed(move |entry| {
            let value = entry.text().to_string();
            view.refresh_mut(|model| {
                let Some(provider) = model.asr_model_size.provider_id() else {
                    return;
                };
                if let Err(err) = model.ctx.set_asr_base_url(provider, &value) {
                    model.error = Some(err);
                } else {
                    model.message =
                        Some(format!("Saved {} API URL", model.asr_model_size.backend()));
                }
            });
        });
    }

    {
        let view = Rc::clone(view);
        let suppress = Rc::clone(&view.suppress_rate_reload);
        speed_spin.connect_value_changed(move |spin| {
            if suppress.get() {
                return;
            }
            let rate = (spin.value() / 100.0).clamp(0.1, 4.0);
            if let Ok(mut model) = view.model.try_borrow_mut() {
                model.playback_rate = rate;
            }
            spawn_player_work(Rc::clone(&view), move |player| player.set_rate(rate));
        });
    }

    {
        let view = Rc::clone(view);
        let url_entry = url_entry.clone();
        let quality_combo_for_handler = quality_combo.clone();
        let work_tx = work_tx.clone();
        let suppress = view.suppress_quality_reload.clone();
        quality_combo.connect_changed(move |combo| {
            if suppress.get() {
                return;
            }
            let format_id = selected_format_id(combo);
            view.refresh_mut(|model| {
                model.selected_format = format_id;
            });
            let has_video = view
                .model
                .try_borrow()
                .ok()
                .is_some_and(|model| model.metadata.is_some());
            if has_video {
                spawn_stream_reload(
                    Rc::clone(&view),
                    work_tx.clone(),
                    url_entry.clone(),
                    quality_combo_for_handler.clone(),
                );
            }
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
            view.refresh_mut(|model| model.begin_seek(start_ms));
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
            view.refresh_mut(|model| model.begin_seek(0));
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
                        package.members =
                            apply_member_profiles(&package.members, &profiles, &package.lines);
                    }
                }
                Ok(Box::new(move |model: &mut UiModel| {
                    UiView::apply_song_language_toggles(model, &package.lines);
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
                    model.message = Some(result.summary);
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

fn spawn_open_work(
    work_tx: std::sync::mpsc::Sender<BackgroundUpdate>,
    progress_tx: std::sync::mpsc::Sender<f64>,
    view: Rc<UiView>,
) {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::time::Duration;

    use crate::log::{verbose, verbose_enabled};

    view.refresh_mut(|model| {
        model.set_busy(Some("Open"));
        model.open_progress = Some(0.0);
    });
    apply_url_entry_progress(&view, Some(0.0));

    let snapshot = view.model.borrow().clone_for_thread();
    let open_done = Arc::new(AtomicBool::new(false));
    let last_progress = Arc::new(AtomicU64::new(0));

    if verbose_enabled() {
        let done = Arc::clone(&open_done);
        let last = Arc::clone(&last_progress);
        std::thread::spawn(move || {
            while !done.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_secs(15));
                if done.load(Ordering::Relaxed) {
                    break;
                }
                let fraction = f64::from_bits(last.load(Ordering::Relaxed));
                if fraction > 0.0 {
                    verbose(format!(
                        "open heartbeat still running at progress {fraction:.2}"
                    ));
                } else {
                    verbose("open heartbeat still running (no progress yet)");
                }
            }
        });
    }

    std::thread::spawn(move || {
        let last = Arc::clone(&last_progress);
        let result = resolve_video_chain(snapshot, |progress| {
            last.store(progress.to_bits(), Ordering::Relaxed);
            let _ = progress_tx.send(progress);
        });
        open_done.store(true, Ordering::Relaxed);
        let _ = work_tx.send(BackgroundUpdate {
            label: "Open",
            result,
        });
    });
}

fn apply_url_entry_progress(view: &UiView, progress: Option<f64>) {
    let entry = &view.url_entry;

    let Some(fraction) = progress else {
        entry.remove_css_class("url-loading");
        entry.set_sensitive(true);
        view.url_progress_provider.load_from_string(
            "entry.url-loading { background-image: none; background-color: inherit; }",
        );
        return;
    };

    entry.add_css_class("url-loading");
    entry.set_sensitive(false);
    let pct = (fraction.clamp(0.0, 1.0) * 100.0).round();
    let css = format!(
        "entry.url-loading {{ \
            background-image: linear-gradient(to right, rgba(78, 148, 255, 0.38) {pct:.0}%, rgba(255, 255, 255, 0.96) {pct:.0}%); \
            background-color: #ffffff; \
        }}"
    );
    view.url_progress_provider.load_from_string(&css);
}

fn apply_theme_preference(window: &ApplicationWindow, theme: ThemePreference) {
    window.remove_css_class("kpml-theme-system");
    window.remove_css_class("kpml-theme-light");
    window.remove_css_class("kpml-theme-dark");
    window.add_css_class(match theme {
        ThemePreference::System => "kpml-theme-system",
        ThemePreference::Light => "kpml-theme-light",
        ThemePreference::Dark => "kpml-theme-dark",
    });

    if let Some(settings) = gtk::Settings::default() {
        settings.set_gtk_application_prefer_dark_theme(theme == ThemePreference::Dark);
    }
}

fn spawn_member_profiles_in_background(
    work_tx: std::sync::mpsc::Sender<BackgroundUpdate>,
    view: Rc<UiView>,
    group: String,
) {
    let snapshot = view.model.borrow().clone_for_thread();
    std::thread::spawn(move || {
        let result = (|| {
            let profiles = snapshot.ctx.search_member_profiles(&group)?;
            crate::log::verbose(format!(
                "members fetched group={group:?} profiles={} with_images={}",
                profiles.len(),
                profiles
                    .iter()
                    .filter(|profile| profile.image_url.is_some())
                    .count(),
            ));
            Ok(Box::new(move |model: &mut UiModel| {
                if let Some(song) = &mut model.song {
                    song.members = apply_member_profiles(&song.members, &profiles, &song.lines);
                    crate::log::verbose(format!(
                        "members applied count={} with_images={}",
                        song.members.len(),
                        song.members
                            .iter()
                            .filter(|member| member.image_url.is_some()
                                || member.local_image_path.is_some())
                            .count(),
                    ));
                    model.editor_table_dirty = true;
                }
            }) as Box<dyn FnOnce(&mut UiModel) + Send>)
        })();
        let _ = work_tx.send(BackgroundUpdate {
            label: "Members",
            result,
        });
    });
}

pub(super) fn spawn_timeline_spectrogram(
    work_tx: std::sync::mpsc::Sender<BackgroundUpdate>,
    view: Rc<UiView>,
) {
    let should_spawn = view.model.try_borrow_mut().ok().and_then(|mut model| {
        let metadata = model.metadata.clone()?;
        if metadata.original_url.trim().is_empty() {
            return None;
        }
        if model
            .timeline_spectrogram
            .as_ref()
            .is_some_and(|spectrogram| spectrogram.video_id == metadata.video_id)
        {
            return None;
        }
        if model.pending_spectrogram_video_id.as_deref() == Some(metadata.video_id.as_str()) {
            return None;
        }
        model.pending_spectrogram_video_id = Some(metadata.video_id.clone());
        Some(())
    });
    if should_spawn.is_none() {
        return;
    }

    let snapshot = view.model.borrow().clone_for_thread();
    std::thread::spawn(move || {
        let result = match snapshot.metadata.clone() {
            Some(metadata) => match snapshot
                .ctx
                .build_timeline_spectrogram(&metadata.video_id, &metadata.original_url)
            {
                Ok(spectrogram) => Ok(Box::new(move |model: &mut UiModel| {
                    if model
                        .metadata
                        .as_ref()
                        .is_some_and(|current| current.video_id == spectrogram.video_id)
                    {
                        model.timeline_spectrogram = Some(spectrogram);
                        model.editor_table_dirty = true;
                    }
                    model.pending_spectrogram_video_id = None;
                }) as Box<dyn FnOnce(&mut UiModel) + Send>),
                Err(err) => Ok(Box::new(move |model: &mut UiModel| {
                    model.pending_spectrogram_video_id = None;
                    model.error = Some(format!("Spectrogram failed: {err}"));
                }) as Box<dyn FnOnce(&mut UiModel) + Send>),
            },
            None => Ok(Box::new(|model: &mut UiModel| {
                model.pending_spectrogram_video_id = None;
            }) as Box<dyn FnOnce(&mut UiModel) + Send>),
        };
        let _ = work_tx.send(BackgroundUpdate {
            label: "Spectrogram",
            result,
        });
    });
}

/// Build the Demucs vocals spectrogram for the open video in the background.
///
/// Reuses a cached `vocals.wav` from a prior ASR-with-Demucs run when present;
/// otherwise this runs Demucs, which is heavy — so it runs separately from the
/// full-mix spectrogram and surfaces whenever it finishes.
pub(super) fn spawn_timeline_demucs_spectrogram(
    work_tx: std::sync::mpsc::Sender<BackgroundUpdate>,
    view: Rc<UiView>,
) {
    let should_spawn = view.model.try_borrow_mut().ok().and_then(|mut model| {
        let metadata = model.metadata.clone()?;
        if metadata.original_url.trim().is_empty() {
            return None;
        }
        if model
            .timeline_demucs_spectrogram
            .as_ref()
            .is_some_and(|spectrogram| spectrogram.video_id == metadata.video_id)
        {
            return None;
        }
        if model.pending_demucs_spectrogram_video_id.as_deref() == Some(metadata.video_id.as_str()) {
            return None;
        }
        model.pending_demucs_spectrogram_video_id = Some(metadata.video_id.clone());
        Some(())
    });
    if should_spawn.is_none() {
        return;
    }

    let snapshot = view.model.borrow().clone_for_thread();
    std::thread::spawn(move || {
        let result = match snapshot.metadata.clone() {
            Some(metadata) => match snapshot
                .ctx
                .build_timeline_demucs_spectrogram(&metadata.video_id, &metadata.original_url)
            {
                Ok(spectrogram) => Ok(Box::new(move |model: &mut UiModel| {
                    if model
                        .metadata
                        .as_ref()
                        .is_some_and(|current| current.video_id == spectrogram.video_id)
                    {
                        model.timeline_demucs_spectrogram = Some(spectrogram);
                        model.editor_table_dirty = true;
                    }
                    model.pending_demucs_spectrogram_video_id = None;
                }) as Box<dyn FnOnce(&mut UiModel) + Send>),
                Err(err) => Ok(Box::new(move |model: &mut UiModel| {
                    // Best-effort: Demucs may not be installed. Don't clobber the
                    // error banner on every song — just leave the row empty.
                    model.pending_demucs_spectrogram_video_id = None;
                    eprintln!("kpopmvlyrics: vocals spectrogram failed: {err}");
                }) as Box<dyn FnOnce(&mut UiModel) + Send>),
            },
            None => Ok(Box::new(|model: &mut UiModel| {
                model.pending_demucs_spectrogram_video_id = None;
            }) as Box<dyn FnOnce(&mut UiModel) + Send>),
        };
        let _ = work_tx.send(BackgroundUpdate {
            label: "Vocals spectrogram",
            result,
        });
    });
}

fn spawn_work<F>(
    work_tx: std::sync::mpsc::Sender<BackgroundUpdate>,
    view: Rc<UiView>,
    label: &'static str,
    work: F,
) where
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

fn spawn_stream_reload(
    view: Rc<UiView>,
    work_tx: std::sync::mpsc::Sender<BackgroundUpdate>,
    url_entry: Entry,
    quality_combo: IdDropDown,
) {
    let (position_ms, was_playing, volume) = view
        .model
        .try_borrow()
        .map(|model| {
            let snapshot = model.player.borrow().snapshot();
            (snapshot.ms, snapshot.playing, model.volume)
        })
        .unwrap_or((0, false, 1.0));

    let format_id = selected_format_id(&quality_combo);
    view.refresh_mut(|model| {
        model.url = url_entry.text().to_string();
        model.selected_format = format_id;
        model.pending_seek_ms = Some(position_ms);
        model.pending_autoplay = was_playing;
        model.volume = volume;
    });

    spawn_work(work_tx, view, "Stream", move |snapshot| {
        let spec = snapshot
            .ctx
            .resolve_stream(&snapshot.url, snapshot.selected_format.as_deref())?;
        Ok(Box::new(move |model: &mut UiModel| {
            model.pending_stream = Some(spec);
        }) as Box<dyn FnOnce(&mut UiModel) + Send>)
    });
}

fn spawn_toggle_play_pause(view: Rc<UiView>) {
    spawn_player_work(view, |player| {
        let snapshot = player.snapshot();
        if snapshot.playing {
            player.pause()
        } else {
            player.play()
        }
    });
}

fn focus_is_text_widget(window: &ApplicationWindow) -> bool {
    gtk::prelude::GtkWindowExt::focus(window).is_some_and(|widget| {
        widget.downcast_ref::<Entry>().is_some() || widget.downcast_ref::<gtk::TextView>().is_some()
    })
}

fn spawn_player_load(view: Rc<UiView>, spec: StreamSpec) {
    let pending = view.model.try_borrow_mut().ok().map(|mut model| {
        (
            model.pending_seek_ms.take(),
            model.pending_autoplay,
            model.volume,
            Rc::clone(&model.player),
        )
    });

    let Some((pending_seek_ms, pending_autoplay, volume, player)) = pending else {
        if let Ok(mut model) = view.model.try_borrow_mut() {
            model.pending_stream = Some(spec);
        }
        return;
    };

    let load_result = catch_unwind(AssertUnwindSafe(|| {
        player
            .try_borrow_mut()
            .map_err(|_| "Video player is busy".to_string())
            .and_then(|mut player| {
                player.load(spec)?;
                let _ = player.set_volume(volume);
                if let Some(ms) = pending_seek_ms {
                    player.seek(ms)?;
                }
                if pending_autoplay {
                    player.play()?;
                }
                Ok(())
            })
    }));

    match load_result {
        Ok(Ok(())) => {
            if let Ok(mut model) = view.model.try_borrow_mut() {
                model.player_loaded = true;
                model.pending_autoplay = false;
                model.error = None;
                if model.open_progress.is_some() {
                    model.open_progress = None;
                }
            }
            apply_url_entry_progress(&view, None);
        }
        Ok(Err(err)) => {
            if let Ok(mut model) = view.model.try_borrow_mut() {
                model.pending_autoplay = false;
                model.error = Some(err);
                if model.open_progress.is_some() {
                    model.open_progress = None;
                }
            }
            apply_url_entry_progress(&view, None);
        }
        Err(payload) => {
            let message = panic_payload_message(payload);
            eprintln!("kpopmvlyrics: video load panicked: {message}");
            if let Ok(mut model) = view.model.try_borrow_mut() {
                model.pending_autoplay = false;
                model.error = Some(format!("Video player failed: {message}"));
                if model.open_progress.is_some() {
                    model.open_progress = None;
                }
            }
            apply_url_entry_progress(&view, None);
        }
    }

    glib::idle_add_local_once(move || {
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

fn selected_format_id(combo: &IdDropDown) -> Option<String> {
    combo.active_id().filter(|id| id.as_str() != "auto")
}

fn clear_children(container: &GtkBox) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}

const NO_ACTIVE_LYRIC: usize = usize::MAX;

fn active_lyric_index(alignment: &[AlignmentLine], current_ms: i64) -> usize {
    let synced: Vec<&AlignmentLine> = alignment
        .iter()
        .filter(|line| has_playback_timing(line))
        .collect();

    if synced.is_empty() {
        return NO_ACTIVE_LYRIC;
    }

    if let Some(active) = synced
        .iter()
        .find(|line| current_ms >= line.start_ms && current_ms < line.end_ms)
    {
        return active.lyric_index;
    }

    // Fallback: latest line whose start time has passed.
    // When several lines share a start time, prefer the earliest lyric in the song.
    if let Some(active) = synced
        .iter()
        .filter(|line| current_ms >= line.start_ms)
        .max_by(|left, right| {
            left.start_ms
                .cmp(&right.start_ms)
                .then_with(|| right.lyric_index.cmp(&left.lyric_index))
        })
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

    view.suppress_quality_reload.set(true);
    let combo = &view.quality_combo;
    combo.clear();
    combo.append("auto", "Auto");
    for format in &model.formats {
        combo.append(&format.format_id, &format.label);
    }
    if let Some(selected) = &model.selected_format {
        combo.set_active_id(selected);
    } else {
        combo.set_active_id("auto");
    }
    view.suppress_quality_reload.set(false);
}

fn icon_media_button(icon_name: &str, label: &str) -> Button {
    let button = Button::new();
    let row = GtkBox::new(Orientation::Horizontal, 4);
    if gtk::IconTheme::for_display(&gtk::gdk::Display::default().expect("default display"))
        .has_icon(icon_name)
    {
        let icon = gtk::Image::from_icon_name(icon_name);
        row.append(&icon);
    }
    row.append(&Label::new(Some(label)));
    button.set_child(Some(&row));
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

fn member_image_path(
    member: &MemberProfile,
    image_cache: &HashMap<String, String>,
) -> Option<String> {
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

    // Use a browser-like User-Agent and a Referer: image CDNs (kpopping's
    // included) increasingly reject hotlinked requests that lack them, which
    // shows up as member cards with blank portraits.
    let client = match Client::builder()
        .user_agent(
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) \
             Chrome/124.0 Safari/537.36",
        )
        .build()
    {
        Ok(client) => client,
        Err(err) => {
            crate::log::verbose(format!("member image client build failed: {err}"));
            return None;
        }
    };
    let response = match client
        .get(url)
        .header(reqwest::header::REFERER, "https://kpopping.com/")
        .send()
    {
        Ok(response) => response,
        Err(err) => {
            crate::log::verbose(format!("member image fetch failed url={url} err={err}"));
            return None;
        }
    };
    if !response.status().is_success() {
        crate::log::verbose(format!(
            "member image fetch status={} url={url}",
            response.status()
        ));
        return None;
    }
    let bytes = response.bytes().ok()?;
    if bytes.is_empty() {
        crate::log::verbose(format!("member image empty body url={url}"));
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

const MEMBER_PORTRAIT_WIDTH: i32 = 150;
const MEMBER_PORTRAIT_HEIGHT: i32 = 210;
const MEMBER_STRIP_HEIGHT: i32 = 280;

fn load_stage_css() {
    let provider = CssProvider::new();
    provider.load_from_string(include_str!("stage.css"));
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

fn apply_member_name_style(label: &Label, color: &str, active: bool) {
    let name = glib::markup_escape_text(label.text().as_str());
    let color = glib::markup_escape_text(color);
    let weight = if active { "800" } else { "600" };
    label.set_markup(&format!(
        "<span foreground='{color}' weight='{weight}'>{name}</span>"
    ));
}

fn scaled_portrait_pixbuf(
    pixbuf: &gdk_pixbuf::Pixbuf,
    width: i32,
    height: i32,
) -> gdk_pixbuf::Pixbuf {
    let scale = (width as f64 / pixbuf.width() as f64).max(height as f64 / pixbuf.height() as f64);
    let scaled_w = (pixbuf.width() as f64 * scale).round().max(1.0) as i32;
    let scaled_h = (pixbuf.height() as f64 * scale).round().max(1.0) as i32;
    let scaled = pixbuf
        .scale_simple(scaled_w, scaled_h, gdk_pixbuf::InterpType::Bilinear)
        .unwrap_or_else(|| pixbuf.clone());
    let x = ((scaled_w - width) / 2).max(0);
    let y = ((scaled_h - height) / 2).max(0);
    if scaled_w > width && scaled_h > height {
        if let Some(copy) = scaled.copy() {
            return copy.new_subpixbuf(x, y, width.min(scaled_w), height.min(scaled_h));
        }
    }
    scaled
}

fn member_portrait_texture(
    member: &MemberProfile,
    image_cache: &HashMap<String, String>,
) -> Option<gdk::Texture> {
    let path = member_image_path(member, image_cache)?;
    let pixbuf = gdk_pixbuf::Pixbuf::from_file(&path).ok()?;
    let scaled = scaled_portrait_pixbuf(&pixbuf, MEMBER_PORTRAIT_WIDTH, MEMBER_PORTRAIT_HEIGHT);
    Some(gdk::Texture::for_pixbuf(&scaled))
}

fn member_card_slug(stage_name: &str) -> String {
    stage_name.to_lowercase().replace(' ', "-")
}

fn name_member_card(widget: &impl IsA<gtk::Widget>, stage_name: &str) {
    widget.set_widget_name(&format!("member-card-{}", member_card_slug(stage_name)));
}

fn rebuild_member_card_css(provider: &CssProvider, members: &[MemberProfile]) {
    let mut css = String::new();
    for member in members {
        let slug = member_card_slug(&member.stage_name);
        let color = &member.color;
        css.push_str(&format!(
            "#member-card-{slug}.active {{
                box-shadow: inset 0 0 0 4px {color};
                border-radius: 8px;
            }}
            #member-card-{slug}.backing {{
                box-shadow: inset 0 0 0 2px {color};
                border-radius: 8px;
            }}"
        ));
    }
    provider.load_from_string(&css);
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
    view.member_stage.borrow_mut().last_highlight.replace(None);

    let Some(song) = &model.song else {
        let empty = Label::new(Some("Members appear after lyrics are loaded"));
        empty.set_opacity(0.7);
        container.append(&empty);
        return;
    };

    prefetch_member_images(view, &song.members);
    rebuild_member_card_css(&view.member_card_css_provider, &song.members);

    let Some(view_rc) = view.this.try_borrow().ok().and_then(|this| this.clone()) else {
        return;
    };

    let mut stage_buttons = Vec::new();
    for member in &song.members {
        let button = Button::new();
        button.add_css_class("flat");
        button.set_focus_on_click(false);
        button.set_hexpand(true);
        button.set_halign(gtk::Align::Fill);
        let stage_name = member.stage_name.clone();
        let member_color = member.color.clone();

        let border_wrap = GtkBox::new(Orientation::Vertical, 0);
        border_wrap.add_css_class("member-card");
        name_member_card(&border_wrap, &stage_name);
        border_wrap.set_margin_start(4);
        border_wrap.set_margin_end(4);
        border_wrap.set_margin_top(2);
        border_wrap.set_margin_bottom(2);

        let inner = GtkBox::new(Orientation::Vertical, 6);
        inner.set_halign(gtk::Align::Center);
        inner.set_margin_start(4);
        inner.set_margin_end(4);
        inner.set_margin_top(4);
        inner.set_margin_bottom(4);

        let portrait_texture = member_portrait_texture(member, &image_cache);

        let portrait_frame = GtkBox::new(Orientation::Vertical, 0);
        portrait_frame.set_size_request(MEMBER_PORTRAIT_WIDTH, MEMBER_PORTRAIT_HEIGHT);
        portrait_frame.add_css_class("member-portrait");
        portrait_frame.set_opacity(0.42);

        if let Some(texture) = portrait_texture.as_ref() {
            // GtkPicture (not GtkImage) is required here: GtkImage renders a
            // paintable at icon size, which shrinks the portrait to a thumbnail.
            let image = gtk::Picture::new();
            image.set_paintable(Some(texture));
            image.set_content_fit(gtk::ContentFit::Cover);
            image.set_size_request(MEMBER_PORTRAIT_WIDTH, MEMBER_PORTRAIT_HEIGHT);
            portrait_frame.append(&image);
        } else {
            let placeholder = Label::new(None);
            placeholder.set_size_request(MEMBER_PORTRAIT_WIDTH, MEMBER_PORTRAIT_HEIGHT);
            placeholder.set_markup(&format!(
                "<span size='xx-large' weight='bold'>{}</span>",
                glib::markup_escape_text(&initials(&stage_name))
            ));
            placeholder.set_valign(gtk::Align::Center);
            portrait_frame.append(&placeholder);
        }
        inner.append(&portrait_frame);

        let name = Label::new(Some(&stage_name));
        name.add_css_class("member-name");
        apply_member_name_style(&name, &member_color, false);
        inner.append(&name);

        border_wrap.append(&inner);
        button.set_child(Some(&border_wrap));

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

        container.append(&button);
        stage_buttons.push(MemberButton {
            stage_name,
            color: member_color,
            border_wrap,
            portrait_frame,
            name_label: name,
        });
    }
    view.member_stage.borrow_mut().buttons = stage_buttons;
}

fn initials(name: &str) -> String {
    name.split_whitespace()
        .filter(|part| !part.is_empty())
        .take(2)
        .filter_map(|part| part.chars().next())
        .map(|ch| ch.to_uppercase().collect::<String>())
        .collect()
}
