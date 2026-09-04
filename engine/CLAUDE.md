# /engine — owner: iOS session

Only the iOS session edits this directory. Other sessions add requests to ../ENGINE_REQUESTS.md.

One Rust crate, identical on Windows and iOS: identity, server control client, peer mesh
(iroh/QUIC), audio pipeline (Opus, jitter buffer, redundancy, mixer), video framing
(one stream per frame), chat + E2E crypto + local history, file transfer with resume,
quality adaptation, settings, stats, deep links, loopback mode.

Platform glue is outside the core: iOS captures/encodes/decodes/renders in Swift and
talks to the engine through /engine-ffi. Windows glue (Media Foundation, WASAPI loopback,
Windows Graphics Capture, webrtc-audio-processing) is added here later, behind
`cfg(windows)` features, on request from the Windows session.

Rules: no `unwrap()`/`expect()` on runtime paths; `tracing` for logs; stats are structs;
`cargo fmt`, `cargo clippy --all-targets -- -D warnings` and `cargo test` must be clean.
Test on this machine: `cargo test -p engine` and `cargo run -p engine --example loopback`.
