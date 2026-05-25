use std::cell::Cell;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::rc::Rc;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use gstreamer as gst;
use gstreamer::bus::BusWatchGuard;
use gstreamer::prelude::*;
use gstreamer::prelude::ObjectExt;
use gstreamer_video as gst_video;
use gstreamer_video::prelude::VideoOverlayExtManual;

use crate::models::{StreamSpec, VideoPosition};
use crate::player::events::PlaybackEvents;

pub struct PlaybackEngine {
    pipeline: Option<gst::Pipeline>,
    volume_element: Option<gst::Element>,
    events: PlaybackEvents,
    playing: bool,
    buffering: bool,
    position_timer: Option<gst::glib::SourceId>,
    bus_watch: Option<BusWatchGuard>,
    suppress_errors: Rc<Cell<bool>>,
    last_error: Arc<Mutex<Option<String>>>,
    shared: Arc<Mutex<PlaybackSnapshot>>,
}

#[derive(Default, Clone)]
struct PlaybackSnapshot {
    position_ms: u64,
    duration_ms: Option<u64>,
    playing: bool,
    buffering: bool,
}

impl PlaybackEngine {
    pub fn new(events: PlaybackEvents) -> Self {
        Self {
            pipeline: None,
            volume_element: None,
            events,
            playing: false,
            buffering: false,
            position_timer: None,
            bus_watch: None,
            suppress_errors: Rc::new(Cell::new(false)),
            last_error: Arc::new(Mutex::new(None)),
            shared: Arc::new(Mutex::new(PlaybackSnapshot::default())),
        }
    }

    pub fn stop(&mut self) {
        self.stop_internal();
    }

    pub fn load(&mut self, spec: StreamSpec, video_sink: gst::Element) -> Result<(), String> {
        ensure_gstreamer()?;

        let pipeline = match spec {
            StreamSpec::Progressive { uri } => build_progressive_pipeline(&uri, video_sink)?,
            StreamSpec::Adaptive {
                video_uri,
                audio_uri,
            } => build_adaptive_pipeline(&video_uri, &audio_uri, video_sink)?,
        };

        let pipeline_weak = pipeline.downgrade();
        let bus = pipeline
            .bus()
            .ok_or_else(|| "GStreamer pipeline has no bus".to_string())?;
        let app = self.events.clone();
        let shared = Arc::clone(&self.shared);
        let last_error = Arc::clone(&self.last_error);
        if let Ok(mut slot) = last_error.lock() {
            *slot = None;
        }
        let playing_flag = Rc::new(Cell::new(false));
        let buffering_flag = Rc::new(Cell::new(false));
        let playing_for_bus = Rc::clone(&playing_flag);
        let buffering_for_bus = Rc::clone(&buffering_flag);

        let suppress_errors = Rc::clone(&self.suppress_errors);

        let bus_watch = bus
            .add_watch_local({
                let suppress_errors = Rc::clone(&suppress_errors);
                let last_error = Arc::clone(&last_error);
                move |_, message| {
                    if suppress_errors.get() {
                        return gst::glib::ControlFlow::Continue;
                    }
                    if catch_unwind(AssertUnwindSafe(|| {
                        handle_bus_message(
                            message,
                            &app,
                            &pipeline_weak,
                            &playing_for_bus,
                            &buffering_for_bus,
                            &shared,
                            &last_error,
                        );
                    }))
                    .is_err()
                    {
                        eprintln!("kpopmvlyrics: GStreamer bus handler panicked");
                    }

                    gst::glib::ControlFlow::Continue
                }
            })
            .map_err(|err| err.to_string())?;

        pipeline
            .set_state(gst::State::Ready)
            .map_err(|err| format_state_error(&pipeline, &self.last_error, err))?;

        self.pipeline = Some(pipeline.clone());
        self.volume_element = pipeline
            .by_name("playbin")
            .or_else(|| pipeline.by_name("audio-volume"));
        self.bus_watch = Some(bus_watch);
        self.suppress_errors.set(false);
        self.playing = false;
        self.buffering = false;
        self.start_position_timer();
        Ok(())
    }

