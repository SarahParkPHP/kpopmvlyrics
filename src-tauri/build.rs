use std::env;

fn main() {
    // Semantic cfg aliases, resolved against the *target* (not the host) so that
    // cross-compiling — e.g. to FreeBSD — routes to the right UI/player backend.
    //
    //   desktop_unix : GTK/Qt + gstreamer native path (Linux + the BSDs)
    //   apple        : macOS (UniFFI/SwiftUI native path)
    //   tauri_shell  : legacy Tauri webview shell (Windows + macOS, until each
    //                  gains its native frontend)
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();

    for name in ["desktop_unix", "apple", "tauri_shell", "native_frontend"] {
        println!("cargo::rustc-check-cfg=cfg({name})");
    }

    // `native_frontend`: the reusable `NativePlayer` + public `frontend` API used
    // by the non-GTK Rust frontends (macOS/UniFFI, Qt/QML, WinUI 3), as opposed
    // to the GTK4 in-process player. Driven by the `native_player` feature.
    if env::var("CARGO_FEATURE_NATIVE_PLAYER").is_ok() {
        println!("cargo::rustc-cfg=native_frontend");
    }

    let desktop_unix = matches!(
        target_os.as_str(),
        "linux" | "freebsd" | "dragonfly" | "openbsd" | "netbsd"
    );
    let apple = target_os == "macos";
    let tauri_shell = matches!(target_os.as_str(), "windows" | "macos");

    if desktop_unix {
        println!("cargo::rustc-cfg=desktop_unix");
    }
    if apple {
        println!("cargo::rustc-cfg=apple");
    }
    if tauri_shell {
        println!("cargo::rustc-cfg=tauri_shell");
    }

    if tauri_shell {
        tauri_build::build();
    }
}
