#![cfg(desktop_unix)]

//! Final Cut / Premiere-style timeline editor for lyrics.
//!
//! Three horizontal tracks (Lead vocals / Backing vocals / Adlibs). Each lyric line
//! that has alignment timing is drawn as a clip positioned by its start/end ms. Clips
//! can be dragged to move, dragged at the edges to resize (retime), selected (which
//! seeks the video and binds the inspector), created, and deleted. A playhead follows
//! the video and a time ruler sits above the tracks. Zoom changes pixels-per-ms.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{
    Box as GtkBox, Button, DrawingArea, Entry, Fixed, GestureClick, GestureDrag, Label,
    Orientation, Overlay, ScrolledWindow, SpinButton,
};

use crate::models::{LyricLayer, MemberProfile};

use super::{spawn_player_work, IdDropDown, UiView};

const RULER_H: f64 = 22.0;
const ROW_H: f64 = 56.0;
const CLIP_PAD: f64 = 4.0;
const EDGE_PX: f64 = 8.0;
const MIN_CLIP_PX: f64 = 14.0;
const HEADER_W: i32 = 120;

const MIN_PX_PER_MS: f64 = 0.01;
const MAX_PX_PER_MS: f64 = 0.6;
const DEFAULT_PX_PER_MS: f64 = 0.06;

#[derive(Clone, Copy)]
enum DragMode {
    Move,
    ResizeStart,
    ResizeEnd,
}

struct DragCtx {
    mode: DragMode,
    orig_x: f64,
    orig_w: f64,
    y: f64,
}

struct TimelineState {
    px_per_ms: f64,
    selected: Option<usize>,
    playhead_ms: i64,
}

struct Inspector {
    row: GtkBox,
    original: Entry,
    romanization: Entry,
    english: Entry,
    member: IdDropDown,
    layer: IdDropDown,
    start: SpinButton,
    end: SpinButton,
    delete: Button,
    suppress: Rc<Cell<bool>>,
}

pub struct Timeline {
    pub root: GtkBox,
    fixed: Fixed,
    drawing: DrawingArea,
    add_lead: Button,
    add_backing: Button,
    add_adlib: Button,
    zoom_in: Button,
    zoom_out: Button,
    inspector: Inspector,
    color_provider: Rc<gtk::CssProvider>,
    state: Rc<RefCell<TimelineState>>,
}

impl Timeline {
    pub fn new() -> Rc<Self> {
        let state = Rc::new(RefCell::new(TimelineState {
            px_per_ms: DEFAULT_PX_PER_MS,
            selected: None,
            playhead_ms: 0,
        }));

        let root = GtkBox::new(Orientation::Vertical, 6);
        root.set_vexpand(true);

        // Toolbar: zoom + add-clip-per-layer.
        let toolbar = GtkBox::new(Orientation::Horizontal, 6);
        let zoom_out = Button::with_label("Zoom −");
        let zoom_in = Button::with_label("Zoom +");
        let add_lead = Button::with_label("+ Lead");
        let add_backing = Button::with_label("+ Backing");
        let add_adlib = Button::with_label("+ Adlib");
        toolbar.append(&zoom_out);
        toolbar.append(&zoom_in);
        let spacer = GtkBox::new(Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        toolbar.append(&spacer);
        toolbar.append(&add_lead);
        toolbar.append(&add_backing);
        toolbar.append(&add_adlib);
        root.append(&toolbar);

        // Track-name header column + scrolling timeline body.
        let body = GtkBox::new(Orientation::Horizontal, 0);
        body.set_vexpand(true);

        let headers = GtkBox::new(Orientation::Vertical, 0);
        headers.set_size_request(HEADER_W, -1);
        headers.add_css_class("timeline-headers");
        let ruler_spacer = GtkBox::new(Orientation::Vertical, 0);
        ruler_spacer.set_size_request(HEADER_W, RULER_H as i32);
        headers.append(&ruler_spacer);
        for layer in LyricLayer::ALL {
            let label = Label::new(Some(layer.label()));
            label.set_xalign(0.0);
            label.set_size_request(HEADER_W, ROW_H as i32);
            label.set_margin_start(8);
            label.add_css_class("timeline-track-header");
            headers.append(&label);
        }
        body.append(&headers);

        let drawing = DrawingArea::new();
        drawing.set_hexpand(true);
        drawing.set_vexpand(true);
        let fixed = Fixed::new();
        fixed.set_hexpand(true);
        fixed.set_vexpand(true);

        let overlay = Overlay::new();
        overlay.set_child(Some(&drawing));
        overlay.add_overlay(&fixed);
        overlay.set_measure_overlay(&fixed, true);
        overlay.set_hexpand(true);
        overlay.set_vexpand(true);

        let scroller = ScrolledWindow::new();
        scroller.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Never);
        scroller.set_hexpand(true);
        scroller.set_vexpand(true);
        scroller.set_min_content_height((RULER_H + ROW_H * 3.0) as i32);
        scroller.set_child(Some(&overlay));
        body.append(&scroller);
        root.append(&body);

        // Inspector for the selected clip.
        let inspector = build_inspector();
        root.append(&inspector.row);

        // Background grid, track separators and playhead are painted by the drawing area.
        {
            let state = Rc::clone(&state);
            drawing.set_draw_func(move |area, ctx, width, height| {
                draw_background(area, ctx, width, height, &state);
            });
        }

        Rc::new(Self {
            root,
            fixed,
            drawing,
            add_lead,
            add_backing,
            add_adlib,
            zoom_in,
            zoom_out,
            inspector,
            color_provider: Rc::new(gtk::CssProvider::new()),
            state,
        })
    }

