use std::sync::OnceLock;

use gstreamer_video::prelude::{VideoOverlayExt, VideoOverlayExtManual};
use tauri::{AppHandle, LogicalPosition, LogicalSize, Manager, WebviewWindow};

use crate::models::{StreamSpec, VideoPosition};
use crate::player::events::PlaybackEvents;
use crate::player::pipeline::{set_video_overlay_handle, PlaybackEngine};

const TOP_RATIO: f64 = 0.38;
const WINDOW_LABEL: &str = "main";

pub struct MacPlayer {
    engine: PlaybackEngine,
    app: AppHandle,
    video_sink: Option<gst::Element>,
}

impl MacPlayer {
    pub fn new(app: AppHandle) -> Self {
        Self {
            engine: PlaybackEngine::new(PlaybackEvents::from_tauri(app.clone())),
            app,
            video_sink: None,
        }
    }

    pub fn setup_window(&mut self, window: &WebviewWindow, _app: &AppHandle) -> Result<(), String> {
        layout_webview(&self.app, window)?;
        let window = window.clone();
        let app = self.app.clone();
        window.on_window_event(move |event| {
            if matches!(event, tauri::WindowEvent::Resized(_)) {
                if let Some(current) = app.get_webview_window(WINDOW_LABEL) {
                    let _ = layout_webview(&app, &current);
                }
            }
        });
        Ok(())
    }

    fn ensure_video_sink(&mut self, window: &WebviewWindow) -> Result<gst::Element, String> {
        if let Some(sink) = self.video_sink.clone() {
            update_render_rectangle(window, &sink)?;
            return Ok(sink);
        }

        let sink = gst::ElementFactory::make("osxvideosink")
            .build()
            .or_else(|_| gst::ElementFactory::make("glimagesink").build())
            .map_err(|err| format!("Could not create a macOS video sink: {err}"))?;

        let ns_view = window.ns_view().map_err(|err| err.to_string())?;
        set_video_overlay_handle(&sink, ns_view as usize)?;
        update_render_rectangle(window, &sink)?;
        self.video_sink = Some(sink.clone());
        Ok(sink)
    }

    pub fn load(&mut self, spec: StreamSpec) -> Result<(), String> {
        let window = self
            .app
            .get_webview_window(WINDOW_LABEL)
            .ok_or_else(|| "Main window is unavailable".to_string())?;
        layout_webview(&self.app, &window)?;
        let sink = self.ensure_video_sink(&window)?;
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
}

fn layout_webview(app: &AppHandle, window: &WebviewWindow) -> Result<(), String> {
    let size = window.inner_size().map_err(|err| err.to_string())?;
    let top_height = (size.height as f64 * TOP_RATIO).round() as u32;
    let Some(webview) = app.get_webview(window.label()) else {
        return Ok(());
    };
    webview.set_auto_resize(false).map_err(|err| err.to_string())?;
    webview
        .set_position(LogicalPosition::new(0.0, 0.0))
        .map_err(|err| err.to_string())?;
    webview
        .set_size(LogicalSize::new(size.width as f64, top_height as f64))
        .map_err(|err| err.to_string())?;
    Ok(())
}

fn update_render_rectangle(window: &WebviewWindow, sink: &gst::Element) -> Result<(), String> {
    let size = window.inner_size().map_err(|err| err.to_string())?;
    let top = (size.height as f64 * TOP_RATIO).round() as i32;
    let ns_view = window.ns_view().map_err(|err| err.to_string())?;
    set_video_overlay_handle(sink, ns_view as usize)?;
    if let Ok(overlay) = sink.clone().dynamic_cast::<gstreamer_video::VideoOverlay>() {
        overlay
            .set_render_rectangle(0, top, size.width as i32, size.height as i32 - top)
            .map_err(|err| err.to_string())?;
    }
    Ok(())
}

static GST_INIT: OnceLock<Result<(), String>> = OnceLock::new();

pub fn ensure_gstreamer() -> Result<(), String> {
    GST_INIT
        .get_or_init(|| gstreamer::init().map_err(|err| err.to_string()))
        .clone()
}
