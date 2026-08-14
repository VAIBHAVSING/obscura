# Packet H: security, limits, isolation and performance

## Goal

Make the Node/WASM browser safe to operate as an npm process and future edge
workload. This packet is continuous: each earlier packet adds focused limits;
the final pass proves they compose correctly.

## Dependencies

Continuous. Final acceptance follows C3 and K1.

## Small tasks

### H1. Threat model and trust boundaries

Treat page HTML/JS, URLs, response bytes, CDP clients and package options as
untrusted. Record for each boundary:

- accepted primitive/buffer types
- maximum input/output/work
- cancellation owner
- lifetime/identity token
- error form
- proof that host objects/functions cannot cross

Reject capability presence when method versions or required peers mismatch.

### H2. VM realm containment

- Page code cannot reach `process`, `require`, `Buffer`, module loader, host
  callbacks, Worker ports or WASM instance internals.
- Perform coercion, getters, thenable handling, serialization and error
  sanitization inside the bounded VM script.
- Never inspect page-controlled objects after the VM timeout ends.
- Temporary bindings are non-enumerable, randomized, generation-bound and
  deleted before invoking page callbacks.
- Capture pristine intrinsics in each fresh realm.

Node `vm` is not a complete security boundary. For hostile multi-tenant use,
the Worker/process and OS/edge runtime limits remain mandatory.

### H3. Resource/work limits

Cover:

- Worker heap/resource limits
- WASM maximum memory
- HTML, selector, ABI JSON and returned graph sizes
- navigation/resource/request/response bytes
- redirect/import/module/tree depth and count
- CSS/font/image decode work
- screenshot/PDF pixels and bytes
- CDP events, remote objects, IO streams and WebSocket queues
- crypto KDF combined work
- page/context/connection counts

Limits must be checked before multiplication/allocation and tested at exact
boundary plus one.

### H4. Cancellation and teardown

- One absolute request deadline plus phase-specific deadlines.
- VM execution timeout and Worker hard termination backstop.
- Page navigation/reset cancels fetch, modules, timers, tasks, CDP actions and
  resource seeding for the old generation.
- Close is idempotent and waits for Worker termination.
- Fatal paths reject all pending requests exactly once.

### H5. Panic/trap policy

- No Rust panic crosses wasm-bindgen.
- Remember release wasm uses `panic=abort`; `catch_unwind` is not a substitute
  for validating panic-prone input before calling libraries.
- Run risky decoders/large work only after strict validation; if a third-party
  decoder can trap on untrusted bytes, isolate the page Worker so termination
  cannot corrupt peers.
- A WASM trap makes that Worker/page unusable and forces deterministic reset.

### H6. Performance contract

- Compile/load WASM once per Worker; reuse long-lived pages/Workers.
- Prefer batched operations and copied bulk buffers over chatty scalar calls.
- Bound fixed-point resource discovery and avoid repeated full-tree scans.
- Maintain a Worker pool only after isolation tests prove cleanup.
- Benchmark identical end-to-end workloads, interleaved, with distributions.
- Separate cold process, Worker-ready, first navigation, warm navigation,
  screenshot and PDF timings.

### H7. Adversarial tests

- Infinite loops/getters/thenables/toJSON/proxy traps.
- WASM trap and Worker replacement.
- Zip/decompression and image/font bombs within fixture limits.
- Redirect/CSS import/module cycles.
- Oversize/fragmented WebSocket and slow clients.
- Stale identity races across navigation/release.
- OOM-pressure fixture in a disposable child process.
- Peer-isolation test while one Worker times out or traps.

## Acceptance criteria

- Every externally reachable operation has documented bounds and cancellation.
- Host escape regressions fail closed.
- One hostile page cannot hang or corrupt peer pages.
- Performance is reported honestly; no ratio compares different workloads.

## Copyable LLM prompt

```text
Audit or implement one H task from
.agent/files/remaining/13-security-performance.md. Work from an explicit threat
model. Find a concrete bypass or add a focused boundary test; do not make broad
speculative rewrites. All page-controlled coercion/serialization must occur
inside a VM deadline. Treat a WASM trap as fatal to that Worker. Do not edit
docs/README/.agent/index.js or commit unless told. Report exploit/repro,
severity, patch, and exact regression evidence.
```
