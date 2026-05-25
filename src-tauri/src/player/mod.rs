mod events;
mod pipeline;

pub use events::PlaybackEvents;

#[cfg(target_os = "linux")]
mod linux_native;
#[cfg(not(target_os = "linux"))]
mod macos;
#[cfg(not(target_os = "linux"))]
mod windows;

#[cfg(target_os = "linux")]
pub use linux_native::NativeLinuxPlayer;

#[cfg(not(target_os = "linux"))]
pub mod tauri_player {
    use std::sync::Mutex;

    use tauri::{AppHandle, Emitter, Manager};

    use crate::models::{StreamSpec, VideoPosition};
    use crate::video::resolve_stream_spec_inner;

    use super::events::PlaybackEvents;
    use super::macos::MacPlayer;
    use super::pipeline::PlaybackEngine;
    use super::windows::WindowsPlayer;

    struct MainThreadPlayer(PlatformPlayer);

    unsafe impl Send for MainThreadPlayer {}
    unsafe impl Sync for MainThreadPlayer {}

    pub struct PlayerState {
        inner: Mutex<Option<MainThreadPlayer>>,
        app: AppHandle,
        source_url: Mutex<Option<String>>,
        format_id: Mutex<Option<String>>,
        pending_seek_ms: Mutex<Option<u64>>,
        pending_play: Mutex<bool>,
    }

    impl PlayerState {
        pub fn new(app: AppHandle) -> Self {
            Self {
                inner: Mutex::new(None),
                app,
                source_url: Mutex::new(None),
                format_id: Mutex::new(None),
                pending_seek_ms: Mutex::new(None),
                pending_play: Mutex::new(false),
            }
        }

        fn ensure_initialized(&self) -> Result<(), String> {
            let mut player = self.inner.lock().map_err(to_string)?;
            if player.is_none() {
                *player = Some(MainThreadPlayer(PlatformPlayer::new(&self.app)));
            }
            Ok(())
        }

        fn with_player<F, T>(&self, action: F) -> Result<T, String>
        where
            F: FnOnce(&mut PlatformPlayer) -> Result<T, String>,
        {
            self.ensure_initialized()?;
            let mut guard = self.inner.lock().map_err(to_string)?;
            let player = guard
                .as_mut()
                .ok_or_else(|| "Native player is unavailable".to_string())?;
            action(&mut player.0)
        }

        pub fn setup_window(&self, window: &tauri::WebviewWindow) -> Result<(), String> {
            self.with_player(|player| player.setup_window(window, &self.app))
        }

        pub fn layout_window_panes(&self, app: &AppHandle, window: &tauri::WebviewWindow) {
            let _ = self.with_player(|player| {
                player.layout_panes(app, window);
                Ok(())
            });
        }

        fn emit_position(&self) {
            let snapshot = self
                .inner
                .lock()
                .ok()
                .and_then(|player| player.as_ref().map(|player| player.0.snapshot()))
                .unwrap_or(VideoPosition {
                    ms: 0,
                    duration_ms: None,
                    playing: false,
                    buffering: false,
                });
            let _ = self.app.emit("video-position", snapshot);
        }

        fn load_spec(&self, spec: StreamSpec) -> Result<(), String> {
            let pending_seek = self.pending_seek_ms.lock().map_err(to_string)?.take();
            let pending_play = *self.pending_play.lock().map_err(to_string)?;

            self.with_player(|player| {
                player.load(spec)?;
                if let Some(ms) = pending_seek {
                    player.seek(ms)?;
                }
                if pending_play {
                    player.play()?;
                }
                Ok(())
            })?;

            self.emit_position();
            Ok(())
        }
    }

    pub enum PlatformPlayer {
        #[cfg(target_os = "windows")]
        Windows(WindowsPlayer),
        #[cfg(target_os = "macos")]
        Mac(MacPlayer),
    }

    impl PlatformPlayer {
        fn new(app: &AppHandle) -> Self {
            #[cfg(target_os = "windows")]
            {
                return Self::Windows(WindowsPlayer::new(app.clone()));
            }
            #[cfg(target_os = "macos")]
            {
                return Self::Mac(MacPlayer::new(app.clone()));
            }
        }

        fn setup_window(
            &mut self,
            window: &tauri::WebviewWindow,
            app: &AppHandle,
        ) -> Result<(), String> {
            match self {
                #[cfg(target_os = "windows")]
                Self::Windows(player) => player.setup_window(window, app),
                #[cfg(target_os = "macos")]
                Self::Mac(player) => player.setup_window(window, app),
            }
        }

        fn layout_panes(&mut self, app: &AppHandle, window: &tauri::WebviewWindow) {
            #[cfg(target_os = "windows")]
            if let Self::Windows(player) = self {
                player.layout_panes(app, window);
            }
            #[cfg(target_os = "macos")]
            if let Self::Mac(player) = self {
                player.layout_panes(app, window);
            }
        }