    /// Wire interactions that need the live `UiView` (created after the widgets).
    pub fn connect(self: &Rc<Self>, view: &Rc<UiView>) {
        gtk::style_context_add_provider_for_display(
            &self.root.display(),
            &*self.color_provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );

        // Zoom buttons.
        {
            let this = Rc::clone(self);
            let view = Rc::clone(view);
            self.zoom_in.connect_clicked(move |_| {
                let next = (this.state.borrow().px_per_ms * 1.4).min(MAX_PX_PER_MS);
                this.state.borrow_mut().px_per_ms = next;
                this.relayout(&view);
            });
        }
        {
            let this = Rc::clone(self);
            let view = Rc::clone(view);
            self.zoom_out.connect_clicked(move |_| {
                let next = (this.state.borrow().px_per_ms / 1.4).max(MIN_PX_PER_MS);
                this.state.borrow_mut().px_per_ms = next;
                this.relayout(&view);
            });
        }

        // Add-clip-per-layer buttons (created at the playhead).
        for (button, layer) in [
            (&self.add_lead, LyricLayer::Lead),
            (&self.add_backing, LyricLayer::Backing),
            (&self.add_adlib, LyricLayer::Adlib),
        ] {
            let this = Rc::clone(self);
            let view = Rc::clone(view);
            button.connect_clicked(move |_| {
                let start_ms = this.state.borrow().playhead_ms;
                let mut new_index = None;
                if let Ok(mut model) = view.model.try_borrow_mut() {
                    new_index = model.add_lyric_line(layer, start_ms);
                    model.editor_table_dirty = true;
                }
                if let Some(index) = new_index {
                    this.state.borrow_mut().selected = Some(index);
                    this.relayout(&view);
                    this.populate_inspector(&view);
                }
            });
        }

        self.connect_inspector(view);
    }

