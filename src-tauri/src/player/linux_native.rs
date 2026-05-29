use gdk_pixbuf as _; // ensure crate is linked
use gstreamer as gst;
use gstreamer::prelude::ObjectExt;
use gtk::gdk;
use gtk::prelude::*;

use crate::models::{StreamSpec, VideoPosition};
use crate::player::events::PlaybackEvents;
use crate::player::pipeline::{ensure_gstreamer, PlaybackEngine};

pub struct NativeLinuxPlayer {
    engine: PlaybackEngine,
    video_box: gtk::Box,
    picture: gtk::Picture,
    video_sink: Option<gst::Element>,
}

impl NativeLinuxPlayer {
    pub fn new(events: PlaybackEvents) -> Self {
        let video_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        video_box.set_hexpand(true);
        video_box.set_vexpand(true);
        let picture = gtk::Picture::new();
        picture.set_hexpand(true);
        picture.set_vexpand(true);
        picture.set_content_fit(gtk::ContentFit::Contain);
        video_box.append(&picture);
        Self {
            engine: PlaybackEngine::new(events),
            video_box,
            picture,
            video_sink: None,
        }
    }

    pub fn video_widget(&self) -> &gtk::Box {
        &self.video_box
    }

    pub fn load(&mut self, spec: StreamSpec) -> Result<(), String> {
        ensure_gstreamer()?;
        self.engine.stop();
        self.release_video_sink();
        let sink = self.build_video_sink()?;
        self.video_sink = Some(sink.clone());
        self.engine.load(spec, sink)
    }

    pub fn play(&mut self) -> Result<(), String> {
        self.engine.play()
    }

    pub fn pause(&mut self) -> Result<(), String> {
        self.engine.pause()
    }

    pub fn replay(&mut self) -> Result<(), String> {
        self.engine.replay()
    }

    pub fn set_volume(&mut self, level: f64) -> Result<(), String> {
        self.engine.set_volume(level)
    }

    pub fn seek(&mut self, ms: u64) -> Result<(), String> {
        self.engine.seek(ms)
    }

    pub fn snapshot(&self) -> VideoPosition {
        self.engine.snapshot()
    }

    fn release_video_sink(&mut self) {
        self.video_sink.take();
        self.picture.set_paintable(None::<&gdk::Paintable>);
    }

    fn build_video_sink(&mut self) -> Result<gst::Element, String> {
        let sink = create_platform_sink()?;
        configure_video_sink(&sink);
        if let Ok(paintable) = paintable_from_sink(&sink) {
            self.picture.set_paintable(Some(&paintable));
        }
        Ok(sink)
    }
}

fn configure_video_sink(sink: &gst::Element) {
    if sink.has_property("sync") {
        sink.set_property("sync", true);
    }
    if sink.has_property("async") {
        sink.set_property("async", true);
    }
}

fn create_platform_sink() -> Result<gst::Element, String> {
    // gtk4paintablesink (from gst-plugin-gtk4) works on both X11 and Wayland and
    // hands us a GdkPaintable we can attach to a gtk::Picture.
    gst::ElementFactory::make("gtk4paintablesink")
        .name("video-sink")
        .build()
        .or_else(|_| {
            gst::ElementFactory::make("autovideosink")
                .name("video-sink")
                .build()
        })
        .map_err(|err| {
            format!("Could not create a video sink (install gst-plugin-gtk4): {err}")
        })
}

fn paintable_from_sink(sink: &gst::Element) -> Result<gdk::Paintable, String> {
    if !sink.has_property("paintable") {
        return Err("sink does not expose a paintable property".to_string());
    }
    Ok(sink.property::<gdk::Paintable>("paintable"))
}