        fn load(&mut self, spec: StreamSpec) -> Result<(), String> {
            match self {
                #[cfg(target_os = "windows")]
                Self::Windows(player) => player.load(spec),
                #[cfg(target_os = "macos")]
                Self::Mac(player) => player.load(spec),
            }
        }

        fn play(&mut self) -> Result<(), String> {
            match self {
                #[cfg(target_os = "windows")]
                Self::Windows(player) => player.play(),
                #[cfg(target_os = "macos")]
                Self::Mac(player) => player.play(),
            }
        }

        fn pause(&mut self) -> Result<(), String> {
            match self {
                #[cfg(target_os = "windows")]
                Self::Windows(player) => player.pause(),
                #[cfg(target_os = "macos")]
                Self::Mac(player) => player.pause(),
            }
        }

        fn seek(&mut self, ms: u64) -> Result<(), String> {
            match self {
                #[cfg(target_os = "windows")]
                Self::Windows(player) => player.seek(ms),
                #[cfg(target_os = "macos")]
                Self::Mac(player) => player.seek(ms),
            }
        }

        fn snapshot(&self) -> VideoPosition {
            match self {
                #[cfg(target_os = "windows")]
                Self::Windows(player) => player.snapshot(),
                #[cfg(target_os = "macos")]
                Self::Mac(player) => player.snapshot(),
            }
        }
    }

    #[tauri::command]
    pub fn resolve_stream(url: String, format_id: Option<String>) -> Result<StreamSpec, String> {
        resolve_stream_spec_inner(&url, format_id.as_deref()).map_err(to_string)
    }

    #[tauri::command]
    pub async fn player_load(
        app: AppHandle,
        url: String,
        format_id: Option<String>,
    ) -> Result<(), String> {
        let spec = resolve_stream_spec_inner(&url, format_id.as_deref()).map_err(to_string)?;
        run_on_player_thread(app, move |state| {
            *state.source_url.lock().map_err(to_string)? = Some(url);
            *state.format_id.lock().map_err(to_string)? = format_id;
            state.load_spec(spec)
        })
        .await
    }

    #[tauri::command]
    pub async fn player_play(app: AppHandle) -> Result<(), String> {
        run_on_player_thread(app, move |state| {
            *state.pending_play.lock().map_err(to_string)? = true;
            state.with_player(|player| {
                player.play()?;
                Ok(())
            })?;
            state.emit_position();
            Ok(())
        })
        .await
    }

    #[tauri::command]
    pub async fn player_pause(app: AppHandle) -> Result<(), String> {
        run_on_player_thread(app, move |state| {
            *state.pending_play.lock().map_err(to_string)? = false;
            state.with_player(|player| {
                player.pause()?;
                Ok(())
            })?;
            state.emit_position();
            Ok(())
        })
        .await
    }

    #[tauri::command]
    pub async fn player_seek(app: AppHandle, ms: u64) -> Result<(), String> {
        run_on_player_thread(app, move |state| {
            state.with_player(|player| {
                player.seek(ms)?;
                Ok(())
            })?;
            state.emit_position();
            Ok(())
        })
        .await
    }

    #[tauri::command]
    pub async fn player_set_quality(app: AppHandle, format_id: String) -> Result<(), String> {
        run_on_player_thread(app, move |state| {
            let url = state
                .source_url
                .lock()
                .map_err(to_string)?
                .clone()
                .ok_or_else(|| "Load a video before changing quality".to_string())?;
            let position = state.with_player(|player| Ok(player.snapshot().ms))?;
            let playing = state.with_player(|player| Ok(player.snapshot().playing))?;

            *state.pending_seek_ms.lock().map_err(to_string)? = Some(position);
            *state.pending_play.lock().map_err(to_string)? = playing;
            *state.format_id.lock().map_err(to_string)? = Some(format_id.clone());

            let spec =
                resolve_stream_spec_inner(&url, Some(format_id.as_str())).map_err(to_string)?;
            state.load_spec(spec)
        })
        .await
    }

    async fn run_on_player_thread<F>(app: AppHandle, action: F) -> Result<(), String>
    where
        F: FnOnce(&PlayerState) -> Result<(), String> + Send + 'static,
    {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let app_for_thread = app.clone();
        app.run_on_main_thread(move || {
            let state = app_for_thread.state::<PlayerState>();
            let result = action(&state);
            let _ = tx.send(result);
        })
        .map_err(to_string)?;

        rx.recv().map_err(|err| err.to_string())?
    }

    const MAIN_WINDOW_LABEL: &str = "main";

    pub fn defer_window_setup(app: AppHandle) {
        if let Some(player) = app.try_state::<PlayerState>() {
            let _ = player.ensure_initialized();
            if let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) {
                let _ = player.setup_window(&window);
            }
        }
    }

    fn to_string<E: std::fmt::Display>(err: E) -> String {
        err.to_string()
    }
}

#[cfg(not(target_os = "linux"))]
pub use tauri_player::{
    defer_window_setup, player_load, player_pause, player_play, player_seek, player_set_quality,
    resolve_stream, PlayerState,
};
