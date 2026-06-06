//! Reusable GStreamer/GLib player thread for the non-GTK native frontends
//! (macOS/UniFFI, Qt/QML, WinUI 3).
//!
//! [`PlaybackEngine`] is `!Send` (it holds `Rc` state and a glib `SourceId`
//! bound to a thread-default `MainContext`), so it lives on its own thread that
//! owns a `glib::MainContext` and runs a `MainLoop`; the UI sends commands over
//! a channel and they are applied on that thread. Frontends differ only in the
//! video sink they request and the native window handle they attach.
//!
//! NOTE: the video sink binds to the window handle handed to [`NativePlayer::attach_surface`]
//! (an `NSView` on macOS, an `HWND` on Windows, a `QWindow`/`xcb`/`wl` handle on
//! Qt). Such bindings are typically UI-main-thread-sensitive, so the surface
//! wiring needs on-device validation per platform.

use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::sync::Mutex;
use std::thread::JoinHandle;
use std::time::Duration;

use gstreamer as gst;
use gstreamer::glib;

use crate::models::{StreamSpec, VideoPosition};
use crate::player::{
    ensure_gstreamer, set_video_overlay_handle, set_video_overlay_rectangle, PlaybackEngine,
    PlaybackEvents,
};

/// Where the video should be drawn inside the host surface (logical pixels).
#[derive(Clone, Copy)]
pub struct SurfaceRect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

enum Command {
    AttachSurface {
        handle: usize,
        rect: SurfaceRect,
    },
    Load {
        spec: StreamSpec,
        reply: Sender<Result<(), String>>,
    },
    Play(Sender<Result<(), String>>),
    Pause(Sender<Result<(), String>>),
    Seek {
        ms: u64,
        reply: Sender<Result<(), String>>,
    },
    Snapshot(Sender<VideoPosition>),
    Shutdown,
}

/// `Send + Sync` handle a frontend holds; forwards calls to the player thread.
pub struct NativePlayer {
    tx: Mutex<Sender<Command>>,
    join: Mutex<Option<JoinHandle<()>>>,
}

impl NativePlayer {
    /// Spawn the player thread. `sink_candidates` are tried in order until one
    /// builds (e.g. `["osxvideosink", "glimagesink"]`). Event callbacks receive
    /// JSON (a `VideoPosition` object / an error message) and run on the player
    /// thread, so frontends must marshal to their UI thread as needed.
    pub fn spawn(
        sink_candidates: Vec<&'static str>,
        on_position_json: impl Fn(String) + Send + 'static,
        on_error: impl Fn(String) + Send + 'static,
    ) -> Self {
        let (tx, rx) = channel::<Command>();
        let join = std::thread::Builder::new()
            .name("kpml-player".into())
            .spawn(move || player_thread_main(rx, sink_candidates, on_position_json, on_error))
            .expect("spawn player thread");
        Self {
            tx: Mutex::new(tx),
            join: Mutex::new(Some(join)),
        }
    }

    fn send(&self, command: Command) -> Result<(), String> {
        self.tx
            .lock()
            .map_err(|_| "player channel poisoned".to_string())?
            .send(command)
            .map_err(|_| "player thread is not running".to_string())
    }

    fn request<T>(&self, make: impl FnOnce(Sender<T>) -> Command) -> Result<T, String> {
        let (reply_tx, reply_rx) = channel::<T>();
        self.send(make(reply_tx))?;
        reply_rx
            .recv()
            .map_err(|_| "player thread dropped the reply".to_string())
    }

    pub fn attach_surface(
        &self,
        handle: u64,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
    ) -> Result<(), String> {
        self.send(Command::AttachSurface {
            handle: handle as usize,
            rect: SurfaceRect {
                x,
                y,
                width,
                height,
            },
        })
    }

    pub fn load(&self, spec: StreamSpec) -> Result<(), String> {
        self.request(|reply| Command::Load { spec, reply })?
    }

    /// Load from a `StreamSpec` JSON string (as returned by `resolve_stream`),
    /// so frontends can stay JSON-only and never touch model types.
    pub fn load_json(&self, stream_spec_json: &str) -> Result<(), String> {
        let spec: StreamSpec = serde_json::from_str(stream_spec_json)
            .map_err(|err| format!("invalid stream spec: {err}"))?;
        self.load(spec)
    }

    pub fn play(&self) -> Result<(), String> {
        self.request(Command::Play)?
    }

    pub fn pause(&self) -> Result<(), String> {
        self.request(Command::Pause)?
    }

