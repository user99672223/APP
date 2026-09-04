# CLAUDE.md — read this first

This repo is APP (placeholder name), specified in SPEC.md. Read SPEC.md completely before doing anything. SPEC.md is the source of truth. If code and spec disagree, the spec wins unless PROGRESS.md records a deliberate deviation.

## Three sessions, strict ownership
- **iOS session**: edits /proto, /engine, /engine-ffi, /ios. Only this session changes the engine or the protocol. It builds the engine and the complete iOS app.
- **Server session**: edits /server only. Reads /proto, never edits it. Protocol needs go into ENGINE_REQUESTS.md.
- **Windows session** (starts last, after the engine is complete): edits /windows only. Engine needs go into ENGINE_REQUESTS.md.

Never edit files outside your session's directories. Each directory has its own CLAUDE.md stating which session owns it.

## Conventions
- Rust 2021, stable toolchain, Cargo workspace at the repo root. `cargo fmt` and `cargo clippy -- -D warnings` must be clean.
- No `unwrap()` / `expect()` in engine or server code paths; return errors.
- Every protocol message and every media header carries a version field. Never remove or renumber fields; only add.
- Server database uses numbered migrations from the very first table.
- All logging through `tracing`. Stats are exposed as structs the UI can read, never just printed.
- Tests for: proto round-trips, and any pure logic (jitter buffer, adaptation controller, file chunking/resume).
- Comments explain why, not what.

## Workflow
- The iOS session works through the steps in SPEC.md §19 as internal milestones, without waiting for approval between them, until the engine and the iOS app are complete. Engine logic is tested on this machine with `cargo test` and a small CLI loopback harness in /engine/examples; the iOS app is built only by GitHub Actions.
- The server and Windows sessions work one step at a time and stop when asked.
- Before ending a session, update PROGRESS.md: what was done, exact commands to build and run, what is next, and any deviation from SPEC.md with the reason.
- If something in SPEC.md is impossible or wrong, stop and say so. Do not silently change the design.
- Ask one precise question rather than guess on anything that changes the architecture.

## Machines
- Development happens on this Windows 11 laptop. Rust compiles and tests here natively.
- The server is developed in WSL and deployed to a Debian laptop.
- iOS is built only on GitHub Actions (workflow in /.github). Never try to build /ios on this machine; write the code, push, read the CI log.