    pub fn play(&mut self) -> Result<(), String> {
        let Some(pipeline) = self.pipeline.as_ref() else {
            return Err("No video loaded".into());
        };
        pipeline
            .set_state(gst::State::Playing)
            .map_err(|err| format_state_error(pipeline, &self.last_error, err))?;
        self.playing = true;
        self.sync_snapshot(true, self.buffering);
        Ok(())
    }

    pub fn pause(&mut self) -> Result<(), String> {
        let Some(pipeline) = self.pipeline.as_ref() else {
            return Ok(());
        };
        pipeline
            .set_state(gst::State::Paused)
            .map_err(|err| err.to_string())?;
        self.playing = false;
        self.sync_snapshot(false, self.buffering);
        Ok(())
    }

    pub fn seek(&mut self, ms: u64) -> Result<(), String> {
        let Some(pipeline) = self.pipeline.as_ref() else {
            return Ok(());
        };

        let _ = pipeline.state(Some(gst::ClockTime::from_seconds(2)));
        let flags = gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE;
        let position = gst::ClockTime::from_mseconds(ms);
        let target = seek_target(pipeline);

        if target.seek_simple(flags, position).is_ok() {
            if let Ok(mut snapshot) = self.shared.lock() {
                snapshot.position_ms = ms;
            }
            self.sync_snapshot(self.playing, self.buffering);
            return Ok(());
        }

        Err("Failed to seek".to_string())
    }

    pub fn replay(&mut self) -> Result<(), String> {
        let Some(pipeline) = self.pipeline.as_ref() else {
            return Err("No video loaded".into());
        };

        let _ = pipeline.set_state(gst::State::Paused);
        let target = seek_target(pipeline);
        let flags = gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE;
        target
            .seek_simple(flags, gst::ClockTime::ZERO)
            .map_err(|_| "Failed to seek to start".to_string())?;

        if let Ok(mut snapshot) = self.shared.lock() {
            snapshot.position_ms = 0;
            snapshot.playing = false;
        }
        self.playing = false;
        self.sync_snapshot(false, self.buffering);

        pipeline
            .set_state(gst::State::Playing)
            .map_err(|err| format_state_error(pipeline, &self.last_error, err))?;
        self.playing = true;
        self.sync_snapshot(true, self.buffering);
        Ok(())
    }

    pub fn set_volume(&mut self, level: f64) -> Result<(), String> {
        let volume = level.clamp(0.0, 1.0);
        if let Some(element) = self.volume_element.as_ref() {
            element.set_property("volume", volume);
        }
        Ok(())
    }

    pub fn snapshot(&self) -> VideoPosition {
        self.shared
            .lock()
            .map(|value| VideoPosition {
                ms: value.position_ms,
                duration_ms: value.duration_ms,
                playing: value.playing,
                buffering: value.buffering,
            })
            .unwrap_or(VideoPosition {
                ms: 0,
                duration_ms: None,
                playing: false,
                buffering: false,
            })
    }

    fn sync_snapshot(&self, playing: bool, buffering: bool) {
        if let Ok(mut snapshot) = self.shared.lock() {
            snapshot.playing = playing;
            snapshot.buffering = buffering;
        }
    }

    fn start_position_timer(&mut self) {
        if let Some(source) = self.position_timer.take() {
            source.remove();
        }

        let app = self.events.clone();
        let shared = Arc::clone(&self.shared);
        let pipeline_weak = self
            .pipeline
            .as_ref()
            .map(|pipeline| pipeline.downgrade());

        let source = gst::glib::timeout_add_local(Duration::from_millis(100), move || {
            if catch_unwind(AssertUnwindSafe(|| {
                emit_position_update(
                    pipeline_weak.as_ref(),
                    &shared,
                    &app,
                );
            }))
            .is_err()
            {
                eprintln!("kpopmvlyrics: GStreamer position timer panicked");
            }

            gst::glib::ControlFlow::Continue
        });

        self.position_timer = Some(source);
    }