    fn connect_inspector(self: &Rc<Self>, view: &Rc<UiView>) {
        let ins = &self.inspector;

        // Text fields.
        for entry in [&ins.original, &ins.romanization, &ins.english] {
            let this = Rc::clone(self);
            let view = Rc::clone(view);
            entry.connect_changed(move |_| this.commit_inspector_text(&view));
        }

        // Member.
        {
            let this = Rc::clone(self);
            let view = Rc::clone(view);
            ins.member.connect_changed(move |combo| {
                if this.inspector.suppress.get() {
                    return;
                }
                let Some(index) = this.state.borrow().selected else {
                    return;
                };
                let member = combo.active_id().filter(|id| !id.is_empty());
                if let Ok(mut model) = view.model.try_borrow_mut() {
                    model.set_line_member(index, member);
                    model.editor_table_dirty = true;
                }
                this.relayout(&view);
            });
        }

        // Layer (moves the clip to another track).
        {
            let this = Rc::clone(self);
            let view = Rc::clone(view);
            ins.layer.connect_changed(move |combo| {
                if this.inspector.suppress.get() {
                    return;
                }
                let Some(index) = this.state.borrow().selected else {
                    return;
                };
                let layer = combo
                    .active_id()
                    .and_then(|id| LyricLayer::from_str(&id))
                    .unwrap_or_default();
                if let Ok(mut model) = view.model.try_borrow_mut() {
                    model.set_line_layer(index, layer);
                    model.editor_table_dirty = true;
                }
                this.relayout(&view);
            });
        }

        // Start / end spin buttons.
        {
            let this = Rc::clone(self);
            let view = Rc::clone(view);
            ins.start.connect_value_changed(move |spin| {
                if this.inspector.suppress.get() {
                    return;
                }
                this.commit_inspector_time(&view, Some(spin.value() as i64), None);
            });
        }
        {
            let this = Rc::clone(self);
            let view = Rc::clone(view);
            ins.end.connect_value_changed(move |spin| {
                if this.inspector.suppress.get() {
                    return;
                }
                this.commit_inspector_time(&view, None, Some(spin.value() as i64));
            });
        }

        // Delete.
        {
            let this = Rc::clone(self);
            let view = Rc::clone(view);
            ins.delete.connect_clicked(move |_| {
                let Some(index) = this.state.borrow().selected else {
                    return;
                };
                if let Ok(mut model) = view.model.try_borrow_mut() {
                    model.delete_lyric_line(index);
                    model.editor_table_dirty = true;
                }
                this.state.borrow_mut().selected = None;
                this.relayout(&view);
                this.populate_inspector(&view);
            });
        }
    }

    fn commit_inspector_text(self: &Rc<Self>, view: &Rc<UiView>) {
        if self.inspector.suppress.get() {
            return;
        }
        let Some(index) = self.state.borrow().selected else {
            return;
        };
        let original = self.inspector.original.text().to_string();
        let romanization = Some(self.inspector.romanization.text().to_string());
        let english = Some(self.inspector.english.text().to_string());
        if let Ok(mut model) = view.model.try_borrow_mut() {
            model.set_line_text(index, original, romanization, english);
            model.editor_table_dirty = true;
        }
        // Relayout so the clip label reflects the new text.
        self.relayout(view);
    }

    fn commit_inspector_time(
        self: &Rc<Self>,
        view: &Rc<UiView>,
        start: Option<i64>,
        end: Option<i64>,
    ) {
        let Some(index) = self.state.borrow().selected else {
            return;
        };
        if let Ok(mut model) = view.model.try_borrow_mut() {
            model.update_alignment(index, |line| {
                if let Some(value) = start {
                    line.start_ms = value.max(0).min(line.end_ms - 1);
                }
                if let Some(value) = end {
                    line.end_ms = value.max(line.start_ms + 1);
                }
                line.needs_review = true;
            });
            model.editor_table_dirty = true;
        }
        self.relayout(view);
    }

    /// Move the playhead (called every position tick); cheap, no relayout.
    pub fn set_playhead(&self, ms: i64) {
        {
            let mut state = self.state.borrow_mut();
            if state.playhead_ms == ms {
                return;
            }
            state.playhead_ms = ms;
        }
        self.drawing.queue_draw();
    }

    /// Full relayout: rebuild clip widgets and the ruler from the model.
    pub fn relayout(self: &Rc<Self>, view: &Rc<UiView>) {
        clear_fixed(&self.fixed);

        let Ok(model) = view.model.try_borrow() else {
            return;
        };
        let Some(song) = model.song.as_ref() else {
            self.set_content_size(0.0);
            self.drawing.queue_draw();
            return;
        };

        let px_per_ms = self.state.borrow().px_per_ms;
        let selected = self.state.borrow().selected;

        // Content width covers the video duration and the last clip end.
        let max_end = model
            .alignment
            .iter()
            .map(|line| line.end_ms)
            .max()
            .unwrap_or(0);
        let duration = model.duration_ms.map(|ms| ms as i64).unwrap_or(0);
        let content_ms = max_end.max(duration).max(10_000);
        self.set_content_size(content_ms as f64 * px_per_ms);

        // Ruler ticks every nice interval (5s baseline, scaled with zoom).
        let tick_ms = ruler_tick_ms(px_per_ms);
        let mut t = 0i64;
        while t <= content_ms {
            let label = Label::new(Some(&fmt_clock(t)));
            label.add_css_class("timeline-ruler-label");
            label.set_xalign(0.0);
            self.fixed.put(&label, t as f64 * px_per_ms + 2.0, 1.0);
            t += tick_ms;
        }

        // Clips.
        let mut css = String::new();
        for line in &song.lines {
            let Some(timing) = model.alignment.iter().find(|a| a.lyric_index == line.index) else {
                continue;
            };
            let layer_index = LyricLayer::ALL
                .iter()
                .position(|layer| *layer == line.layer)
                .unwrap_or(0);
            let x = timing.start_ms.max(0) as f64 * px_per_ms;
            let w = ((timing.end_ms - timing.start_ms).max(1) as f64 * px_per_ms).max(MIN_CLIP_PX);
            let y = RULER_H + layer_index as f64 * ROW_H + CLIP_PAD;
            let h = ROW_H - 2.0 * CLIP_PAD;

            let clip = self.build_clip(view, line.index, line, selected, x, w, y);
            if let Some(color) = clip_color(line, &song.members) {
                css.push_str(&format!(
                    "#timeline-clip-{} {{ background-color: {color}; }}\n",
                    line.index
                ));
            }
            clip.set_size_request(w as i32, h as i32);
            self.fixed.put(&clip, x, y);
        }
        self.color_provider.load_from_string(&css);

        drop(model);
        self.drawing.queue_draw();
    }

