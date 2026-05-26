use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk::prelude::*;
use gtk::{
    Adjustment, Align, Box as GtkBox, Button, EventBox, Label, Orientation, Overlay, Revealer,
    RevealerTransitionType, Scale, Widget,
};

use super::{icon_media_button, spawn_player_work, UiView};

pub struct VideoOverlay {
    pub overlay: Overlay,
    pub controls_revealer: Revealer,
    pub play_button: Button,
    pub pause_button: Button,
    pub stop_button: Button,
    pub replay_button: Button,
    pub seek_scale: Scale,
    pub seek_adjustment: Adjustment,
    pub     volume_scale: Scale,
    seek_dragging: Rc<Cell<bool>>,
    pending_seek_ms: Rc<Cell<Option<u64>>>,
    hide_source: Rc<RefCell<Option<gtk::glib::SourceId>>>,
}

pub fn build_video_overlay(video_widget: &Widget) -> VideoOverlay {
    let overlay = Overlay::new();
    overlay.add(video_widget);

    let event_box = EventBox::new();
    event_box.set_hexpand(true);
    event_box.set_vexpand(true);
    overlay.add_overlay(&event_box);

    let controls_revealer = Revealer::new();
    controls_revealer.set_transition_type(RevealerTransitionType::Crossfade);
    controls_revealer.set_reveal_child(false);
    controls_revealer.set_halign(Align::Fill);
    controls_revealer.set_valign(Align::End);

    let card = GtkBox::new(Orientation::Vertical, 6);
    card.style_context().add_class("video-controls-card");
    card.set_margin_start(12);
    card.set_margin_end(12);
    card.set_margin_bottom(12);
    card.set_margin_top(8);

    let seek_adjustment = Adjustment::new(0.0, 0.0, 0.0, 1000.0, 5000.0, 0.0);
    let seek_scale = Scale::with_range(Orientation::Horizontal, 0.0, 1.0, 1000.0);
    seek_scale.set_adjustment(&seek_adjustment);
    seek_scale.set_draw_value(false);
    seek_scale.set_hexpand(true);

    let controls_row = GtkBox::new(Orientation::Horizontal, 8);
    let play_button = icon_media_button("media-playback-start", "Play");
    let pause_button = icon_media_button("media-playback-pause", "Pause");
    let stop_button = icon_media_button("media-playback-stop", "Stop");
    let replay_button = icon_media_button("media-playback-start", "Replay");

    let volume_label = Label::new(Some("Vol"));
    volume_label.style_context().add_class("video-controls-label");
    let volume_scale = Scale::with_range(Orientation::Horizontal, 0.0, 100.0, 1.0);
    volume_scale.set_value(100.0);
    volume_scale.set_size_request(90, -1);
    volume_scale.set_draw_value(false);

    controls_row.pack_start(&play_button, false, false, 0);
    controls_row.pack_start(&pause_button, false, false, 0);
    controls_row.pack_start(&stop_button, false, false, 0);
    controls_row.pack_start(&replay_button, false, false, 0);
    controls_row.pack_end(&volume_scale, false, false, 0);
    controls_row.pack_end(&volume_label, false, false, 0);

    card.pack_start(&seek_scale, false, false, 0);
    card.pack_start(&controls_row, false, false, 0);
    controls_revealer.add(&card);
    overlay.add_overlay(&controls_revealer);

    VideoOverlay {
        overlay,
        controls_revealer,
        play_button,
        pause_button,
        stop_button,
        replay_button,
        seek_scale,
        seek_adjustment,
        volume_scale,
        seek_dragging: Rc::new(Cell::new(false)),
        pending_seek_ms: Rc::new(Cell::new(None)),
        hide_source: Rc::new(RefCell::new(None)),
    }
}

