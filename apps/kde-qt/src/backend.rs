//! cxx-qt bridge: a `Backend` QObject that exposes the shared Rust core to QML.
//!
//! Like the GTK4 and WinUI 3 frontends, this calls `AppContext`/`NativePlayer`
//! in-process (no FFI). Structured data crosses to QML as JSON strings via
//! `invoke(command, args)`, matching every other frontend; player events arrive
//! as the `position`/`playerError` signals.

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qproperty(QString, last_error)]
        type Backend = super::BackendRust;

        /// Open the core + start the player thread. Call from `Component.onCompleted`.
        #[qinvokable]
        fn initialize(self: Pin<&mut Backend>);

        /// Run a core command; returns the result (or `{"error":...}`) as JSON.
        #[qinvokable]
        fn invoke(self: Pin<&mut Backend>, command: &QString, args: &QString) -> QString;

        #[qinvokable]
        fn player_load(self: Pin<&mut Backend>, stream_spec_json: &QString) -> QString;
        #[qinvokable]
        fn player_play(self: Pin<&mut Backend>) -> QString;
        #[qinvokable]
        fn player_pause(self: Pin<&mut Backend>) -> QString;
        #[qinvokable]
        fn player_seek(self: Pin<&mut Backend>, ms: f64) -> QString;
        #[qinvokable]
        fn player_snapshot(self: Pin<&mut Backend>) -> QString;
        #[qinvokable]
        fn attach_surface(
            self: Pin<&mut Backend>,
            handle: f64,
            x: f64,
            y: f64,
            width: f64,
            height: f64,
        );

        /// Emitted from the player thread with a `VideoPosition` JSON payload.
        #[qsignal]
        fn position(self: Pin<&mut Backend>, json: QString);
        /// Emitted with a player error message.
        #[qsignal]
        fn player_error(self: Pin<&mut Backend>, message: QString);
    }

    // Allow queueing closures onto the Qt event loop from the player thread.
    impl cxx_qt::Threading for Backend {}
}

use core::pin::Pin;
use std::cell::RefCell;

use cxx_qt::{CxxQtType, Threading};
use cxx_qt_lib::QString;
use kpopmvlyrics_lib::frontend::{invoke as core_invoke, AppContext, NativePlayer};

/// Sinks the Qt/Wayland/X11 desktop path prefers, in order.
const QT_SINKS: &[&str] = &["glimagesink", "xvimagesink", "waylandsink", "autovideosink"];

pub struct BackendRust {
    last_error: QString,
    ctx: RefCell<Option<AppContext>>,
    player: RefCell<Option<NativePlayer>>,
}

impl Default for BackendRust {
    fn default() -> Self {
        Self {
            last_error: QString::default(),
            ctx: RefCell::new(None),
            player: RefCell::new(None),
        }
    }
}

impl qobject::Backend {
    pub fn initialize(mut self: Pin<&mut Self>) {
        match AppContext::open() {
            Ok(ctx) => *self.as_ref().rust().ctx.borrow_mut() = Some(ctx),
            Err(err) => {
                self.as_mut().set_last_error(QString::from(&err));
                return;
            }
        }

        let position_thread = self.qt_thread();
        let error_thread = self.qt_thread();
        let player = NativePlayer::spawn(
            QT_SINKS.to_vec(),
            move |json| {
                let _ = position_thread.queue(move |qobject| {
                    qobject.position(QString::from(&json));
                });
            },
            move |message| {
                let _ = error_thread.queue(move |qobject| {
                    qobject.player_error(QString::from(&message));
                });
            },
        );
        *self.as_ref().rust().player.borrow_mut() = Some(player);
    }

    pub fn invoke(self: Pin<&mut Self>, command: &QString, args: &QString) -> QString {
        let ctx_ref = self.rust().ctx.borrow();
        let Some(ctx) = ctx_ref.as_ref() else {
            return QString::from("{\"error\":\"core not initialized\"}");
        };
        match core_invoke(ctx, &command.to_string(), &args.to_string()) {
            Ok(json) => QString::from(&json),
            Err(err) => QString::from(&error_json(&err)),
        }
    }

    pub fn player_load(self: Pin<&mut Self>, stream_spec_json: &QString) -> QString {
        self.with_player(|player| player.load_json(&stream_spec_json.to_string()))
    }

    pub fn player_play(self: Pin<&mut Self>) -> QString {
        self.with_player(|player| player.play())
    }

    pub fn player_pause(self: Pin<&mut Self>) -> QString {
        self.with_player(|player| player.pause())
    }

    pub fn player_seek(self: Pin<&mut Self>, ms: f64) -> QString {
        self.with_player(|player| player.seek(ms.max(0.0) as u64))
    }

    pub fn player_snapshot(self: Pin<&mut Self>) -> QString {
        match self.rust().player.borrow().as_ref() {
            Some(player) => QString::from(&player.snapshot_json()),
            None => QString::from("null"),
        }
    }

    pub fn attach_surface(
        self: Pin<&mut Self>,
        handle: f64,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    ) {
        if let Some(player) = self.rust().player.borrow().as_ref() {
            let _ = player.attach_surface(
                handle as u64,
                x as i32,
                y as i32,
                width as i32,
                height as i32,
            );
        }
    }

    fn with_player(
        self: Pin<&mut Self>,
        call: impl FnOnce(&NativePlayer) -> Result<(), String>,
    ) -> QString {
        match self.rust().player.borrow().as_ref() {
            Some(player) => match call(player) {
                Ok(()) => QString::from("{}"),
                Err(err) => QString::from(&error_json(&err)),
            },
            None => QString::from("{\"error\":\"player not initialized\"}"),
        }
    }
}

fn error_json(message: &str) -> String {
    let escaped = serde_json::to_string(message).unwrap_or_else(|_| "\"\"".to_string());
    format!("{{\"error\":{escaped}}}")
}
