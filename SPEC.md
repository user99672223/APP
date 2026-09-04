# APP — Architecture & Spec (v1)

Private peer‑to‑peer voice/video/chat/screen‑share/file‑transfer app for Windows and iOS. Media always goes device‑to‑device. A small self‑hosted server handles accounts, presence, signaling, offline messages and notifications. "APP" is a placeholder name.

**Decisions made for you where you hadn't answered (change any):** voice processing on by default · system audio on by default with screen share · no room password · no creator powers · no participant cap · files can be sent to one or several people · receiver must accept files · group video encodes once at the ceiling, a lagging peer skips frames until the next keyframe.

---

## 1. Components

```
Windows app (Flutter)    ─┐
                          ├─ engine (Rust, shared) ─ iroh/QUIC ─► other devices   (media, chat, files)
iOS app (Swift, SwiftUI) ─┘         │
                                    └─ iroh/QUIC ─► server (Debian laptop) ─► ntfy.sh ─► iPhone notification
```

- **engine** — one Rust crate: networking, media pipelines, crypto, settings, stats. Same code on both platforms.
- **Windows app** — Flutter UI. Engine linked through flutter_rust_bridge; decoded video arrives as Flutter textures via irondash_texture. Windows capture/encode/audio glue lives in the Rust engine.
- **iOS app** — SwiftUI UI + AVFoundation/VideoToolbox/AVAudioEngine glue. Links the engine through UniFFI.
- **server** — one Rust binary on the Debian laptop (systemd). Accounts, directory, presence, rooms, call signaling, store‑and‑forward, ntfy dispatch. SQLite. Never touches media. Reachable as an iroh endpoint, so no port forwarding and no Tailscale in the app. Moving it to a VM = copy its key file.
- **ntfy** — ntfy.sh + the ntfy iOS app. Wakes the sideloaded iOS app through a deep link.

## 2. Libraries

| Area | Choice |
|---|---|
| P2P transport, NAT traversal, encryption, identity | **iroh** (QUIC via quinn) |
| Async runtime | tokio |
| Wire format | serde + postcard |
| Audio codec | audiopus (libopus) |
| Audio I/O, Windows | cpal (WASAPI); system‑audio loopback via the `windows` crate |
| Echo cancel / noise suppression, Windows | webrtc‑audio‑processing |
| Audio I/O, iOS | AVAudioEngine with voice processing (Swift) |
| Camera, Windows | nokhwa (Media Foundation backend) |
| Camera, iOS | AVFoundation (Swift) |
| Video encode/decode, Windows | Media Foundation via `windows` crate — H.264, HEVC, AV1 where the GPU supports it |
| Video encode/decode, iOS | VideoToolbox (Swift) — H.264/HEVC both ways; AV1 decode on iPhone 15 Pro and later |
| AV1 software | SVT‑AV1 (encode), dav1d (decode), via FFI bindings built in CI |
| Screen capture, Windows | windows‑capture (Windows Graphics Capture API) |
| Video render, Windows | irondash_texture (engine → Flutter texture) |
| Video render, iOS | AVSampleBufferDisplayLayer |
| Windows UI | Flutter; tray_manager |
| iOS UI | SwiftUI |
| Swift ↔ Rust / Dart ↔ Rust | UniFFI / flutter_rust_bridge |
| Client local DB | redb + chacha20poly1305 |
| Server DB | rusqlite (SQLite) |
| Passwords | argon2 (Argon2id) |
| E2E message crypto | ed25519‑dalek, x25519‑dalek, chacha20poly1305, hkdf, blake3 |
| ntfy publishing | reqwest |
| Logging / metrics | tracing |
| **Not used** | libwebrtc, FFmpeg, Tailscale inside the app |

## 3. Identity & accounts

