# Packet C3: reuse the existing Obscura CDP implementation inside WASM

## Goal

Compile the existing `obscura-cdp` protocol, target/session state and domain
dispatch into the final `obscura-wasm` browser binary. Remove operating-system
server responsibilities from the portable build. The npm package supplies
WebSocket/HTTP transport, Node V8, network I/O, clocks and persistence locations;
it must not duplicate CDP or browser semantics.

This packet supersedes the earlier recommendation in Packet C1 to re-express a
separate portable CDP implementation. The source of truth must become the
existing `obscura-cdp` domain code. The current `PortableCdp` in
`crates/obscura-wasm/src/cdp.rs` is migration scaffolding and is deleted after
parity and cutover.

## Current implementation checkpoint

The first foundation slice is now present in the working tree: `protocol.rs`
and `action.rs` are transport-free and compile for `wasm32-unknown-unknown`,
`engine.rs` defines the data-only `CdpEngine` contract, and the WASM dispatcher
uses the shared protocol limits and action queue. Native TCP/WebSocket code is
behind the explicit `native-server` feature; the workspace CLI opts into that
feature while the WASM dependency disables all CDP defaults. This is not the
portable browser cutover yet: target/session state, domain dispatch, and the
existing `PortableCdp` replacement remain in the packets below.

## Product boundary

```text
Playwright / Puppeteer / raw CDP
                 |
                 | WebSocket text frames
                 v
npm host adapter
  - HTTP/WebSocket transport
  - Node V8 realms
  - fetch/DNS/TLS
  - timers and Worker isolation
  - chooses filesystem/database/KV persistence location
                 |
                 | bounded request/action/completion ABI
                 v
single obscura WASM browser module
  - reused obscura-cdp parser and domain dispatch
  - browser contexts, targets, sessions and event ordering
  - navigation/cookie/cache policy
  - DOM, CSS, layout, paint, PNG and PDF
  - response bodies, remote objects and IO stream state
```

The npm adapter may store opaque browser-context snapshots, but logical context
state and identity remain in WASM. Moving target/session/cookie/cache semantics
into JavaScript would split the source of truth and defeat this migration.

## Current source audit

`crates/obscura-cdp` currently contains about 11,130 Rust lines:

- `server.rs` (1,988 lines) is OS transport: TCP listeners, blocking accept
  threads, Tokio, Tungstenite, signals and connection-thread management. It is
  moved behind a native-only feature and never compiled for the portable
  target.
- `dispatch.rs` (749 lines) contains the reusable command router, but
  `CdpContext` directly owns native `obscura_browser::Page`, `BrowserContext`,
  Tokio channels and a V8 mutex.
- `domains/*.rs` (about 8,200 lines) contains the valuable existing CDP
  behavior. Most handlers use `Page` or `BrowserContext` directly and are
  async only because the native backend is async.
- The current crate manifest unconditionally pulls `obscura-browser`,
  `obscura-js`, `obscura-net`, Tokio and Tungstenite, which pulls native V8 and
  OS networking into every build.
- `crates/obscura-wasm/src/cdp.rs` already proves the required host-action
  model, but duplicates a subset of existing CDP behavior.

## Target crate layout

Keep the package name `obscura-cdp`; reuse it rather than introducing a second
protocol implementation.

```text
crates/obscura-cdp/
  protocol.rs        request/response/event types and validation
  state.rs           contexts, targets, sessions, IDs, queues and limits
  engine.rs          target-neutral CdpEngine interface
  action.rs          bounded asynchronous host-action state machine
  dispatch.rs        existing shared command router
  domains/*.rs       existing domain implementations using CdpEngine
  native/
    server.rs        temporary legacy native transport feature
    engine.rs        temporary native Page/BrowserContext adapter

crates/obscura-wasm/
  cdp_engine.rs       CdpEngine implementation over portable page state
  lib.rs              wasm-bindgen/raw-byte ABI exports

node/obscura/
  cdp transport       WebSocket framing and HTTP discovery only
  action host         V8/network/timer/persistence action execution
```

During migration, use features:

```toml
[features]
default = ["portable"]
portable = []
native-engine = ["dep:obscura-browser", "dep:obscura-js", "dep:obscura-net", "dep:tokio"]
native-server = ["native-engine", "dep:tokio-tungstenite", "dep:futures-util"]
render = []
```

All native dependencies must be optional and target-gated. `obscura-wasm`
depends on `obscura-cdp` with `default-features = false, features = ["portable"]`.
After npm acceptance and parity, delete `native/server.rs`, the native engine
adapter and native features if the project no longer needs a Rust executable.

## Portable engine interface

Domain handlers must depend on behavior, not native objects. Start with a
target-neutral interface whose results are immediate values or explicit host
actions:

```rust
pub trait CdpEngine {
    fn create_context(&mut self, options: ContextOptions) -> Result<ContextId, CdpFailure>;
    fn dispose_context(&mut self, id: &ContextId) -> Result<(), CdpFailure>;
    fn create_page(&mut self, context: &ContextId, url: &str) -> Result<PageId, CdpFailure>;
    fn close_page(&mut self, page: &PageId) -> Result<(), CdpFailure>;
    fn page_snapshot(&self, page: &PageId) -> Result<PageSnapshot, CdpFailure>;
    fn start_action(&mut self, action: EngineAction) -> Result<ActionId, CdpFailure>;
    fn complete_action(&mut self, id: ActionId, result: ActionResult) -> Result<(), CdpFailure>;
}
```

Do not use `async_trait`, futures executors or Tokio in the portable core.
Network and V8 work become bounded records in an action queue. Completion
re-enters the same Rust state machine and produces the final CDP response and
events.

Initial host action kinds:

- `Navigate` and `FetchResource`
- `Evaluate`, `CallFunctionOn`, `GetProperties` and remote-object release
- classic/module script fetch and module resolution
- input delivery requiring the page realm
- context persistence load/store
- host clock/timer wake only where the existing portable task runtime cannot
  complete synchronously

Screenshot, PDF, DOM inspection, layout and paint should complete entirely in
WASM and must not become Node browser logic.

## Low-level WASM ABI

CDP is already UTF-8 JSON, so preserve its bytes and avoid parsing/rebuilding it
in JavaScript. The first stable ABI uses `Uint8Array`; a later shared-memory
fast path may remove copies without changing semantics.

```text
cdp_abi_version() -> 2
browser_create(config_bytes) -> browser_id
browser_close(browser_id)
connection_open(browser_id) -> connection_id
connection_close(connection_id)
cdp_ingest(connection_id, request_bytes) -> output_batch
cdp_drain_actions(browser_id, max_actions, max_bytes) -> action_batch
cdp_complete_actions(browser_id, completion_batch) -> output_batch
cdp_drain_events(connection_id, max_events, max_bytes) -> output_batch
context_export(browser_id, context_id) -> snapshot_bytes
context_import(browser_id, snapshot_bytes) -> context_id
```

`output_batch` contains complete CDP response/event JSON frames with a compact
length-prefixed envelope. JavaScript sends those bytes without inspecting CDP
fields. Pure commands cross JS/WASM once. Host-action commands cross once to
start and once to complete. Batched completion prevents one boundary crossing
per resource when many fetches finish together.

Required bounds:

- maximum request/message bytes
- maximum output batch bytes and frame count
- maximum queued actions/events per browser and connection
- maximum contexts, pages, sessions, remote objects and streams
- maximum snapshot bytes and explicit snapshot schema version
- monotonic non-reused IDs with controlled exhaustion
- deterministic cleanup of actions, streams and realms on page/context/
  connection/browser close

## Browser-context persistence

The npm package decides *where* state is stored. WASM decides *what* the state
means.

1. WASM exports a versioned opaque context snapshot containing portable
   cookies, storage, permissions, cache metadata and browser preferences.
2. Node stores those bytes in a caller-selected directory, database or remote
   KV provider.
3. Node returns the exact bytes to `context_import` on startup.
4. Encryption, filesystem permissions and atomic file replacement belong to
   the npm persistence provider.
5. Target/session IDs, pending actions, V8 remote-object handles and live
   network requests are never persisted.

## Implementation packets

Each packet is intentionally small enough for one coding agent and ends with a
reviewable commit.

