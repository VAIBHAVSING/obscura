# Portable CDP state checkpoint

## Source

- Commit: `30c8b0e` (`feat: add portable Rust CDP state ABI`)
- Node host exposure: `b5a3bc1` (`feat: expose portable CDP core through Node worker`)
- Package action routing: `e3f740b` (`feat: route package page actions through WASM CDP core`)
- Lifecycle hardening: `0e36ec7` (`feat: harden portable CDP target lifecycle`)
- DOM state routing: `7d7af36` (`feat: route portable DOM CDP state through WASM`)
- Cookie/storage routing: `5e229b3` (`feat: route portable cookie CDP state through WASM`)
- Emulation/layout routing: `bced515` (`feat: route portable emulation state through WASM`)
- History/reload routing: `1638cef` (`feat: route portable page history through WASM`)
- Runtime command routing: `ad98fc1` (`feat: route portable runtime commands through WASM`)
- Navigation-header routing: `3155ca8` (`feat: route portable navigation headers through WASM`)
- Input action routing: `7b06f8c` (`feat: route portable input events through WASM`)
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
- Cookie/storage reads and writes, viewport/layout emulation, navigation history,
  reload, Runtime remote-object operations, extra navigation headers, and basic
  mouse/keyboard/text input are dispatched by Rust and completed by bounded
  Node host actions where page/V8 work is required.
- ABI version `cdpAbiVersion: 1` is included in the portable capability probe.
- JSON message, event, action-result, method, and session limits are bounded.
- Connection close removes sessions, events, and pending actions.

## Verification

- `cargo check -p obscura-wasm`: passed.
- `cargo check --release -p obscura-wasm --target wasm32-unknown-unknown`:
  passed.
- Release `obscura-wasm` nextest: 61/61 passed, including CDP lifecycle, DOM,
  cookie, emulation, history, runtime-action, header, and input coverage.
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
  screenshot, PDF, emulation, input, and header coverage against a fresh
  render-enabled WASM wrapper.
- Lifecycle coverage now proves deterministic auto-attach snapshots, target
  creation discovery ordering, detach/close event invalidation, stale action
  rejection, and preservation of host action errors as CDP error responses.
- Rust now serves `DOM.getDocument`, selector queries, outer HTML, attributes,
  node descriptions, and `DOM.setChildNodes`; the Node adapter seeds the Rust
  target from the live WASM document, synchronizes committed navigation and
  `Page.setDocumentContent`, and translates the worker-local page-1/frame IDs
  to package target IDs. The real-artifact test covers these paths.
- A local HTTP fixture proves Rust-owned `Network.setExtraHTTPHeaders` reaches
  the Node fetch host. The same fresh-artifact test proves Rust-owned mouse and
  keyboard actions reach page listeners.

## Remaining integration

The package WebSocket adapter still owns JavaScript transport, package-level
target/context bookkeeping, network event/Fetch/IO plumbing, and fallback
domains not listed in the portable route. DOM mutation synchronization, full
Network/Fetch event state, stream ownership, and complete page/context/session
parity remain. This checkpoint is a substantially routed Rust/WASM CDP core,
not yet full CDP or full browser parity.