impl VideoOverlay {
    pub fn connect_handlers(self: &Rc<Self>, view: &Rc<UiView>) {
        let overlay = self.overlay.clone();
        let controls_revealer = self.controls_revealer.clone();
        let hide_source = Rc::clone(&self.hide_source);

        let show_controls = {
            let controls_revealer = controls_revealer.clone();
            let hide_source = Rc::clone(&hide_source);
            move || {
                if let Some(source) = hide_source.borrow_mut().take() {
                    source.remove();
                }
                controls_revealer.set_reveal_child(true);
            }
        };

        let schedule_hide = {
            let controls_revealer = controls_revealer.clone();
            let hide_source = Rc::clone(&hide_source);
            move || {
                if let Some(source) = hide_source.borrow_mut().take() {
                    source.remove();
                }
                let controls_revealer = controls_revealer.clone();
                let hide_source = Rc::clone(&hide_source);
                let hide_source_for_timeout = Rc::clone(&hide_source);
                let source = gtk::glib::timeout_add_local(Duration::from_millis(2500), move || {
                    controls_revealer.set_reveal_child(false);
                    hide_source_for_timeout.borrow_mut().take();
                    gtk::glib::ControlFlow::Break
                });
                *hide_source.borrow_mut() = Some(source);
            }
        };

        {
            let show_controls = show_controls.clone();
            overlay.connect_enter_notify_event(move |_, _| {
                show_controls();
                gtk::glib::Propagation::Proceed
            });
        }

        {
            let schedule_hide = schedule_hide.clone();
            overlay.connect_leave_notify_event(move |_, _| {
                schedule_hide();
                gtk::glib::Propagation::Proceed
            });
        }

        {
            let show_controls = show_controls.clone();
            let schedule_hide = schedule_hide.clone();
            overlay.connect_button_press_event(move |_, _| {
                if controls_revealer.reveals_child() {
                    schedule_hide();
                } else {
                    show_controls();
                }
                gtk::glib::Propagation::Proceed
            });
        }

        {
            let view = Rc::clone(view);
            self.play_button.connect_clicked(move |_| {
                spawn_player_work(Rc::clone(&view), |player| player.play());
            });
        }

        {
            let view = Rc::clone(view);
            self.pause_button.connect_clicked(move |_| {
                spawn_player_work(Rc::clone(&view), |player| player.pause());
            });
        }

        {
            let view = Rc::clone(view);
            self.stop_button.connect_clicked(move |_| {
                spawn_player_work(Rc::clone(&view), |player| {
                    player.pause()?;
                    player.seek(0)
                });
            });
        }

        {
            let view = Rc::clone(view);
            let pending_seek_ms = Rc::clone(&self.pending_seek_ms);
            let seek_adjustment = self.seek_adjustment.clone();
            self.replay_button.connect_clicked(move |_| {
                pending_seek_ms.set(Some(0));
                seek_adjustment.set_value(0.0);
                spawn_player_work(Rc::clone(&view), |player| player.replay());
            });
        }

        {
            let view = Rc::clone(view);
            let pending_seek_ms = Rc::clone(&self.pending_seek_ms);
            let seek_adjustment = self.seek_adjustment.clone();
            let seek_dragging_press = Rc::clone(&self.seek_dragging);
            let seek_dragging_release = Rc::clone(&self.seek_dragging);
            let seek_dragging_change = Rc::clone(&self.seek_dragging);
            self.seek_scale.connect_button_press_event(move |_, _| {
                seek_dragging_press.set(true);
                gtk::glib::Propagation::Proceed
            });
            self.seek_scale.connect_button_release_event(move |scale, _| {
                let ms = scale.value().max(0.0) as u64;
                seek_dragging_release.set(false);
                pending_seek_ms.set(Some(ms));
                seek_adjustment.set_value(ms as f64);
                spawn_player_work(Rc::clone(&view), move |player| player.seek(ms));
                gtk::glib::Propagation::Proceed
            });
            self.seek_scale.connect_change_value(move |_, scroll_type, _new_value| {
                if matches!(
                    scroll_type,
                    gtk::ScrollType::Jump
                        | gtk::ScrollType::StepForward
                        | gtk::ScrollType::StepBackward
                ) {
                    seek_dragging_change.set(true);
                }
                gtk::glib::Propagation::Proceed
            });
        }

        {
            let view = Rc::clone(view);
            self.volume_scale.connect_value_changed(move |scale| {
                let level = scale.value().clamp(0.0, 100.0) / 100.0;
                if let Ok(mut model) = view.model.try_borrow_mut() {
                    model.volume = level;
                }
                spawn_player_work(Rc::clone(&view), move |player| player.set_volume(level));
            });
        }
    }

    pub fn update_seek_bar(&self, current_ms: i64, duration_ms: Option<u64>) {
        if self.seek_dragging.get() {
            return;
        }

        if let Some(pending) = self.pending_seek_ms.get() {
            let current = current_ms.max(0) as u64;
            if current.abs_diff(pending) > 1_500 {
                self.seek_adjustment.set_value(pending as f64);
                return;
            }
            self.pending_seek_ms.set(None);
        }

        let upper = duration_ms.map(|value| value as f64).unwrap_or(0.0);
        if upper > 0.0 && (self.seek_adjustment.upper() - upper).abs() > 1.0 {
            self.seek_adjustment.set_upper(upper);
        }

        let value = if upper > 0.0 {
            (current_ms as f64).clamp(0.0, upper)
        } else {
            current_ms.max(0) as f64
        };

        if (self.seek_adjustment.value() - value).abs() > 50.0 {
            self.seek_adjustment.set_value(value);
        }
    }
}