    pub fn seek(&self, ms: u64) -> Result<(), String> {
        self.request(|reply| Command::Seek { ms, reply })?
    }

    pub fn snapshot(&self) -> VideoPosition {
        self.request(Command::Snapshot).unwrap_or(VideoPosition {
            ms: 0,
            duration_ms: None,
            playing: false,
            buffering: false,
        })
    }

    /// Current position/state as a `VideoPosition` JSON object.
    pub fn snapshot_json(&self) -> String {
        serde_json::to_string(&self.snapshot()).unwrap_or_else(|_| "null".to_string())
    }
}

impl Drop for NativePlayer {
    fn drop(&mut self) {
        let _ = self.send(Command::Shutdown);
        if let Some(join) = self.join.lock().ok().and_then(|mut guard| guard.take()) {
            let _ = join.join();
        }
    }
}

fn player_thread_main(
    rx: Receiver<Command>,
    sink_candidates: Vec<&'static str>,
    on_position_json: impl Fn(String) + Send + 'static,
    on_error: impl Fn(String) + Send + 'static,
) {
    if let Err(err) = ensure_gstreamer() {
        on_error(format!("GStreamer init failed: {err}"));
        return;
    }

    let events = PlaybackEvents::from_callbacks(
        move |position: VideoPosition| {
            if let Ok(json) = serde_json::to_string(&position) {
                on_position_json(json);
            }
        },
        on_error,
    );

    let main_context = glib::MainContext::new();
    let main_loop = glib::MainLoop::new(Some(&main_context), false);
    let run_loop = main_loop.clone();

    let _ = main_context.with_thread_default(|| {
        let mut state = PlayerThread {
            engine: PlaybackEngine::new(events),
            sink: None,
            surface: None,
            sink_candidates,
        };

        // The glib MainLoop blocks; drain the command channel from a timer that
        // shares this thread-default context (so it and the engine's position
        // timer both fire here).
        glib::timeout_add_local(Duration::from_millis(8), move || loop {
            match rx.try_recv() {
                Ok(Command::Shutdown) | Err(TryRecvError::Disconnected) => {
                    state.engine.stop();
                    run_loop.quit();
                    return glib::ControlFlow::Break;
                }
                Ok(command) => state.handle(command),
                Err(TryRecvError::Empty) => return glib::ControlFlow::Continue,
            }
        });

        main_loop.run();
    });
}

struct PlayerThread {
    engine: PlaybackEngine,
    sink: Option<gst::Element>,
    surface: Option<(usize, SurfaceRect)>,
    sink_candidates: Vec<&'static str>,
}

impl PlayerThread {
    fn handle(&mut self, command: Command) {
        match command {
            Command::AttachSurface { handle, rect } => {
                self.surface = Some((handle, rect));
                if let Some(sink) = self.sink.clone() {
                    self.apply_surface(&sink);
                }
            }
            Command::Load { spec, reply } => {
                let _ = reply.send(self.load(spec));
            }
            Command::Play(reply) => {
                let _ = reply.send(self.engine.play());
            }
            Command::Pause(reply) => {
                let _ = reply.send(self.engine.pause());
            }
            Command::Seek { ms, reply } => {
                let _ = reply.send(self.engine.seek(ms));
            }
            Command::Snapshot(reply) => {
                let _ = reply.send(self.engine.snapshot());
            }
            // Shutdown is handled by the polling loop in `player_thread_main`.
            Command::Shutdown => {}
        }
    }

    fn load(&mut self, spec: StreamSpec) -> Result<(), String> {
        let sink = self.ensure_sink()?;
        self.engine.load(spec, sink)
    }

    fn ensure_sink(&mut self) -> Result<gst::Element, String> {
        if let Some(sink) = self.sink.clone() {
            self.apply_surface(&sink);
            return Ok(sink);
        }
        let sink = self
            .sink_candidates
            .iter()
            .find_map(|name| gst::ElementFactory::make(name).build().ok())
            .ok_or_else(|| {
                format!("Could not create a video sink from {:?}", self.sink_candidates)
            })?;
        self.apply_surface(&sink);
        self.sink = Some(sink.clone());
        Ok(sink)
    }

    fn apply_surface(&self, sink: &gst::Element) {
        if let Some((handle, rect)) = self.surface {
            let _ = set_video_overlay_handle(sink, handle);
            let _ = set_video_overlay_rectangle(sink, rect.x, rect.y, rect.width, rect.height);
        }
    }
}