- **Device key**: Ed25519 key pair generated on first launch. It is also the iroh EndpointId. Every connection is mutually authenticated by it.
- **Account**: username, password (Argon2id hash), display name, server‑assigned handle. Created in‑app. Registration requires the server's invite code.
- **Login** binds the device key to the account. After that the device never sends the password again; the key is the credential.
- **Multi‑device**: an account can have any number of device keys. Calls ring every device; first answer wins. Messages are encrypted to every device key of the recipient.
- **Device management**: list and revoke devices from any logged‑in device. Password reset: admin only (you).
- **Directory**: one pool. Every registered user is visible to everyone, with presence (online / last seen). No contacts, no requests.
- Chat history lives on the device that received it. No history sync between your own devices in v1.

## 4. Server

- Rust binary, systemd on Debian, SQLite. iroh endpoint with a fixed key; clients dial by key. ALPN `app/control/1`.
- **Tables**: accounts · devices (account_id, endpoint_id, device_name, ntfy_topic, last_seen) · rooms (id, code, created, members) · pending_messages (recipient_device, ciphertext, created) · calls (id, from_account, to_account, room_id, state) · invite_code.
- **Does**: register / login / bind device · directory + presence broadcasts · rooms (create, join, leave, peer lists) · call invite / answer / decline / timeout · store‑and‑forward · ntfy dispatch.
- **Control protocol**: one long‑lived bidirectional QUIC stream per device carrying postcard‑encoded `ClientMsg` / `ServerMsg` enums. Heartbeat every 10 s; a device is offline after two missed heartbeats.
- Presence: online = control stream alive and heartbeating.
- Never sees media, chat plaintext or file contents.

## 5. Peer connections

- Every pair of devices in a room has its own iroh connection. ALPN `app/media/1`. Full mesh at any size.
- Direct when hole punching succeeds; iroh relay for that pair only when it doesn't. Public n0 relays first; self‑hosted later (can run on the same laptop).
- Per‑peer: direct/relayed indicator, RTT, loss.
- Auto‑reconnect on network change; room membership survives a short drop.

**Channels on a peer connection**

| Channel | Type | Contents |
|---|---|---|
| `ctrl` | bidirectional stream | keyframe request, mute state, codec announce, bitrate hints, hang‑up |
| `chat` | bidirectional stream | length‑prefixed postcard messages |
| `file/<id>` | one unidirectional stream per file | header + bytes |
| audio | datagrams | header (seq u16, timestamp u32, channels u8) + Opus frame + previous Opus frame |
| video | one unidirectional stream per frame | header (frame_no, timestamp, codec, keyframe flag, width, height, length) + encoded frame |
| screen video / screen audio | same as video / audio, separate stream families | — |

Late video frames: the sender resets the stream; the receiver discards partials; any dropped frame triggers a keyframe request over `ctrl`.

## 6. Rooms & calls

- **Room**: server record + 6‑character code (A–Z, 0–9, case‑insensitive). Lives while anyone is in it; code expires after 24 h idle. No password, no creator powers, no cap.
- Join by typing the code, or by being pulled in by a member from the directory. Server sends the peer list; each device dials the new peers.
- **Direct call** = caller creates a room, server invites every device of the callee. Callee answers on one device; the others stop ringing. 60 s timeout → missed call.
- Anyone can start a room and pull in anyone.

## 7. Notifications (iOS) & deep links

- Each device generates a random 32‑character ntfy topic on ntfy.sh and registers it with the server.
- Server pushes only when live delivery to that device is not acknowledged within 2 s. Notification text is generic ("Incoming call", "New message"). Click URL forms:
  - `app://call/<callId>?from=<userId>&exp=<unixtime>`
  - `app://dm/<userId>?msg=<msgId>`
  - `app://room/<roomId>`
- On open: render instantly from the local directory (name, avatar), connect to the server, verify the event, then proceed — or show "ended / expired / invalid". `exp` prevents a stale call notification from ringing.
- On every launch and return to foreground: full inbox sync from the server. The tapped notification only chooses the first screen.
- Windows needs no ntfy: the app stays in the tray with the control stream open and shows an in‑app popup.
- No native iOS call screen (requires Apple VoIP push, unavailable to sideloaded apps).

