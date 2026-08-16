# Portable CDP state checkpoint

## Source

- Commit: `30c8b0e` (`feat: add portable Rust CDP state ABI`)
- Node host exposure: `b5a3bc1` (`feat: expose portable CDP core through Node worker`)
- Crate: `crates/obscura-wasm/src/cdp.rs`
- The existing Tokio/TCP/WebSocket server remains native-only. This module is
  transport-independent and is compiled into the portable WASM artifact.

## Implemented ABI

- `PortableCdp` owns bounded connection, target, browser-context, session,
  action, and event state.
- `openConnection`, `closeConnection`, `cdpRequest`, `completeAction`,
  `pollCdpEvents`, and `cdpStatus` are wasm-bindgen exports.
- Browser/Target attachment, target creation/closure, context lifecycle,
  Runtime/Page enable, frame metadata, and target events are handled in Rust.
- Navigation, evaluation, screenshot, and PDF are represented as opaque host
  actions. The host completes them with `completeAction`.
- ABI version `cdpAbiVersion: 1` is included in the portable capability probe.
- JSON message, event, action-result, method, and session limits are bounded.
- Connection close removes sessions, events, and pending actions.

## Verification

- `cargo check -p obscura-wasm`: passed.
- `cargo check --release -p obscura-wasm --target wasm32-unknown-unknown`:
  passed.
- Release `obscura-wasm` nextest: 52/52 passed, including four CDP tests.
- Release wasm-bindgen output exported `PortableCdp`, `cdpAbiVersion`,
  `openConnection`, `cdpRequest`, `completeAction`, and `pollCdpEvents`.
- Direct Node smoke against the generated wrapper passed browser version,
  target discovery, attachment, and event polling.
- The Node Worker now exposes bounded `portableCdpOpen`, `portableCdpRequest`,
  `portableCdpComplete`, `portableCdpPoll`, and `portableCdpClose` methods;
  the real generated wrapper test passed the full action round trip.

## Remaining integration

The package WebSocket adapter still owns its current JavaScript target/session
dispatch. The next step is to route one connection/page action path through
this ABI, then move the remaining CDP domains and page ownership into the
shared WASM state. The current checkpoint is an implemented core slice, not
full CDP or full browser parity.
