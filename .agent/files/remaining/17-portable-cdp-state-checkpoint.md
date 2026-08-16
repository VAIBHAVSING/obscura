# Portable CDP state checkpoint

## Source

- Commit: `30c8b0e` (`feat: add portable Rust CDP state ABI`)
- Node host exposure: `b5a3bc1` (`feat: expose portable CDP core through Node worker`)
- Package action routing: `e3f740b` (`feat: route package page actions through WASM CDP core`)
- Lifecycle hardening: `0e36ec7` (`feat: harden portable CDP target lifecycle`)
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
- Release `obscura-wasm` nextest: 56/56 passed, including nine CDP tests and
  action-error preservation.
- Release wasm-bindgen output exported `PortableCdp`, `cdpAbiVersion`,
  `openConnection`, `cdpRequest`, `completeAction`, and `pollCdpEvents`.
- Direct Node smoke against the generated wrapper passed browser version,
  target discovery, attachment, and event polling.
- The Node Worker now exposes bounded `portableCdpOpen`, `portableCdpRequest`,
  `portableCdpComplete`, `portableCdpPoll`, and `portableCdpClose` methods;
  the real generated wrapper test passed the full action round trip.
- The package suite against a fresh `--features render` wrapper passed 6/6
  runnable tests (one Playwright test skipped because no external
  `playwright-core` path was supplied); a non-render wrapper is intentionally
  rejected by the package's render capability gate.
- With an external `playwright-core` 1.62.1 installation, the full package
  suite passed 7/7, including Playwright CDP navigation, evaluation, cookie,
  screenshot, and PDF coverage after the WASM action route was enabled.
- Lifecycle coverage now proves deterministic auto-attach snapshots, target
  creation discovery ordering, detach/close event invalidation, stale action
  rejection, and preservation of host action errors as CDP error responses.

## Remaining integration

The package WebSocket adapter still owns its current JavaScript target/session
dispatch for domains not listed in the initial portable route. The next step
is to move DOM tree responses/mutations, Network/Fetch/IO, Input/Emulation,
and full page/context/session ownership into the shared WASM state. The
current checkpoint is an implemented and hardened core slice, not full CDP or
full browser parity.