## 8. Messaging

- **Live**: text goes over the peer connection when the peer is connected in the same room; otherwise through the server.
- **Offline**: server stores ciphertext per recipient device, delivers on next connect, deletes on ack, expires after 30 days.
- **E2E scheme**: random per‑message key → message encrypted with XChaCha20‑Poly1305 → key wrapped for each recipient device with X25519 (derived from that device's Ed25519 key) + HKDF → signed by the sender's device key. The server holds only wrapped blobs.
- Group chat offline: same scheme fanned out to every member's devices.
- Local history: redb, encrypted at rest, per room and per DM, clearable. No history for late joiners in v1.
- v1 is text only. Typing indicators, edit/delete, reactions, inline images: later.

## 9. Audio pipeline

Capture → echo cancel / noise suppression → Opus encode → datagram → jitter buffer → Opus decode (+ concealment) → mix → playback.

- 48 kHz. 10 ms frames. Opus application `RESTRICTED_LOWDELAY` (2.5 ms lookahead).
- Channels: 2 if the input device provides 2, else 1. Encoder follows the input.
- Bitrate: manual, default 510 kbps, adaptive downward only, lockable.
- Redundancy: every packet also carries the previous frame. On by default (Opus in‑band FEC is unavailable at this bitrate/mode).
- Jitter buffer: adaptive, initial target 20 ms, manual override.
- Voice processing (echo cancel, noise suppression, AGC): on by default. Windows via webrtc‑audio‑processing; iOS via AVAudioEngine voice processing. Raw‑mic toggle disables it (headphones needed to avoid echo). Voice processing forces mono.
- OS buffers: iOS preferred IO buffer 5 ms; Windows WASAPI shared low‑latency (~10 ms), exclusive mode optional.
- Group: mix N−1 decoded streams with a soft limiter. Per‑peer volume. Mute.
- Devices selectable; hot‑swap on unplug.

## 10. Video pipeline

Capture → hardware encode → one stream per frame → hardware decode → render.

- **Codec**: HEVC default; H.264 and AV1 selectable per sender. The sender announces its codec on `ctrl`; if any receiver can't decode it (hardware or software), the sender falls back to HEVC for everyone.
- **AV1 reality**: iPhone encodes AV1 in software only (SVT‑AV1, expect ≤720p30 and heat); iPhone 15 Pro and later decode in hardware, older iPhones through dav1d. Windows encodes in hardware on RTX 40‑series / Intel Arc / RX 7000, otherwise SVT‑AV1.
- Resolution / fps: manual, default 1080p60 or the camera's best below it. Bitrate: manual, default 12 Mbps, adaptive downward only, lockable.
- Encoder settings: no B‑frames, keyframe every 2 s and on request, real‑time/low‑latency mode, CBR.
- Group: encode once per sender at the ceiling. A peer whose link falls behind has frames skipped on its connection until the next keyframe; only that peer stutters. Simulcast later.
- Front/back camera on iOS; mirrored self‑view; video off / audio‑only mode.
- Render: Windows hands decoded frames to a Flutter texture through irondash_texture; iOS feeds AVSampleBufferDisplayLayer.
- A/V sync: audio is the master clock; toggle off for minimum latency.

## 11. Screen share

- Source: Windows only — full screen or one window, cursor shown. iOS views only.
- Same codec / resolution / fps / bitrate controls as video. Default 1080p60 HEVC 12 Mbps; up to native resolution and refresh rate.
- System audio (stereo, WASAPI loopback) sent alongside as its own Opus stream at the audio settings. On by default.
- Camera and screen at the same time from one person: allowed. Several people sharing at once: allowed. Each share is its own stream family.

## 12. File transfer

- One unidirectional stream per file. Header: file id, name, size, BLAKE3 hash. Body: raw bytes. Receiver tracks the byte offset; after a reconnect the sender resumes from the last acknowledged offset. Hash verified on completion.
- Send to one person or several selected. Receiver is prompted to accept. Any size. Uncapped speed with a manual cap.
- iOS: the app must stay in the foreground during a transfer.

## 13. Quality adaptation

- **Ceiling model**: every quality setting is a maximum the user sets. The controller only lowers below it during congestion and climbs back when the link recovers. A per‑setting lock disables adaptation for that setting.
- **Signals, per peer**: RTT rise over baseline, packet loss, datagram drops, video frame delivery delay, stream resets — read from quinn connection stats and engine timing.
- **Actions, in order**: video bitrate → fps → resolution; audio bitrate last. Step back up after 5 s stable.
- Audio always fits: video budget = link estimate − audio.

## 14. Settings (all manual, all persisted)

- **Audio**: bitrate (510) · redundancy on/off · jitter buffer override · voice processing on/off · mic device · speaker device · per‑peer volume
- **Video**: codec (HEVC) · resolution (1080p) · fps (60) · bitrate (12 Mbps) · camera · mirror self‑view
- **Screen**: source · cursor · codec / resolution / fps / bitrate · system audio on/off
- **Adaptation**: per‑setting lock · A/V sync on/off
- **Files**: auto‑accept off · speed cap
- **Notifications**: per‑user mute
- **Account**: devices · logout

## 15. Diagnostics

- Stats overlay per peer: link type, RTT, loss, jitter buffer depth, audio/video bitrate in and out, fps, encode/decode ms, frame delivery delay, clock drift, dropped frames.
- Structured logs (tracing), exportable from the app.
- Loopback mode: the app calls itself through the full pipeline.

## 16. Security

- All links: QUIC TLS 1.3, device keys as identities, mutual authentication. Relays forward ciphertext only.
- Server sees: usernames, handles, device keys, IPs, presence, room membership, ntfy topics, encrypted message blobs. Never plaintext.
- Local storage encrypted at rest with a device‑bound key (Keychain on iOS, DPAPI on Windows).
- Invite‑code registration. Argon2id passwords. Device revocation.

## 17. Platform notes

- **Windows**: x64. Tray‑resident. Windows Graphics Capture for screen, Media Foundation for camera and codecs, WASAPI shared low‑latency for mic/speaker plus loopback for system audio.
- **iOS**: background audio mode keeps a call alive when switching apps; the camera pauses in the background (Apple rule); no screen share source; custom URL scheme `app://`; ntfy app installed; sideloaded, built by GitHub Actions.

## 18. Repository layout

```
/proto        message types shared by engine and server (Rust module)
/engine       Rust crate: transport, media, chat, files, settings, stats
/engine-ffi   UniFFI (Swift) + flutter_rust_bridge (Dart) bindings
/server       Rust binary
/windows      Flutter app, links engine via flutter_rust_bridge
/ios          Xcode project (SwiftUI), links engine-ffi
/.github      CI: Windows build; iOS xcframework + archive
```

## 19. Build order

1. Skeleton — engine, Windows app, iOS app, CI; "hello" across the bridge on both.
2. Server — accounts, invite code, directory, presence, control protocol. Both apps log in and see each other.
3. Peer connections — rooms, mesh, ping, direct/relayed indicator.
4. Chat — live + store‑and‑forward + E2E + ntfy + deep links + inbox sync.
5. Files.
6. Audio.
7. Video + codec switching.
8. Adaptation + diagnostics overlay.
9. Screen share + system audio.
10. Call/ringing UX, device management, complete settings UI.

## 20. Deferred

Android, macOS, simulcast, media server for large rooms, history sync between your own devices, late‑joiner history, QR device linking, call recording, block, typing/edit/reactions, native call UI.
