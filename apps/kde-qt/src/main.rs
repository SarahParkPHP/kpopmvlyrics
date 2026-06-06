//! KDE Plasma / Qt-QML frontend entry point.

use cxx_qt_lib::{QGuiApplication, QQmlApplicationEngine, QUrl};

mod backend;

fn main() {
    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();

    if let Some(engine) = engine.as_mut() {
        engine.load(&QUrl::from(
            "qrc:/qt/qml/com/kpopmvlyrics/kde/qml/Main.qml",
        ));
    }

    if let Some(app) = app.as_mut() {
        app.exec();
    }
}