    fn stop_internal(&mut self) {
        if let Some(source) = self.position_timer.take() {
            source.remove();
        }
        self.bus_watch.take();
        self.volume_element = None;
        self.suppress_errors.set(true);
        if let Some(pipeline) = self.pipeline.take() {
            let _ = pipeline.set_state(gst::State::Null);
        }
        self.suppress_errors.set(false);
        if let Ok(mut snapshot) = self.shared.lock() {
            *snapshot = PlaybackSnapshot::default();
        }
    }
}

impl Drop for PlaybackEngine {
    fn drop(&mut self) {
        self.stop_internal();
    }
}

fn handle_bus_message(
    message: &gst::Message,
    app: &PlaybackEvents,
    pipeline_weak: &gst::glib::WeakRef<gst::Pipeline>,
    playing_for_bus: &Cell<bool>,
    buffering_for_bus: &Cell<bool>,
    shared: &Arc<Mutex<PlaybackSnapshot>>,
    last_error: &Arc<Mutex<Option<String>>>,
) {
    use gst::MessageView;

    match message.view() {
        MessageView::Error(error) => {
            let detail = error
                .debug()
                .map(|value| value.to_string())
                .unwrap_or_else(|| "Unknown playback error".to_string());
            let src = message
                .src()
                .map(|src| src.name().to_string())
                .unwrap_or_else(|| "pipeline".to_string());
            let formatted = format!("{src}: {} ({detail})", error.error());
            if let Ok(mut slot) = last_error.lock() {
                *slot = Some(formatted.clone());
            }
            if let Some(handler) = &app.on_error {
                handler(formatted);
            }
            playing_for_bus.set(false);
            buffering_for_bus.set(false);
        }
        MessageView::Eos(_) => {
            playing_for_bus.set(false);
        }
        MessageView::Buffering(buffering) => {
            buffering_for_bus.set(buffering.percent() < 100);
        }
        MessageView::StateChanged(state) => {
            if state
                .src()
                .and_then(|src| {
                    pipeline_weak
                        .upgrade()
                        .map(|pipeline| src == pipeline.upcast_ref::<gst::Object>())
                })
                .unwrap_or(false)
            {
                let playing = state.current() == gst::State::Playing;
                playing_for_bus.set(playing);
            }
        }
        _ => {}
    }

    if let Ok(mut snapshot) = shared.lock() {
        snapshot.playing = playing_for_bus.get();
        snapshot.buffering = buffering_for_bus.get();
    }
}

fn emit_position_update(
    pipeline_weak: Option<&gst::glib::WeakRef<gst::Pipeline>>,
    shared: &Arc<Mutex<PlaybackSnapshot>>,
    app: &PlaybackEvents,
) {
    let mut position_ms = 0;
    let mut duration_ms = None;
    let mut playing = false;
    let mut buffering = false;

    if let Some(pipeline) = pipeline_weak.and_then(|weak| weak.upgrade()) {
        if let Some(clock) = pipeline.clock() {
            if let Some(base) = pipeline.base_time() {
                if let Some(position) = pipeline.query_position::<gst::ClockTime>() {
                    position_ms = position.mseconds();
                }
                if let Some(duration) = pipeline.query_duration::<gst::ClockTime>() {
                    duration_ms = Some(duration.mseconds());
                }
                let _ = (clock, base);
            }
        }
    }

    if let Ok(mut snapshot) = shared.lock() {
        snapshot.position_ms = position_ms;
        snapshot.duration_ms = duration_ms;
        playing = snapshot.playing;
        buffering = snapshot.buffering;
    }

    if let Some(handler) = &app.on_position {
        handler(VideoPosition {
            ms: position_ms,
            duration_ms,
            playing,
            buffering,
        });
    }
}

