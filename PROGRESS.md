# PROGRESS

## Current step
Engine complete for SPEC.md §19 steps 1-9 on this machine; iOS app written for step 10 (call/ringing UX, device management, complete settings UI) but never compiled: the first GitHub Actions run is the next step. `cargo fmt --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings` and `cargo test --workspace --all-features` are all green (50 tests).

## Done
- 2026-09-03: Toolchain installed (Rust 1.98.1 stable MSVC, CMake 4.4.3, NASM 3.02). `.cargo/config.toml` sets `CMAKE_POLICY_VERSION_MINIMUM=3.5` so the bundled libopus builds with CMake 4.
- /proto complete (22 tests).
- Engine (`/engine`, 25 unit + 10 integration tests, all green):
  - encrypted redb store (XChaCha20-Poly1305 per value, device-bound key); Ed25519 device identity = iroh endpoint id, X25519 derived; settings with spec defaults + validation; tracing logs to file with export.
  - server control client: one bi stream, seq/reply correlation, 10 s heartbeat with RTT, reconnect with backoff, address updates.
  - session: register/login/logout, directory cache + presence, devices list/revoke, every server push handled.
  - peer mesh: one iroh connection per pair, lower id dials, reconnect loop, ctrl + chat streams, uni-stream/datagram pumps, 1 s pings, direct/relay detection from iroh paths, QUIC loss sampling.
  - rooms/calls: create/join by code/leave/invite, call/answer/decline/cancel/hang up, resync after reconnect, mute/video state over ctrl.
  - chat: E2E envelope (per-message key, X25519+HKDF wrap per device, Ed25519 signature), live over peers with receipts, store-and-forward via the server, offline outbox, inbox sync, dedupe, `app://` deep links resolved against the server.
  - files: one uni stream per file, accept/reject/cancel, speed cap, progress, resume from the acknowledged offset after a drop or a receiver restart, BLAKE3 verified.
  - audio: Opus 48 kHz/10 ms RESTRICTED_LOWDELAY CBR, bitrate ceiling + adaptation, redundancy (previous frame in every datagram, trimmed when over the path MTU), adaptive jitter buffer (20 ms initial, override), PLC, idle silence, N-1 mixing with per-peer volume and soft limiter, mic level.
  - video: one uni stream per encoded frame, late frames reset, per-peer skip-until-keyframe, keyframe requests both ways (rate limited), codec announce with fallback to HEVC when a receiver lacks the codec, receiver ordering/loss recovery, delay/drift stats, A/V hold equal to the audio jitter target (audio is master), encode/decode ms reported by the platform.
  - adaptation (§13): level 0-7 ladder video bitrate → fps → resolution → audio, per-setting locks, step down on RTT rise/loss/resets/receiver reports/bitrate hints when every link is congested, climb back after 5 s calm; receiver reports sent every second.
  - stats structs (§15) per peer + engine; `adapt_level`.
  - mock server (`mock_server.rs`, feature `mock-server`): the whole control protocol in memory, used by tests and the harness; a reference for the server session.
  - loopback harness: `cargo run -p engine --example loopback --features mock-server -- --seconds 8` → "LOOPBACK OK" (audio both ways, video 30 fps both ways, chat, 1 MiB file).
- engine-ffi: full UniFFI 0.32 surface (records/enums mirrors, async request calls, sync media calls, listener with `onEvent` and `onVideoFrame`); builds on Windows; Swift bindings generate (`Engine.swift`, `EngineFFI.h`, modulemap).
- iOS (`/ios/Sources`): AppModel (engine bridge, events → published state), MediaController (AVAudioEngine voice-processing IO at 48 kHz/5 ms, camera + VideoToolbox HEVC/H.264 encoder with Annex-B framing, one AVSampleBufferDisplayLayer renderer per peer, keyframe requests, encode/decode timing), views: server setup, login/register, directory with presence + call/invite, room screen with tiles, self-view, controls, people/volume sheet, chat (DM + room, receipts, unread), files (picker, accept/reject/cancel, progress, resume), every §14 settings section, devices list + revoke, ntfy topic + subscribe link, incoming-call overlay, room-invite alert, diagnostics overlay, log export, deep links, inbox sync on foreground, background audio mode. Untested until CI builds it.
- CI workflow (`.github/workflows/ios.yml`): engine tests on Linux, xcframework + Swift bindings + unsigned IPA on macOS.

## How to build and run
- Everything: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace --all-features`
- Engine only: `cargo test -p engine --features mock-server`
- Loopback harness: `cargo run -p engine --example loopback --features mock-server -- --seconds 8` (add `APP_LOG=debug` for logs, `--no-video` to skip video)
- Swift bindings preview on Windows: `cargo build -p engine-ffi && cargo run -p uniffi-bindgen -- generate --library target/debug/engine_ffi.dll --language swift --out-dir <dir>`
- iOS: push to GitHub; the workflow uploads `APP-unsigned-ipa`. Never build /ios locally.
- On this laptop cargo needs `%USERPROFILE%\.cargo\bin`, `C:\Program Files\CMake\bin` and `%LOCALAPPDATA%\bin\NASM` on PATH (all on the user PATH now; open a new terminal).

## Next
1. Push, run the GitHub Actions workflow, fix the iOS build from the CI log (Swift has never been compiled; expect a round of type/API fixes).
2. First CI run; fix from the log (expected trouble spots: libopus cmake for iOS, ring on iOS, xcodegen project).
3. Windows-native engine glue (Media Foundation, WASAPI loopback, Windows Graphics Capture, webrtc-audio-processing, DPAPI key) when the Windows session starts and requests it.

## Deviations from SPEC.md
- Room membership is tracked per device (not per account): a user on two devices can be in a room twice. Calls still ring every device; first answer wins.
- Every server-routed message uses one path (`SendPending`/`Pending`): delivered live when the device is connected, stored otherwise, ntfy after 2 s without ack. The spec's store-and-forward and its "push only when live delivery is not acknowledged within 2 s" rule expressed as one mechanism.
- Peer-to-peer chat frames carry the same E2E envelope as the server path. One code path, identical history entries either way.
- Mesh dial direction: the device with the lower device id dials, the other accepts (spec: "each device dials the new peers"). Deterministic, no duplicate connections, same result.
- Media stream headers on peer connections are length-prefixed (u32 LE) postcard so a header's end is known without trial decoding. Peer protocol is engine-only.
- Audio redundancy at 510 kbps stereo (about 1276 bytes with the copy) exceeds the QUIC datagram budget on many paths; such packets are sent without the copy (`trimmed` counter) instead of failing. Lower bitrates keep full redundancy.
- Adaptation steps the single encoder down only when every link is congested; a single lagging peer only has frames skipped until the next keyframe (spec §10 rule). One peer alone cannot degrade the group.
- A/V sync v1: video is held by the receiver's audio jitter-buffer target for that peer (audio is the master clock) instead of per-frame timestamp matching. Off = deliver immediately.
- AV1 on iOS is not implemented (no SVT-AV1/dav1d FFI yet): the engine negotiates it and falls back to HEVC when a peer lacks it; the iOS app advertises H.264 + HEVC only. The Windows session will decide on the AV1 software path.
- Storage-at-rest key on Windows: the engine takes a 32-byte key from the platform; DPAPI wrapping is Windows glue for the Windows phase. The harness uses a fixed key.
