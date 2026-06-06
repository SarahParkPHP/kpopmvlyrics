//! Win32 host window + shared-core integration for Windows.
//!
//! This is the foundation the WinUI 3 XAML UI builds on: it opens the core,
//! starts the player with a `d3d11videosink`, and binds that sink to a native
//! window handle (HWND). The XAML controls / lyric views are layered on top via
//! the Windows App SDK (see README). Like every Rust frontend it calls
//! `AppContext`/`NativePlayer`/`invoke` in-process — no FFI.
//!
//! NOTE: this module compiles only on Windows and has NOT been built here;
//! expect windows-rs API adjustments on first compile.

use std::sync::OnceLock;

use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

use kpopmvlyrics_lib::frontend::{invoke, AppContext, NativePlayer};

/// Sinks the Windows path prefers, in order.
const WINDOWS_SINKS: &[&str] = &["d3d11videosink", "autovideosink"];

struct AppState {
    ctx: AppContext,
    player: NativePlayer,
}

static STATE: OnceLock<AppState> = OnceLock::new();

/// Convenience: run a core command and return the JSON result.
#[allow(dead_code)]
pub fn command(name: &str, args_json: &str) -> std::result::Result<String, String> {
    let state = STATE.get().ok_or("core not initialized")?;
    invoke(&state.ctx, name, args_json)
}

pub fn run() -> std::result::Result<(), String> {
    let ctx = AppContext::open()?;
    let player = NativePlayer::spawn(
        WINDOWS_SINKS.to_vec(),
        |position_json| {
            // TODO: marshal to the UI thread and update the XAML view-model.
            let _ = position_json;
        },
        |message| eprintln!("kpml player error: {message}"),
    );
    let _ = STATE.set(AppState { ctx, player });

    unsafe {
        let instance: HINSTANCE = GetModuleHandleW(None).map_err(stringify)?.into();
        let class_name = w!("KpmlWindowClass");

        let wc = WNDCLASSW {
            hInstance: instance,
            lpszClassName: class_name,
            lpfnWndProc: Some(wndproc),
            hCursor: LoadCursorW(None, IDC_ARROW).map_err(stringify)?,
            ..Default::default()
        };
        RegisterClassW(&wc);

        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class_name,
            w!("K-Pop MV Lyrics"),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            1000,
            680,
            None,
            None,
            instance,
            None,
        )
        .map_err(stringify)?;

        attach_video(hwnd);

        let mut message = MSG::default();
        while GetMessageW(&mut message, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }

    Ok(())
}

/// Re-bind the video sink to the window's current client rectangle.
unsafe fn attach_video(hwnd: HWND) {
    let Some(state) = STATE.get() else { return };
    let mut rect = RECT::default();
    let _ = GetClientRect(hwnd, &mut rect);
    let _ = state
        .player
        .attach_surface(hwnd.0 as u64, 0, 0, rect.right, rect.bottom);
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_SIZE => {
                attach_video(hwnd);
                LRESULT(0)
            }
            WM_KEYDOWN => {
                if let Some(state) = STATE.get() {
                    // Spacebar = play (minimal transport stand-in until the
                    // WinUI 3 controls exist).
                    if wparam.0 as u32 == 0x20 {
                        let _ = state.player.play();
                    }
                }
                LRESULT(0)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

fn stringify<E: std::fmt::Display>(err: E) -> String {
    err.to_string()
}