    fn set_content_size(&self, width: f64) {
        let w = (width.ceil() as i32).max(1);
        let h = (RULER_H + ROW_H * 3.0) as i32;
        self.drawing.set_size_request(w, h);
        self.fixed.set_size_request(w, h);
    }

    #[allow(clippy::too_many_arguments)]
    fn build_clip(
        self: &Rc<Self>,
        view: &Rc<UiView>,
        index: usize,
        line: &crate::models::LyricLine,
        selected: Option<usize>,
        clip_x: f64,
        clip_w: f64,
        clip_y: f64,
    ) -> GtkBox {
        let clip = GtkBox::new(Orientation::Vertical, 0);
        clip.set_widget_name(&format!("timeline-clip-{index}"));
        clip.add_css_class("timeline-clip");
        if selected == Some(index) {
            clip.add_css_class("selected");
        }
        clip.set_overflow(gtk::Overflow::Hidden);

        let text = if line.original.trim().is_empty() {
            "(empty)".to_string()
        } else {
            line.original.clone()
        };
        let label = Label::new(Some(&text));
        label.set_xalign(0.0);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.set_margin_start(6);
        label.set_margin_end(6);
        label.set_margin_top(3);
        clip.append(&label);
        clip.set_tooltip_text(Some(&line.original));

        // Click: select + bind inspector (the fixed-level click handles seeking).
        {
            let this = Rc::clone(self);
            let view = Rc::clone(view);
            let click = GestureClick::new();
            click.connect_released(move |_, _, _, _| {
                this.state.borrow_mut().selected = Some(index);
                this.relayout(&view);
                this.populate_inspector(&view);
            });
            clip.add_controller(click);
        }

        // Drag: move the body, resize at the edges.
        {
            let this = Rc::clone(self);
            let view = Rc::clone(view);
            let clip_for_drag = clip.clone();
            let ctx: Rc<RefCell<Option<DragCtx>>> = Rc::new(RefCell::new(None));
            let drag = GestureDrag::new();

            {
                let ctx = Rc::clone(&ctx);
                drag.connect_drag_begin(move |_, start_x, _start_y| {
                    let mode = if start_x <= EDGE_PX {
                        DragMode::ResizeStart
                    } else if start_x >= clip_w - EDGE_PX {
                        DragMode::ResizeEnd
                    } else {
                        DragMode::Move
                    };
                    *ctx.borrow_mut() = Some(DragCtx {
                        mode,
                        orig_x: clip_x,
                        orig_w: clip_w,
                        y: clip_y,
                    });
                });
            }
            {
                let ctx = Rc::clone(&ctx);
                let this = Rc::clone(&this);
                let clip_for_drag = clip_for_drag.clone();
                drag.connect_drag_update(move |_, off_x, _off_y| {
                    if let Some(ctx) = ctx.borrow().as_ref() {
                        let (nx, nw) = apply_drag(ctx, off_x);
                        this.fixed.move_(&clip_for_drag, nx, ctx.y);
                        clip_for_drag.set_size_request(nw as i32, (ROW_H - 2.0 * CLIP_PAD) as i32);
                    }
                });
            }
            {
                let ctx = Rc::clone(&ctx);
                let this = Rc::clone(&this);
                let view = Rc::clone(&view);
                drag.connect_drag_end(move |_, off_x, _off_y| {
                    let Some(ctx) = ctx.borrow_mut().take() else {
                        return;
                    };
                    let (nx, nw) = apply_drag(&ctx, off_x);
                    let px_per_ms = this.state.borrow().px_per_ms;
                    let start_ms = (nx / px_per_ms).round() as i64;
                    let end_ms = ((nx + nw) / px_per_ms).round() as i64;
                    if let Ok(mut model) = view.model.try_borrow_mut() {
                        model.update_alignment(index, |line| {
                            line.start_ms = start_ms.max(0);
                            line.end_ms = end_ms.max(line.start_ms + 1);
                            line.needs_review = true;
                        });
                        model.editor_table_dirty = true;
                    }
                    this.state.borrow_mut().selected = Some(index);
                    this.relayout(&view);
                    this.populate_inspector(&view);
                });
            }
            clip.add_controller(drag);
        }

        clip
    }

