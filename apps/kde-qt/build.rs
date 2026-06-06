use cxx_qt_build::{CxxQtBuilder, QmlModule};

fn main() {
    CxxQtBuilder::new()
        .qml_module(QmlModule {
            uri: "com.kpopmvlyrics.kde",
            rust_files: &["src/backend.rs"],
            qml_files: &["qml/Main.qml"],
            ..Default::default()
        })
        .build();
}
