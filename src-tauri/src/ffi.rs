//! UniFFI boundary for the native macOS (SwiftUI) frontend.
//!
//! Unlike the Rust frontends (GTK4, and the planned WinUI 3 / Qt-QML), SwiftUI
//! cannot call [`AppContext`] in-process, so this module exposes a small
//! UniFFI-generated surface. Structured data crosses as serde JSON strings —
//! the same `invoke(name, args) -> json` shape the codebase already used for
//! Tauri — which keeps `models.rs` free of FFI-specific derives (UniFFI does
//! not support the `usize` fields those models carry) and lets the Swift side
//! decode with `Codable` against the existing camelCase JSON.

use std::sync::{Arc, Mutex};

use crate::app::AppContext;
use crate::player::NativePlayer;

/// Video sinks the macOS player prefers, in order.
const MACOS_SINKS: &[&str] = &["osxvideosink", "glimagesink"];

/// Shared slot for the foreign player-event observer. Cloned into the player
/// thread's event closures so it can deliver `onPosition`/`onError`.
type SharedObserver = Arc<Mutex<Option<Box<dyn PlaybackObserver>>>>;

/// Error surfaced across the FFI boundary. Maps to a thrown error in Swift.
#[derive(Debug, uniffi::Error)]
pub enum CoreError {
    Failed { message: String },
}

impl std::fmt::Display for CoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CoreError::Failed { message } => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for CoreError {}

impl From<String> for CoreError {
    fn from(message: String) -> Self {
        CoreError::Failed { message }
    }
}

/// Call the registered observer, if any, holding the lock only briefly.
fn notify(observer: &SharedObserver, call: impl FnOnce(&dyn PlaybackObserver)) {
    if let Ok(guard) = observer.lock() {
        if let Some(obs) = guard.as_ref() {
            call(obs.as_ref());
        }
    }
}

/// Receives player events on the foreign (Swift) side. Payloads are JSON to
/// mirror the former `video-position` / `video-player-error` Tauri events.
#[uniffi::export(callback_interface)]
pub trait PlaybackObserver: Send + Sync {
    fn on_position(&self, position_json: String);
    fn on_error(&self, message: String);
}

/// The single object the SwiftUI app holds for the lifetime of the process.
#[derive(uniffi::Object)]
pub struct Core {
    ctx: Arc<AppContext>,
    observer: SharedObserver,
    player: NativePlayer,
}

#[uniffi::export]
impl Core {
    /// Open the database-backed application context and start the player thread.
    #[uniffi::constructor]
    pub fn new() -> Result<Arc<Self>, CoreError> {
        let ctx = AppContext::open()?;
        let observer: SharedObserver = Arc::new(Mutex::new(None));
        let position_observer = observer.clone();
        let error_observer = observer.clone();
        let player = NativePlayer::spawn(
            MACOS_SINKS.to_vec(),
            move |position_json| {
                notify(&position_observer, |obs| obs.on_position(position_json));
            },
            move |message| {
                notify(&error_observer, |obs| obs.on_error(message));
            },
        );
        Ok(Arc::new(Self {
            ctx: Arc::new(ctx),
            observer,
            player,
        }))
    }

    /// Register (or replace) the foreign player-event observer.
    pub fn set_observer(&self, observer: Box<dyn PlaybackObserver>) {
        *self.observer.lock().expect("observer mutex poisoned") = Some(observer);
    }

    /// Dispatch a command by name. `args_json` is a JSON object of arguments
    /// (camelCase keys); the result is the command's return value as JSON.
    pub fn invoke(&self, command: String, args_json: String) -> Result<String, CoreError> {
        crate::command::invoke(&self.ctx, &command, &args_json).map_err(CoreError::from)
    }

    /// Bind the video to a native surface (an `NSView` pointer) and position it.
    pub fn player_attach_surface(
        &self,
        handle: u64,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
    ) -> Result<(), CoreError> {
        Ok(self.player.attach_surface(handle, x, y, width, height)?)
    }

    /// Load a stream. `stream_spec_json` is a `StreamSpec` as returned by the
    /// `resolve_stream` command, so the caller resolves the URL then loads it.
    pub fn player_load(&self, stream_spec_json: String) -> Result<(), CoreError> {
        Ok(self.player.load_json(&stream_spec_json)?)
    }

    pub fn player_play(&self) -> Result<(), CoreError> {
        Ok(self.player.play()?)
    }

    pub fn player_pause(&self) -> Result<(), CoreError> {
        Ok(self.player.pause()?)
    }

    pub fn player_seek(&self, ms: u64) -> Result<(), CoreError> {
        Ok(self.player.seek(ms)?)
    }

    /// Current playback position/state as a `VideoPosition` JSON object.
    pub fn player_snapshot(&self) -> Result<String, CoreError> {
        Ok(self.player.snapshot_json())
    }
}

#[cfg(test)]
mod tests {
    use super::CoreError;

    #[test]
    fn core_error_displays_message() {
        let error = CoreError::Failed {
            message: "boom".to_string(),
        };
        assert_eq!(error.to_string(), "boom");
    }
}
