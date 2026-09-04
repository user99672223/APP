# /proto — owner: iOS session

Only the iOS session edits this directory. The server and Windows sessions read it
and add requests to ../ENGINE_REQUESTS.md.

What lives here: every message and media header exchanged between engine and server
(`control`), between two devices (`peer`), the E2E envelope (`e2e`), deep-link URL
forms (`deeplink`), shared constants (`consts`) and length-prefixed framing (`framing`).
Plain data only: serde + postcard, no I/O, no crypto, no platform code.

Compatibility rules (postcard is positional, not self-describing):
- Never remove, reorder or rename an existing field or enum variant.
- Add new enum variants only at the end. Old builds never emit them, new builds can.
- Never add a field to an existing struct. Put new data in a new variant or a new
  struct, or bump `PROTO_VERSION` and keep decoding the old shape.
- Every frame and media header carries a `version` field. Check it before decoding the rest.
- Round-trip tests for every message type live next to the types.

Using it from the server: `proto = { path = "../proto", features = ["tokio"] }`.