fn build_progressive_pipeline(uri: &str, video_sink: gst::Element) -> Result<gst::Pipeline, String> {
    let playbin = gst::ElementFactory::make("playbin3")
        .name("playbin")
        .build()
        .map_err(|err| err.to_string())?;
    playbin.set_property("uri", uri);
    playbin.set_property("video-sink", &video_sink);

    playbin
        .dynamic_cast::<gst::Pipeline>()
        .map_err(|_| "playbin3 could not be used as a pipeline".to_string())
}

fn add_to_pipeline(
    pipeline: &gst::Pipeline,
    element: &gst::Element,
    label: &str,
) -> Result<(), String> {
    pipeline
        .add(element)
        .map_err(|err| format!("Failed to add element '{label}': {err}"))
}

fn seek_target(pipeline: &gst::Pipeline) -> gst::Element {
    pipeline
        .by_name("playbin")
        .unwrap_or_else(|| pipeline.clone().upcast::<gst::Element>())
}

fn format_state_error(
    pipeline: &gst::Pipeline,
    last_error: &Arc<Mutex<Option<String>>>,
    err: impl std::fmt::Display,
) -> String {
    if let Ok(slot) = last_error.lock() {
        if let Some(message) = slot.as_ref() {
            return message.clone();
        }
    }

    format!(
        "{err} ({})",
        element_state_diagnostics(pipeline)
    )
}

fn element_state_diagnostics(pipeline: &gst::Pipeline) -> String {
    let mut details = Vec::new();
    let mut iter = pipeline.iterate_recurse();
    loop {
        match iter.next() {
            Ok(Some(element)) => {
                let (result, state, pending) = element.state(gst::ClockTime::ZERO);
                if result.is_err() {
                    details.push(format!(
                        "{}: state={state:?}, pending={pending:?}, result={result:?}",
                        element.name()
                    ));
                }
            }
            Ok(None) => break,
            Err(_) => break,
        }
    }

    if details.is_empty() {
        "no element state details".to_string()
    } else {
        details.join("; ")
    }
}

fn wait_for_state(
    pipeline: &gst::Pipeline,
    target: gst::State,
    timeout: gst::ClockTime,
) -> Result<(), String> {
    let (result, state, pending) = pipeline.state(Some(timeout));
    if let Err(err) = result {
        return Err(err.to_string());
    }
    if state < target {
        return Err(format!(
            "Timed out waiting for pipeline to reach {target:?} (currently {state:?}, pending {pending:?})"
        ));
    }
    Ok(())
}

fn build_adaptive_pipeline(
    video_uri: &str,
    audio_uri: &str,
    video_sink: gst::Element,
) -> Result<gst::Pipeline, String> {
    let pipeline = gst::Pipeline::default();

    let video_decode = gst::ElementFactory::make("uridecodebin")
        .name("video-decode")
        .property("uri", video_uri)
        .build()
        .map_err(|err| err.to_string())?;
    let audio_decode = gst::ElementFactory::make("uridecodebin")
        .name("audio-decode")
        .property("uri", audio_uri)
        .build()
        .map_err(|err| err.to_string())?;
    let video_queue = gst::ElementFactory::make("queue").build().map_err(|err| err.to_string())?;
    let audio_queue = gst::ElementFactory::make("queue").build().map_err(|err| err.to_string())?;
    let video_convert = gst::ElementFactory::make("videoconvert")
        .build()
        .map_err(|err| err.to_string())?;
    let audio_convert = gst::ElementFactory::make("audioconvert")
        .build()
        .map_err(|err| err.to_string())?;
    let audio_resample = gst::ElementFactory::make("audioresample")
        .build()
        .map_err(|err| err.to_string())?;
    let volume = gst::ElementFactory::make("volume")
        .name("audio-volume")
        .build()
        .map_err(|err| err.to_string())?;
    let audio_sink = gst::ElementFactory::make("autoaudiosink")
        .build()
        .map_err(|err| err.to_string())?;

    for (element, label) in [
        (&video_decode, "video decoder"),
        (&video_queue, "video queue"),
        (&video_convert, "video convert"),
        (&video_sink, "video sink"),
        (&audio_decode, "audio decoder"),
        (&audio_queue, "audio queue"),
        (&audio_convert, "audio convert"),
        (&audio_resample, "audio resample"),
        (&volume, "audio volume"),
        (&audio_sink, "audio sink"),
    ] {
        add_to_pipeline(&pipeline, element, label)?;
    }

    video_queue
        .link(&video_convert)
        .map_err(|err| err.to_string())?;
    video_convert
        .link(&video_sink)
        .map_err(|err| err.to_string())?;
    audio_queue
        .link(&audio_convert)
        .map_err(|err| err.to_string())?;
    audio_convert
        .link(&audio_resample)
        .map_err(|err| err.to_string())?;
    audio_resample
        .link(&volume)
        .map_err(|err| err.to_string())?;
    volume
        .link(&audio_sink)
        .map_err(|err| err.to_string())?;

    connect_decodebin_to_queue(&video_decode, &video_queue, "video/");
    connect_decodebin_to_queue(&audio_decode, &audio_queue, "audio/");

    Ok(pipeline)
}