### C3.0 Freeze parity evidence

- Record normalized native request/response/event transcripts for all existing
  `obscura-cdp` domain tests and the Playwright startup handshake.
- Add a reusable corpus runner that can feed the same requests to native and
  portable backends.
- Record current latency, RSS and concurrency baselines before refactoring.

Acceptance: the corpus is deterministic and fails when session IDs, event
ordering, strict fields such as `canAccessOpener`, or errors diverge.

### C3.1 Make protocol types portable

- Move request/response/event types, JSON validation, limits and ID allocators
  into transport-free modules.
- Remove Tokio and browser imports from those modules.
- Add malformed JSON, oversized message, ID exhaustion and queue-limit tests.

Acceptance: `cargo check -p obscura-cdp --no-default-features --features portable
--target wasm32-unknown-unknown` reaches the dispatcher rather than an OS/V8
dependency failure.

### C3.2 Isolate and then remove native transport

- Move `server.rs` behind `native-server` so it is excluded from portable
  compilation.
- Move TCP, Tokio, Tungstenite, signals, threads and filesystem persistence out
  of the crate's default graph.
- Keep a temporary native adapter only for parity comparisons.

Acceptance: the portable dependency tree contains no Tokio net, Tungstenite,
`mio`, native TLS, `obscura-net`, `deno_core` or `rusty_v8`.

### C3.3 Replace `CdpContext` native ownership

- Convert `pages: Vec<Page>` and `BrowserContext` maps to stable IDs plus
  portable metadata.
- Replace V8 mutex and interception channels with action/event queues.
- Preserve loader, session, isolated-world, target and execution-context ID
  behavior from the existing implementation.

Acceptance: Browser and Target corpus tests pass against the shared core, with
multi-context and multi-session cleanup tests.

### C3.4 Port pure/stateful domains first

Migrate existing handlers without rewriting their external semantics:

- Browser
- Target
- IO
- Storage
- Network state/cookies/cache controls
- Fetch pattern and paused-request state
- Emulation and page history

Acceptance: normalized native/portable responses and events match for every
covered method.

### C3.5 Connect DOM and rendering domains to the portable engine

- Adapt existing DOM, DOMSnapshot, Accessibility and Input handlers to
  `ObscuraCore` node handles and page revision.
- Adapt screenshot and PDF handlers to the existing WASM renderer/PDF APIs.
- Keep response streams and capture metadata in shared CDP state.

Acceptance: the same deterministic page produces equivalent normalized DOM,
layout, screenshot and PDF CDP results through the reused handlers.

### C3.6 Convert Runtime and Page async paths to actions

- Convert navigation, reload, evaluation, remote objects, isolated worlds,
  preload scripts, script/module loading and lifecycle events into explicit
  action continuations.
- Completion validates browser/page/document generation before applying a
  result.
- A stale or duplicate completion returns a controlled error and cannot mutate
  a replacement document.

Acceptance: concurrent navigation and evaluation never block CDP control
commands and maintain the current event order.

### C3.7 Implement the WASM engine adapter

- Add `obscura-cdp` portable dependency to `obscura-wasm`.
- Implement `CdpEngine` over the existing portable DOM/navigation/cookie/
  render/page state.
- Expose the ABI above with panic containment and exact byte/count limits.
- Instantiate multiple browser contexts and pages inside one WASM instance.

Acceptance: one real wasm-bindgen artifact handles at least two contexts, eight
pages and multiple flattened sessions without JavaScript-owned browser state.

### C3.8 Cut over from `PortableCdp`

- Route the npm package to the reused `obscura-cdp` exports.
- Run both implementations temporarily against the same corpus and compare
  normalized output.
- Delete `crates/obscura-wasm/src/cdp.rs` only when all currently supported
  commands and events pass through the shared implementation.

Acceptance: no portable CDP command is implemented separately in Node or in a
second Rust dispatcher.

### C3.9 Thin npm transport and host actions

- Keep HTTP discovery and WebSocket framing in npm.
- Forward request bytes into WASM and returned frame bytes to the socket.
- Execute only typed host actions: Node V8, fetch/DNS/TLS, timers and context
  snapshot persistence.
