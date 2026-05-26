mod align;
mod app;
mod captions;
mod db;
mod lyrics;
mod members;
mod models;
mod player;
mod tauri_app;
mod video;
mod whisper;

#[cfg(target_os = "linux")]
mod ui;

pub use tauri_app::run;
