# K-Pop MV Lyrics — KDE Plasma / Qt-QML

Native KDE/Qt frontend, built with [cxx-qt](https://kdab.github.io/cxx-qt/).
Like the GTK4 and WinUI 3 frontends it calls the shared Rust core
(`AppContext` + `NativePlayer`) **in-process** — no FFI/IPC. Structured data
crosses to QML as JSON via `Backend.invoke(command, args)`, the same command
surface every frontend uses; player events arrive as the `position` /
`playerError` signals.

```
QML (Main.qml) ──> Backend QObject (cxx-qt) ──> Rust core (AppContext + NativePlayer)
        ▲                                                    │
        └──────────── position / playerError signals ◀───────┘
```

## Prerequisites

- **Qt 6** development packages (`qmake6`, `moc`, Qt Quick / QML runtime).
- **Rust** (stable) and a C++ compiler.
- **GStreamer** (the core's player uses it; Qt path prefers
  `glimagesink`/`xvimagesink`/`waylandsink`).

## Build & run

```sh
cd apps/kde-qt
cargo build
./target/debug/kpml-kde
```

`cargo build` compiles the shared core (with the `native_player` feature), the
cxx-qt bridge, runs `moc`, and links Qt — no extra steps.

## Layout

| Path | Purpose |
|------|---------|
| `src/backend.rs` | `#[cxx_qt::bridge]` `Backend` QObject: `invoke`, player controls, `position`/`playerError` signals |
| `src/main.rs` | `QGuiApplication` + `QQmlApplicationEngine` loading `Main.qml` |
| `qml/Main.qml` | QML UI (address row, transport, language toggles, lyric list) |
| `build.rs` | `CxxQtBuilder` + `QmlModule` registration |

## Status / caveats

- **Builds and links against Qt 6.11** on Linux (verified). Not yet run against
  a live display here; exercise on a KDE/Wayland or X11 session.
- **Video rendering is a TODO.** Audio plays, but drawing the GStreamer output
  into the QML scene needs either a `qml6glsink` `GstGLVideoItem` or a native
  window-handle (`winId`) passed to `Backend.attach_surface` — the plumbing
  (`attach_surface`, `set_video_overlay_*`) is in place; only the QML-side
  surface/handle wiring remains. `Main.qml` shows a placeholder.
- `Backend.invoke` is **synchronous** and runs on the GUI thread, so long
  network calls (fetch lyrics/captions) block the UI. A production build should
  move those onto a worker (cxx-qt supports background threads + `qt_thread`,
  already used for player events).
- Targets KDE Plasma / Plasma Mobile, but Qt runs on any Linux/BSD desktop;
  runtime GTK-vs-Qt selection (per `XDG_CURRENT_DESKTOP`) is a later step.
