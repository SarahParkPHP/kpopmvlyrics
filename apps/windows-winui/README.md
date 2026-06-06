# K-Pop MV Lyrics — Windows (WinUI 3)

Native Windows frontend. Like the GTK4 and Qt frontends it calls the shared Rust
core (`AppContext` + `NativePlayer`) **in-process** via windows-rs — no FFI. The
core's player uses `d3d11videosink` bound to a native window handle (HWND).

```
WinUI 3 XAML  ─┐
               ├─ Rust (windows-rs) ──> Rust core (AppContext + NativePlayer)
Win32 host HWND ┘                                   │
        ▲                                           │
        └──────────── player events ◀──────────────┘
```

## Status — scaffold, NOT yet built

This crate has **not been compiled** (developed on Linux; windows-rs requires the
Windows target/SDK). What's here:

- **Shared-core integration** (`src/app.rs`): opens `AppContext`, starts
  `NativePlayer` with `d3d11videosink`, and binds the sink to an HWND via
  `attach_surface` — the platform-specific plumbing the player needs.
- A minimal **Win32 host window** + message loop as the integration foundation.
- A cross-platform `main.rs` stub so the crate (and the shared core) still
  type-checks in CI on Linux/macOS; the Windows code is `cfg(windows)`.

## TODO to reach a real WinUI 3 app

1. Build on Windows (`cargo build --target x86_64-pc-windows-msvc`) and fix any
   windows-rs 0.58 API drift in `src/app.rs`.
2. Add the **Windows App SDK** bootstrapper (the "windows-reactor"/WinAppSDK
   bootstrap) and project packaging.
3. Replace the Win32 host with a **WinUI 3** `Window` and XAML UI (address row,
   transport, language toggles, lyric list, alignment editor) — mirroring the
   SwiftUI/Qt frontends. Render video into a `SwapChainPanel` or a child HWND and
   pass that handle to `attach_surface`.
4. Route all data through `app::command(name, args_json)` (the shared JSON
   command surface) and marshal `NativePlayer` position events onto the UI thread
   to drive the view-model.

## Build (on Windows)

```sh
cd apps/windows-winui
cargo run --target x86_64-pc-windows-msvc
```

Requires Rust (MSVC toolchain), the Windows SDK, and GStreamer for Windows on
`PATH` (for `d3d11videosink`).