    /// Fill the inspector from the currently-selected clip (or disable it).
    pub fn populate_inspector(self: &Rc<Self>, view: &Rc<UiView>) {
        let ins = &self.inspector;
        ins.suppress.set(true);

        let selected = self.state.borrow().selected;
        let Ok(model) = view.model.try_borrow() else {
            ins.suppress.set(false);
            return;
        };

        // Rebuild member options from the current song.
        ins.member.clear();
        ins.member.append("", "All");
        if let Some(song) = model.song.as_ref() {
            for member in &song.members {
                ins.member.append(&member.stage_name, &member.stage_name);
            }
        }

        let line = selected.and_then(|index| {
            model
                .song
                .as_ref()
                .and_then(|song| song.lines.iter().find(|line| line.index == index))
        });

        match line {
            Some(line) => {
                ins.row.set_sensitive(true);
                ins.original.set_text(&line.original);
                ins.romanization
                    .set_text(line.romanization.as_deref().unwrap_or(""));
                ins.english.set_text(line.english.as_deref().unwrap_or(""));
                ins.member
                    .set_active_id(line.member.as_deref().unwrap_or(""));
                ins.layer.set_active_id(line.layer.as_str());
                if let Some(timing) = selected
                    .and_then(|index| model.alignment.iter().find(|a| a.lyric_index == index))
                {
                    ins.start.set_value(timing.start_ms as f64);
                    ins.end.set_value(timing.end_ms as f64);
                }
            }
            None => {
                ins.row.set_sensitive(false);
                ins.original.set_text("");
                ins.romanization.set_text("");
                ins.english.set_text("");
                ins.start.set_value(0.0);
                ins.end.set_value(0.0);
            }
        }

        drop(model);
        ins.suppress.set(false);
    }

    /// Seek the video and move the playhead to `ms` (used by the ruler click).
    fn seek_to(&self, view: &Rc<UiView>, ms: i64) {
        let ms = ms.max(0);
        self.set_playhead(ms);
        if let Ok(mut model) = view.model.try_borrow_mut() {
            model.begin_seek(ms as u64);
            model.pending_seek_ms = Some(ms as u64);
        }
        spawn_player_work(Rc::clone(view), move |player| player.seek(ms as u64));
    }

    /// Connect the click-to-seek gesture on the timeline body. Called once after build.
    pub fn connect_seek(self: &Rc<Self>, view: &Rc<UiView>) {
        let this = Rc::clone(self);
        let view = Rc::clone(view);
        let click = GestureClick::new();
        click.connect_released(move |gesture, _, x, _| {
            let _ = gesture;
            let px_per_ms = this.state.borrow().px_per_ms;
            if px_per_ms > 0.0 {
                this.seek_to(&view, (x / px_per_ms).round() as i64);
            }
        });
        self.fixed.add_controller(click);
    }
}

