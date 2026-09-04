# /uniffi-bindgen — owner: iOS session

Tiny binary that runs UniFFI's bindings generator against the built engine-ffi library
(the setup UniFFI's docs recommend instead of a globally installed `uniffi-bindgen`):

    cargo run -p uniffi-bindgen -- generate --library <path to libengine_ffi.a or .dylib> --language swift --out-dir ios/Generated

Keep its `uniffi` version identical to the one in /engine-ffi.
