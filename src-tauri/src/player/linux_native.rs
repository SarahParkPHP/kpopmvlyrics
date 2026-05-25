use gtk::prelude::*;
use gstreamer as gst;
use gstreamer::prelude::Cast;
use gstreamer::prelude::ObjectExt;
use crate::models::{StreamSpec, VideoPosition};
use crate::player::events::PlaybackEvents;
use crate::player::pipeline::{ensure_gstreamer, set_video_overlay_handle, PlaybackEngine};

pub struct NativeLinuxPlayer {
    engine: PlaybackEngine,
    video_box: gtk::Box,
    video_sink: Option<gst::Element>,
    uses_embedded_widget: bool,
}

impl NativeLinuxPlayer {
    pub fn new(events: PlaybackEvents) -> Self {
        Self {
            engine: PlaybackEngine::new(events),
            video_box: gtk::Box::new(gtk::Orientation::Vertical, 0),
            video_sink: None,
            uses_embedded_widget: false,
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

    pub fn seek(&mut self, ms: u64) -> Result<(), String> {
        self.engine.seek(ms)
    }

    pub fn snapshot(&self) -> VideoPosition {
        self.engine.snapshot()
    }

    fn release_video_sink(&mut self) {
        self.video_sink.take();
        for child in self.video_box.children() {
            self.video_box.remove(&child);
        }
        self.uses_embedded_widget = false;
    }

    fn build_video_sink(&mut self) -> Result<gst::Element, String> {
        let sink = create_platform_sink()?;
        configure_video_sink(&sink);
        if let Ok(widget) = gtk_widget_from_gtk_sink(&sink) {
            self.video_box.pack_start(&widget, true, true, 0);
            widget.show_all();
            self.video_box.show_all();
            self.uses_embedded_widget = true;
        } else if let Some(window) = self.video_box.window() {
            set_video_overlay_handle(sink.upcast_ref(), window.as_ptr() as usize)?;
        }
        Ok(sink)
    }
}

fn configure_video_sink(sink: &gst::Element) {
    sink.set_property("async", false);
}

fn create_platform_sink() -> Result<gst::Element, String> {
    if is_wayland_session() {
        gst::ElementFactory::make("gtksink")
            .build()
            .or_else(|_| gst::ElementFactory::make("waylandsink").build())
            .map_err(|err| {
                format!("Could not create a Wayland video sink (install gst-plugin-gtk): {err}")
            })
    } else {
        gst::ElementFactory::make("gtksink")
            .build()
            .or_else(|_| gst::ElementFactory::make("glimagesink").build())
            .or_else(|_| gst::ElementFactory::make("xvimagesink").build())
            .or_else(|_| gst::ElementFactory::make("autovideosink").build())
            .map_err(|err| format!("Could not create an X11 video sink: {err}"))
    }
}

fn gtk_widget_from_gtk_sink(sink: &gst::Element) -> Result<gtk::Widget, String> {
    use gstreamer::glib::translate::ToGlibPtr;
    use gstreamer::prelude::ObjectExt;
    use gtk::glib::translate::FromGlibPtrNone;

    let object: gstreamer::glib::Object = sink.property("widget");
    let widget_ptr: *mut gtk::ffi::GtkWidget = {
        let stash = ToGlibPtr::<*mut gstreamer::glib::gobject_ffi::GObject>::to_glib_none(&object);
        stash.0 as *mut gtk::ffi::GtkWidget
    };
    if widget_ptr.is_null() {
        return Err("gtksink returned a null widget".to_string());
    }
    // SAFETY: gtksink's widget property is a valid GtkWidget pointer.
    Ok(unsafe { gtk::Widget::from_glib_none(widget_ptr) })
}

fn is_wayland_session() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some()
}
