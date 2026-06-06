mod align;
mod app;
mod captions;
#[cfg(feature = "native_player")]
mod command;
mod db;
mod export;
mod log;
mod lyrics;
mod members;
mod models;
mod player;
mod process_util;
mod tauri_app;
mod video;
mod asr;

#[cfg(desktop_unix)]
mod ui;

#[cfg(feature = "macos_ffi")]
mod ffi;

#[cfg(feature = "macos_ffi")]
uniffi::setup_scaffolding!();

/// Public API for the in-process Rust frontends (Qt/QML via cxx-qt, WinUI 3 via
/// windows-rs). They construct an [`AppContext`], drive a [`NativePlayer`], and
/// route data through [`invoke`] — the same JSON command surface the macOS
/// UniFFI app and GTK4 UI use.
#[cfg(feature = "native_player")]
pub mod frontend {
    pub use crate::app::AppContext;
    pub use crate::command::invoke;
    pub use crate::player::NativePlayer;
}

pub use log::{filter_app_args, init_logging};
pub use tauri_app::run_with_args;
