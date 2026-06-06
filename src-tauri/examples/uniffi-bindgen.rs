//! Standalone UniFFI binding generator.
//!
//! Generates the Swift bindings consumed by the macOS/SwiftUI frontend, e.g.:
//!
//! ```sh
//! cargo run --features macos_ffi --example uniffi-bindgen -- \
//!     generate --library target/debug/libkpopmvlyrics_lib.dylib \
//!     --language swift --out-dir apps/macos/Generated
//! ```
fn main() {
    uniffi::uniffi_bindgen_main()
}
