//! Windows frontend entry point.
//!
//! The real frontend (Win32 host window + core integration, the foundation for
//! the WinUI 3 XAML UI) lives in `app.rs` behind `cfg(windows)`. On other
//! platforms this is a stub so the crate still builds (and the shared core is
//! type-checked) in CI on Linux/macOS.

#[cfg(windows)]
mod app;

#[cfg(windows)]
fn main() {
    if let Err(err) = app::run() {
        eprintln!("kpml-windows: {err}");
        std::process::exit(1);
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("kpml-windows builds and runs only on Windows.");
}
