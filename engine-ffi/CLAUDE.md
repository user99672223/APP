# /engine-ffi — owner: iOS session

Only the iOS session edits this directory.

UniFFI bindings of the engine for Swift (proc-macro style, `#[uniffi::export]`).
Builds as a static library for `aarch64-apple-ios` / `aarch64-apple-ios-sim`; the CI
workflow in /.github packs it into `EngineFFI.xcframework` and generates
`ios/Generated/engine.swift` with the `uniffi-bindgen` crate at the repo root.

flutter_rust_bridge (Dart) bindings for the Windows app are added here later, behind a
`dart` feature, when the Windows session requests them.

Keep the exported surface small and typed: records for data, enums for events,
one `Engine` object, one `EngineListener` callback interface.
