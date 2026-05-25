use std::rc::Rc;

use crate::models::VideoPosition;

#[derive(Clone, Default)]
pub struct PlaybackEvents {
    pub on_position: Option<Rc<dyn Fn(VideoPosition)>>,
    pub on_error: Option<Rc<dyn Fn(String)>>,
}

#[cfg(not(target_os = "linux"))]
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