fn build_inspector() -> Inspector {
    let row = GtkBox::new(Orientation::Horizontal, 6);
    row.add_css_class("timeline-inspector");
    row.set_sensitive(false);

    let original = Entry::new();
    original.set_placeholder_text(Some("Original"));
    original.set_hexpand(true);
    let romanization = Entry::new();
    romanization.set_placeholder_text(Some("Romanization"));
    romanization.set_hexpand(true);
    let english = Entry::new();
    english.set_placeholder_text(Some("English"));
    english.set_hexpand(true);

    let member = IdDropDown::new();
    member.append("", "All");
    let layer = IdDropDown::new();
    for value in LyricLayer::ALL {
        layer.append(value.as_str(), value.label());
    }

    let start = SpinButton::with_range(0.0, 3_600_000.0, 50.0);
    let end = SpinButton::with_range(0.0, 3_600_000.0, 50.0);
    let delete = Button::with_label("Delete");
    delete.add_css_class("destructive-action");

    row.append(&original);
    row.append(&romanization);
    row.append(&english);
    row.append(&Label::new(Some("Member")));
    row.append(&member.widget);
    row.append(&Label::new(Some("Layer")));
    row.append(&layer.widget);
    row.append(&Label::new(Some("Start")));
    row.append(&start);
    row.append(&Label::new(Some("End")));
    row.append(&end);
    row.append(&delete);

    Inspector {
        row,
        original,
        romanization,
        english,
        member,
        layer,
        start,
        end,
        delete,
        suppress: Rc::new(Cell::new(false)),
    }
}

/// Compute the clip's new (x, width) for a drag offset, clamped to valid ranges.
fn apply_drag(ctx: &DragCtx, off_x: f64) -> (f64, f64) {
    match ctx.mode {
        DragMode::Move => ((ctx.orig_x + off_x).max(0.0), ctx.orig_w),
        DragMode::ResizeStart => {
            let nx = (ctx.orig_x + off_x).max(0.0);
            let right = ctx.orig_x + ctx.orig_w;
            let nx = nx.min(right - MIN_CLIP_PX);
            (nx, right - nx)
        }
        DragMode::ResizeEnd => {
            let nw = (ctx.orig_w + off_x).max(MIN_CLIP_PX);
            (ctx.orig_x, nw)
        }
    }
}

fn draw_background(
    _area: &DrawingArea,
    ctx: &gtk::cairo::Context,
    width: i32,
    height: i32,
    state: &Rc<RefCell<TimelineState>>,
) {
    let width = width as f64;
    let height = height as f64;
    let state = state.borrow();

    // Track separators.
    ctx.set_source_rgba(1.0, 1.0, 1.0, 0.10);
    ctx.set_line_width(1.0);
    for i in 0..=3 {
        let y = RULER_H + i as f64 * ROW_H;
        ctx.move_to(0.0, y);
        ctx.line_to(width, y);
    }
    let _ = ctx.stroke();

    // Vertical gridlines at ruler ticks.
    let tick_ms = ruler_tick_ms(state.px_per_ms);
    let tick_px = tick_ms as f64 * state.px_per_ms;
    if tick_px > 0.0 {
        ctx.set_source_rgba(1.0, 1.0, 1.0, 0.06);
        let mut x = 0.0;
        while x <= width {
            ctx.move_to(x, RULER_H);
            ctx.line_to(x, height);
            x += tick_px;
        }
        let _ = ctx.stroke();
    }

    // Playhead.
    let px = state.playhead_ms as f64 * state.px_per_ms;
    if px >= 0.0 && px <= width {
        ctx.set_source_rgba(1.0, 0.25, 0.35, 0.95);
        ctx.set_line_width(2.0);
        ctx.move_to(px, 0.0);
        ctx.line_to(px, height);
        let _ = ctx.stroke();
    }
}

fn clear_fixed(fixed: &Fixed) {
    while let Some(child) = fixed.first_child() {
        fixed.remove(&child);
    }
}

fn clip_color(line: &crate::models::LyricLine, members: &[MemberProfile]) -> Option<String> {
    let name = line.member.as_deref()?;
    let first = name.split(',').next().unwrap_or(name).trim();
    members
        .iter()
        .find(|member| member.stage_name.eq_ignore_ascii_case(first))
        .map(|member| member.color.clone())
}

/// Choose a ruler tick interval (ms) so labels stay ~80px apart.
fn ruler_tick_ms(px_per_ms: f64) -> i64 {
    const STEPS: [i64; 9] = [
        1_000, 2_000, 5_000, 10_000, 15_000, 30_000, 60_000, 120_000, 300_000,
    ];
    let target_px = 80.0;
    for step in STEPS {
        if step as f64 * px_per_ms >= target_px {
            return step;
        }
    }
    *STEPS.last().unwrap()
}

fn fmt_clock(ms: i64) -> String {
    let total = ms.max(0) / 1000;
    format!("{}:{:02}", total / 60, total % 60)
}