- Apply backpressure and bounded action concurrency per browser/context/origin.

Acceptance: deleting JavaScript CDP routing tables does not remove any browser
capability; JavaScript contains transport/action adapters only.

### C3.10 Concurrency and memory model

- Compile `WebAssembly.Module` once and reuse it.
- Default to one persistent Worker/WASM instance containing multiple contexts
  and pages, rather than one process or module compilation per page.
- Allow an optional Worker pool for isolation; consistently hash a context to
  one Worker so its V8 realms and WASM state stay local.
- Batch event drains and action completions; never poll in a busy loop.
- Enforce global and per-context network/action/page limits.

Acceptance: benchmarks cover 1, 8, 32 and 128 concurrent pages, report p50/p95/
p99 command latency, completed pages/sec, peak RSS and bytes per idle page, and
show no unbounded queue growth under a slow CDP client.

### C3.11 Remove native browser/server code

- Stop building the Rust TCP/WebSocket CDP server for the npm product.
- Remove native Page/BrowserContext adapters after parity evidence is retained.
- Remove native V8, Tokio-net and native TLS from the final npm/WASM dependency
  graph.
- Keep native sources only if another explicitly supported product requires
  them; they must not be in the npm build or package.

Acceptance: a clean npm package build compiles only `wasm32-unknown-unknown`,
contains no native executable or `.node` file, and runs on a machine without the
native Obscura CLI.

### C3.12 Final compatibility gates

- Existing `obscura-cdp` unit/domain corpus through the shared core.
- Full `obscura-wasm` release tests and real artifact tests.
- Playwright: connect, contexts, pages, navigation, evaluate, locator click,
  fetch/XHR, screenshot, PDF, close and reconnect.
- Puppeteer equivalent smoke.
- Request interception, response interception, cookies, modules and resource
  loading under concurrency.
- Worker termination/restart and context snapshot restore.
- Full repository render nextest and required deterministic render fixtures.
- Obstacle course 33/33 when the companion repository is available.

## Performance gates

Measure before optimizing. Exclude network/server time when measuring dispatch.

- One JS-to-WASM crossing for pure CDP commands.
- At most one start and one batched completion crossing for host-action commands.
- No JSON parse in JavaScript on the normal CDP transport path.
- No per-command Worker, WASM instance, V8 realm or allocator recreation.
- Warm cached `WebAssembly.Module` is reused by all browser instances.
- Reused-core control-command latency and memory must not regress by more than
  the repository's 10% noise floor versus the current portable core.
- Throughput must scale until CPU or configured network concurrency is
  saturated, not serialize on a process-wide mutex.

Retain raw benchmark samples and artifact/source hashes. Do not claim a speedup
from different workloads.

## Completion definition

This migration is complete only when:

1. `obscura-cdp` itself builds for `wasm32-unknown-unknown` without native
   engine/server dependencies.
2. The final `obscura-wasm` binary uses that crate as its only CDP dispatcher.
3. npm contains transport and host adapters but no duplicate CDP/browser state.
4. The package contains no native Linux executable, native addon or embedded
   V8.
5. Real Playwright and Puppeteer acceptance flows pass against the packaged
   artifact.
6. Concurrency, memory, cleanup and bounded-queue gates pass with retained
   evidence.

## Copyable implementation prompt

```text
Implement exactly one C3 packet from
.agent/files/remaining/18-reuse-obscura-cdp-in-wasm.md.

Read AGENTS.md and the whole C3 plan first. Reuse the existing
crates/obscura-cdp domain implementation; do not create another CDP dispatcher.
The portable build may not depend on obscura-browser, obscura-js, obscura-net,
Tokio networking, Tungstenite, deno_core or rusty_v8. Express asynchronous V8
and network work as bounded typed host actions with generation-safe completion.
WASM owns browser contexts, targets, sessions, cookies, DOM/render state and
event ordering. npm owns transport, Node V8, network I/O and the location of
opaque context snapshots. Add focused parity and cleanup tests. Do not edit
README/docs/root index.js or unrelated files. Do not commit unless requested.
Report exact files, commands, results and remaining gaps.
```