fn connect_decodebin_to_queue(decode: &gst::Element, queue: &gst::Element, prefix: &'static str) {
    let queue_weak = queue.downgrade();
    decode.connect_pad_added(move |_decode, pad| {
        let Some(caps) = pad.current_caps().or_else(|| pad.allowed_caps()) else {
            return;
        };
        let Some(structure) = caps.structure(0) else {
            return;
        };
        if !structure.name().starts_with(prefix) {
            return;
        }
        let Some(queue) = queue_weak.upgrade() else {
            return;
        };
        let Some(sink_pad) = queue.static_pad("sink") else {
            return;
        };
        if sink_pad.is_linked() {
            return;
        }
        if let Err(err) = pad.link(&sink_pad) {
            eprintln!("kpopmvlyrics: failed to link {prefix} decode pad: {err}");
        }
    });
}

pub fn set_video_overlay_handle(sink: &gst::Element, handle: usize) -> Result<(), String> {
    if let Ok(video_sink) = sink.clone().dynamic_cast::<gst_video::VideoOverlay>() {
        unsafe {
            video_sink.set_window_handle(handle);
        }
        return Ok(());
    }

    if let Some(video_sink) = find_video_overlay(sink) {
        unsafe {
            video_sink.set_window_handle(handle);
        }
        return Ok(());
    }

    Err("Video sink does not support window embedding".into())
}

fn find_video_overlay(element: &gst::Element) -> Option<gst_video::VideoOverlay> {
    if let Ok(overlay) = element.clone().dynamic_cast::<gst_video::VideoOverlay>() {
        return Some(overlay);
    }

    if let Ok(bin) = element.clone().dynamic_cast::<gst::Bin>() {
        let mut iter = bin.iterate_recurse();
        loop {
            match iter.next() {
                Ok(Some(child)) => {
                    if let Some(overlay) = find_video_overlay(&child) {
                        return Some(overlay);
                    }
                }
                Ok(None) => break,
                Err(_) => break,
            }
        }
    }

    None
}

pub(crate) fn ensure_gstreamer() -> Result<(), String> {
    static GST_INIT: OnceLock<Result<(), String>> = OnceLock::new();
    GST_INIT
        .get_or_init(|| {
            gst::init().map_err(|err| err.to_string())?;
            configure_hardware_decoders();
            Ok(())
        })
        .clone()
}

fn configure_hardware_decoders() {
    let boost_rank = gst::Rank::PRIMARY + 100;

    const HARDWARE_DECODERS: &[&str] = &[
        "vah264dec",
        "vah265dec",
        "vavp9dec",
        "vaav1dec",
        "nvh264dec",
        "nvh265dec",
        "nvvp9dec",
        "nvav1dec",
        "nvvp8dec",
        "vaapih264dec",
        "vaapivp9dec",
        "vaapih265dec",
    ];

    for name in HARDWARE_DECODERS {
        if let Some(factory) = gst::ElementFactory::find(name) {
            factory.set_rank(boost_rank);
        }
    }
}
