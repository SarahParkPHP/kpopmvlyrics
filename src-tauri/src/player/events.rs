use std::rc::Rc;

use crate::models::VideoPosition;

#[derive(Clone, Default)]
pub struct PlaybackEvents {
    pub on_position: Option<Rc<dyn Fn(VideoPosition)>>,
    pub on_error: Option<Rc<dyn Fn(String)>>,
}

impl PlaybackEvents {
    /// Canonical, UI-agnostic constructor: every native frontend (GTK4, SwiftUI
    /// via UniFFI, WinUI 3, Qt) supplies its own position/error sinks here.
    /// Consumed by the reusable `NativePlayer` (macOS/Qt/WinUI frontends).
    #[cfg(native_frontend)]
    pub fn from_callbacks(
        on_position: impl Fn(VideoPosition) + 'static,
        on_error: impl Fn(String) + 'static,
    ) -> Self {
        Self {
            on_position: Some(Rc::new(on_position)),
            on_error: Some(Rc::new(on_error)),
        }
    }
}

#[cfg(tauri_shell)]
impl PlaybackEvents {
    pub fn from_tauri(app: tauri::AppHandle) -> Self {
        use tauri::Emitter;

        let on_error = {
            let app = app.clone();
            Some(Rc::new(move |message: String| {
                let _ = app.emit("video-player-error", message);
            }))
        };
        let on_position = {
            let app = app.clone();
            Some(Rc::new(move |position: VideoPosition| {
                let _ = app.emit("video-position", position);
            }))
        };
        Self {
            on_position,
            on_error,
        }
    }
}
