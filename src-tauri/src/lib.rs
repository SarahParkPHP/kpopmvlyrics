mod align;
mod app;
mod captions;
mod db;
mod log;
mod lyrics;
mod members;
mod models;
mod player;
mod process_util;
mod tauri_app;
mod video;
mod whisper;

#[cfg(target_os = "linux")]
mod ui;

pub use log::{filter_app_args, init_logging};
pub use tauri_app::run_with_args;
