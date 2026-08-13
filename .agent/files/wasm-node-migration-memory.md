# Obscura Node and WebAssembly migration memory

Verified implementation commit: `40cd6948b93a403cfb1dc1722a2790060da83103`
(`fix: harden WASM harness verification`). The final harness and portable
boundary gates include the fail-closed CLI and WASM-audit regression fixes in
that commit.

## Decision

V8 137.3.0 cannot be compiled to `wasm32-unknown-unknown`. Its build requests
`librusty_v8_release_wasm32-unknown-unknown.a.gz`, which is not published, and
its source build has no wasm32 platform branch. V8 can execute WebAssembly, but
the V8 engine is not itself a supported WebAssembly payload.

The viable portable design remains:

```text
Node or Deno host and its V8 isolate
  page JavaScript and browser bootstrap
  host transport, timers, scheduling, and process limits
             |
             | versioned, batched ABI
             v
target obscura-wasm (planned full portable core)
  DOM, selectors, style, layout, portable state, and CPU paint
```

Node is the implemented feasibility-harness Worker host, not yet a production
browser host. The generated Deno bindings execute the same portable core, but
there is not yet a Deno Worker/browser integration. The separate
`obscura-node` experiment embeds a second, native V8 inside Node. It is a
native-fidelity fallback and proof of coexistence, not the host-V8 WebAssembly
architecture.

## Implemented feasibility slice

- `crates/obscura-wasm` reuses Obscura's parser, DOM tree, selectors, text
  access, and serialization through wasm-bindgen. Its negotiated ABI version is
  1. HTML input, selector input, and returned strings have explicit byte limits;
  invalid selectors are `SyntaxError`s and oversize values are `RangeError`s.
- The portable link sets a maximum of 4,096 WebAssembly pages, or 256 MiB. The
  generated binary defines one memory with that maximum; it does not import
  memory, networking, or browser objects.
- `node/wasm-v8-harness` loads the module in a persistent Worker, evaluates
  JavaScript with host Node V8, and exposes a small read-only `document` facade
  backed by the Rust/WASM DOM. ABI negotiation, realm isolation, request and
  evaluation limits, value serialization limits, deterministic disposal,
  startup cleanup, repeated replacement, and forced termination are covered.
- The harness now has an artifact-required gate. It refuses to treat a raw
  inspection-only WASM module as a successful executable backend, and the gate
  fails at startup if either the real wasm-bindgen wrapper or real native addon
  was not supplied.
- The Deno-target wasm-bindgen package was exercised directly in Deno with 176
  assertions over ABI/probe metadata, parsing, selectors, text and HTML,
  replacement, error types, exact and oversize limits, 100 reuse iterations,
  exports/imports, and the 4,096-page memory maximum.
- `tooling/wasm/audit.sh` checks the portable dependency boundary and classifies
  the native networking, paint, deno_core, and rusty_v8 blockers.
- `crates/obscura-node` is an experimental Node-API wrapper around the current
  native `ObscuraJsRuntime` and its separately initialized embedded V8.

This is still not a full browser, CDP endpoint, Puppeteer/Playwright
replacement, complete bootstrap/task bridge, complete rendering/resource host
split, or production Deno integration. The passing artifacts prove the bounded
DOM bridge and native-addon feasibility slices only.

## Build and verification commands

Portable release WASM and Node wrapper:

```bash
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 \
  cargo build --release -p obscura-wasm --target wasm32-unknown-unknown

/workspaces/.obscura-wasm-tools/wasm-bindgen-0.2.125-x86_64-unknown-linux-musl/wasm-bindgen \
  --target nodejs \
  --out-dir /workspaces/.obscura-wasm-pkg-final \
  target/wasm32-unknown-unknown/release/obscura_wasm.wasm

node node/wasm-v8-harness/bin/run.mjs \
  --module /workspaces/.obscura-wasm-pkg-final/obscura_wasm.js \
  --mode all
```

Deno wrapper and the disposable verification program used for the final audit:

```bash
/workspaces/.obscura-wasm-tools/wasm-bindgen-0.2.125-x86_64-unknown-linux-musl/wasm-bindgen \
  --target deno \
  --out-dir /workspaces/.obscura-wasm-deno-final \
  target/wasm32-unknown-unknown/release/obscura_wasm.wasm

deno run \
  --allow-read=/workspaces/.obscura-wasm-deno-final \
  /tmp/obscura-deno-audit.ts
```

The audit program is disposable evidence outside the repository, not a checked
in test. Its result was 176/176 assertions, and the assertion source remaining
at `/tmp/obscura-deno-audit.ts` has SHA-256
`bad3792d05a62d1daf3f41968069f25e14a9fbea2b24beb9664ebd7aa1489003`.
The Deno executable, version record, and command output were not retained, so
this is prior local feasibility evidence rather than a reproducible checked-in
gate.

Artifact-required Node tests:

```bash
cd node/wasm-v8-harness
OBSCURA_REAL_WASM_MODULE=/workspaces/.obscura-wasm-pkg-final/obscura_wasm.js \
OBSCURA_REAL_NATIVE_ADDON=/workspaces/obscura-node.node \
  npm run test:artifacts
```

Portable boundary audit:

```bash
tooling/wasm/audit.sh
tooling/wasm/audit.sh --require-full
```

The strict audit remains expected to fail until networking, paint resources,
deno_core, and V8 have portable replacements.

The final default audit exited 0 with all three portable checks passing. The
strict audit exited 1 with the four expected blockers: networking, paint,
deno_core, and V8.

Native embedded-V8 experiment on Linux:

```bash
CARGO_TARGET_DIR=/workspaces/.obscura-node-target \
  CARGO_INCREMENTAL=0 \
  CARGO_BUILD_JOBS=2 \
  V8_FROM_SOURCE=1 \
  GN_ARGS='v8_monolithic=true v8_use_external_startup_data=false v8_monolithic_for_shared_library=true' \
  cargo build --release --locked --manifest-path crates/obscura-node/Cargo.toml
```

The prebuilt rusty_v8 archive cannot link into a shared object because it uses
`R_X86_64_TPOFF32` local-exec TLS relocations. The source configuration above
produces `-fPIC` objects and defines `V8_TLS_USED_IN_LIBRARY` so V8 uses a
shared-library-safe TLS model. Keep `napi_build::setup()` because it emits
`-Wl,-z,nodelete` on Linux.

Release-mode Rust verification:

```bash
cargo nextest run --release -p obscura-wasm
cargo nextest run --release --features render -p obscura-js

cargo nextest run --release --features render --no-fail-fast

CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2 \
  cargo build --release -p obscura-cli --bins --features render
```

## Final artifact identity and proof

The artifacts checked with the implementation commit identified above are:

| Artifact | Bytes | SHA-256 |
| --- | ---: | --- |
| Raw release `obscura_wasm.wasm` | 1,320,633 | `8b7da34d472983cd7c6d0862357fcc4e1a7f1707407ba45ce08144a56f1ecbc7` |
| Node `obscura_wasm_bg.wasm` | 864,046 | `eff4ca2473dd87d14a054a7e55c73b3e798b68d9efa4e0ecbd9cd04d8425070f` |
| Node `obscura_wasm.js` | 10,856 | `45fd893922d906b344f3872e415720a491c89215aa381d1593a9d61acfb88d7e` |
| Deno `obscura_wasm_bg.wasm` | 864,046 | `eff4ca2473dd87d14a054a7e55c73b3e798b68d9efa4e0ecbd9cd04d8425070f` |
| Deno `obscura_wasm.js` | 10,464 | `27abf94ce85ce15d5411eace16c89ca2a30a4372435de04d27893cd6b745395a` |
| Native `obscura-node.node` | 68,819,424 | `c688203098f2c8b943a34a07cee46d3ad5d1bdd43e949051826e480dc827bcec` |
| Release CLI `obscura` | 93,518,104 | `80c47cace873713c841cffbe5c41b79a6eb6658bd3ffc14e47f915ab0589d5ed` |

The Node and Deno wrappers use the same 864,046-byte WASM payload. Binary
inspection and the Deno audit confirmed one defined memory with maximum 4,096
pages, equal to 256 MiB.

The current native addon is an x86-64 ELF shared object, dynamically linked and
not stripped. The final ELF audit found:

- the expected 25-symbol dynamic-export allowlist, including
  `napi_register_module_v1`, with no missing or unexpected exports;
- 38 TLS symbols, zero dynamic TLS symbols, 15 `DTPMOD64` relocations with no
  external targets, and zero forbidden `TPOFF32` relocations;
- zero collisions with Node or libnode exports, all 33 N-API imports resolved,
  and no V8/rusty_v8 undefined-symbol collision with the host;
- `BIND_NOW`, `NODELETE`, GNU RELRO, and a non-executable GNU stack, with no
  text relocations, rpath/runpath, or external V8/Node library dependency.

The sole undefined V8-pattern symbol is the weak TLS initializer
`_ZTHN2v88internal12trap_handler21g_thread_in_wasm_codeE`; it does not collide
with a host export.

The runtime proof used Node 24.14.0 with host V8 13.6.233.17-node.41 and
embedded rusty_v8 13.7.152.14-rusty. It confirmed that initialization is
required on the main thread and is idempotent; intrinsic JSON serialization
survives page overrides; infinite getters and `toJSON` hooks time out and the
runtime remains reusable; circular results fail cleanly; four concurrent and
ten sequential Worker lifecycles complete; one timed-out Worker does not poison
three peers or the main runtime; and 100 close-stress iterations produce 200
idempotent close calls plus 100 post-close rejections.

Recorded verification results:

- Deno portable-core audit: 176/176 assertions;
- ordinary Node harness run: 27/30 passed, with the 3 real-artifact tests
  skipped as expected when artifact paths are not supplied;
- artifact-required Node harness with both real artifacts: 30/30 tests, zero
  skipped;
- `obscura-js` release nextest: 279/279 tests;
- `obscura-wasm` release nextest: 1/1 test;
- full workspace release nextest with render: 1,393/1,393 executed tests passed,
  4 skipped, zero failed (run id
  `93fb8f73-563c-4705-8629-ed6c215145c9`);
- the exact release `obscura-cli` render build shown above passed.

The 33/33 obstacle course was not run because the companion
`/workspaces/obscura-benchmark` repository is not present in this workspace.

## Benchmark methodology and current result

`/workspaces/obscura-final-bench/final-head-with-cli.json` is the final
network-free run on a two-logical-CPU AMD EPYC 7763 host with Node 24.14.0. Its
SHA-256 is
`f9d0531be623b2973fde1f29ce599e8af13075f5f75342d4dffee1689d134676`.
The validated summary at
`/workspaces/obscura-final-bench/final-head-with-cli-summary.json` has SHA-256
`da0a7fe3cb29d57d54f48b418cb008b76048ddee6a7e39ef72a6327695e1066b`.

The run contains 75 raw samples: 25 fresh processes for each of portable WASM,
the native addon, and the release CLI. The three-way sequential order rotates
each round. The filesystem was preconditioned by hashing every artifact. Each
Node process used 20 warmups, 1,000 timed warm operations, and 25 churn
operations. Results use R-7 percentiles without outlier removal and fix
`TZ=UTC`, `LANG=C`, and `LC_ALL=C`. The summary validator covered both Node
backends; a separate raw-data check confirmed 25 unique, finite, positive CLI
samples across rounds 1 through 25.

Selected p50 / p95 values are:

| Measurement | Portable WASM | Native addon | Release CLI |
| --- | ---: | ---: | ---: |
| Process start to Worker ready | 111.221 / 342.242 ms | 115.330 / 230.549 ms | n/a |
| Worker module load | 7.171 / 15.778 ms | 0.510 / 2.476 ms | n/a |
| Process start to first backend result | 132.832 / 388.008 ms | 122.734 / 251.934 ms | n/a |
| Warm persistent operation | 0.567 / 1.459 ms | 0.281 / 0.507 ms | n/a |
| Create/replace or create/drop operation | 3.938 / 9.169 ms | 7.056 / 10.084 ms | n/a |
| Ready RSS | 67.00 / 69.00 MiB | 79.36 / 83.13 MiB | n/a |
| Full `data:` fetch, parse, eval, and exit | n/a | n/a | 32.967 / 116.793 ms |
| Approximate polled peak RSS | n/a | n/a | 31.93 / 32.44 MiB |

For the two Node backends, only the process-wide start-to-ready and ready-RSS
rows have directly comparable endpoints. The CLI row is a separate
full-process `data:` URL fetch, parse, `6 * 7` evaluation, and exit workload,
not the same endpoint as either Node first-result row. Node RSS is measured
directly; CLI peak RSS is Linux `/proc` `VmHWM` polled every 2 ms and can miss a
short-lived peak.

The native warm operation is a Worker round trip plus `6 * 7` in one persistent
embedded runtime, while the portable warm operation is a Worker round trip plus
an `h1` query in an already-loaded core and host realm. Their p50 rates were
3,560 and 1,765 operations per second respectively, but the work differs.
Native create/drop constructs and closes an embedded runtime; portable
create/replace constructs a core, disposes the prior document, and queries
`h1`. The throughput and churn rows are path measurements, not head-to-head
speed claims. The outer fresh-process totals are diagnostic because they
include every timed workload, shutdown, and JSON serialization.

The spike has not demonstrated an end-to-end browser cold-start or throughput
win. A persistent pool can amortize Worker/module startup, but chatty
JavaScript/WASM calls can be much slower. Keep the ABI batched and buffer-based,
and measure completed representative browser workloads interleaved against the
same revision before setting a performance target.

## Migration phases and acceptance gates

1. Stable host ABI and ownership
   - Extend the current feasibility ABI v1 negotiation into a stable
     full-browser ABI with integer handles, stable errors, bulk buffers, page
     reset, request/response queues, and deterministic disposal.
   - Reject stale and cross-page handles. No panic may cross wasm-bindgen or
     Node-API boundaries.

2. Host transport and profile state
   - Keep redirect, cookie, interception, proxy, and SSRF policy in portable
     Rust where practical. Execute sockets and fetch in Node or Deno.
   - Validate initial, redirected, and rewritten URLs. Preserve private-network
     blocking unless explicitly enabled.

3. DOM, bootstrap, and task bridge
   - Run the existing browser bootstrap in host V8 and replace deno_core ops
     with a generated batched adapter.
   - Preserve mutation argument order, cycle guards, traversal limits,
     microtask/timer/network/render ordering, and forced termination.

4. Render and resource split
   - Keep style/layout in portable Rust. Move image, SVG, font, CSS, time, and
     randomness acquisition behind host contracts.
   - Return paint/PDF buffers with explicit ownership and release.

5. Browser and CDP compatibility
   - Add Page, targets, sessions, lifecycle, and CDP over the completed runtime.
   - Preserve strict fields such as `canAccessOpener` and validate Puppeteer and
     Playwright flows.

6. Production hardening
   - Use a persistent Worker pool, explicit deadlines and memory/body limits,
     cancellation, page isolation, and clean shutdown.
   - Run focused and full release nextest, the exact CLI build, obstacle course
     33/33, WPT subtest comparisons, rendering fixtures, and interleaved latency
     and resource benchmarks.

Migration completion requires all phases. The current successful artifacts and
tests do not satisfy the full-browser acceptance gates.


---

# 2026-08-11 follow-up: stateful WASM DOM bridge parity audit and tests (2026-08-11 16:28:24 UTC)

Implementation commit: `2da61c9` on `wasm-node-migration` (parent
`da3b1d4`); evidence commit `fbdc200`. Only `crates/obscura-wasm/src/lib.rs`
changed (source), plus this memory file.

## What changed

1. Page revision now mirrors native `render_mutation_impact`:
   `dom_op_inner` consults `mutation_effectiveness()` (pre-op state) and
   advances `page_revision` only for effective mutations. No-op attribute
   writes (same value / missing attribute), same-value text writes,
   rejected cycle moves (append_child / insert_before mirror the tree's
   host-including cycle guard via `ancestor_chain_contains`), and all
   create/clone/template-content allocations never bump. Wire results
   (`"true"`/`"false"`) are unchanged. Revision overflow fails safely
   before mutating; read-only ops keep working at the boundary.
2. `mod tests` grew from 4 to 39 tests:

   - ABI: versions 1/1/1/1, `probe()` capabilities including the
     previously-unasserted `documentMetadataAbiVersion`, document identity,
     metadata round-trip, strict batch envelope validation.
   - 63-command surface: canonical `ALL_DOM_COMMANDS` manifest
     source-locked against the dispatcher arms (charset-validated,
     sorted/deduped) plus a smoke test hitting every command and asserting
     its contract shape.
   - Handles: same-node stability, per-node uniqueness over a full BFS,
     detached/reparented retention, clone uniqueness, set_html
     invalidation of every previous handle, never-reuse across resets,
     controlled errors for invalid/zero/stale handles, allocation overflow
     fail-safe, cycle rejection + 3000-deep traversal termination, template
     content fragment handles (lazy, stable, cloned, reset-invalidated).
   - Revision: full matrix incl. no-op writes, already-last append,
     already-immediately-before insert, cycle-rejected moves, element
     textContent no-op, creation ops, set_html exactly once, batch
     revision = effective ops only, overflow fail-safe.
   - Parity: id index (detach cleans subtree ids, reparent restores via
     fallback scan, plain remove_attribute leaves a stale entry and
     rewrite-with-missing-attr cannot purge it - both native quirks
     verified against ops.rs and tree.rs), namespace attributes, selector
     error degradation, contains() self=false quirk, insert_before
     (new, reference) argument order, connected/root semantics,
     doctype/PI serialization and cloning, compare_order.
   - Limits/failure: 1024/1025 batch ops (pre-execution rejection),
     8MiB/64KiB/64-byte caps with UTF-8 byte-length semantics,
     malformed/truncated/structurally-invalid batch JSON never applies a
     prefix, reuse-after-failure, panic-boundary translation (js_sys
     cannot construct errors on native test hosts; the panic arm is
     asserted via the documented js_sys limitation and the real module
     exercises the same arm on wasm32 in Node).

## Verification evidence

- `cargo-nextest nextest run --release -p obscura-wasm`: **39/39 PASS**.
- `npm test --prefix node/wasm-v8-harness`: **34 pass, 0 fail, 3 skip**
  (real-artifact tests skipped without env).
- wasm32 release build:
  `target/wasm32-unknown-unknown/release/obscura_wasm.wasm`,
  1,497,297 bytes, sha256
  `07df9e353f2b48bafacd74062ae0bb613a93d45803cea68242d50df42a30e09f`.
- wasm-bindgen wrapper generated into `/workspaces/.obscura-wasm-pkg-stateful/`
  (previously missing; `/workspaces/.obscura-wasm-pkg-final/` holds the
  outdated wrapper).
- Real-artifact suite
  (`OBSCURA_REAL_WASM_MODULE=...stateful/obscura_wasm.js
  OBSCURA_REAL_NATIVE_ADDON=/workspaces/obscura-node.node
  npm run test:artifacts --prefix node/wasm-v8-harness`): **37/37 PASS**,
  including the production-bootstrap-vs-stateful-bridge test, the real
  wasm-bindgen ABI-1/SyntaxError test, and bridge ABI limit enforcement.
- Disposable real-module check (`/tmp/real-module-verify.mjs`, not
  committed): **42/42 PASS** - 8MiB/64KiB/1024-op byte boundaries (UTF-8
  byte semantics), revision semantics through the wrapper, batch order +
  effective revision, setDocumentMetadata identity stability, set_html
  invalidation and document-handle never-reuse, id index reindex, doctype
  nodeType.
- Full gates (delegated subagent):
  - `cargo-nextest nextest run --release --features render --no-fail-fast`:
    **1434 passed, 4 skipped, 0 failed** (release compile 17m42s + 55.2s
    exec); all 39 obscura-wasm tests included; zero wasm-related warnings.
  - `CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2 cargo build --release -p
    obscura-cli --bins --features render`: exit 0, 2m28s;
    `./target/release/obscura` 93,518,056 bytes, sha256
    `c3c184b2cb531e7cc49bdf06e7d75bbac5b8029e3968451445c354aa0ea98a70`.
- **External blocker**: the companion `obscura-benchmark` repository is not
  present in this workspace, so the 33/33 obstacle course cannot run. It
  must be reported as an unavailable gate, not a passed one.

## Debugging note (real-module verification)

Passing a JS number (e.g. `JSON.parse(...).nodeId`) instead of a string to
`domOp` corrupts the wasm heap: wasm-bindgen's `passStringToWasm0` coerces
it through `encodeInto` into a zero-length view, and the trap surfaces
later as `__rdl_realloc: memory access out of bounds` in an unrelated call.
Always `String(...)` handles before crossing the ABI. The module itself was
not at fault; artifact tests passed unchanged.

## Follow-ups

- Browser-page migration (networking, timers, script loading, rendering,
  Page/CDP, edge adapters) remains separate and is not claimed here.
- Obstacle course 33/33 must be run in the companion repo when available.

## Shared portable platform operations (2026-08-13)

Implementation commit: `2dc3db0` (`feat: share portable platform operations`)
on `wasm-node-migration`; it follows the committed Node/bootstrap bridge
`22d482d`. The feature is pushed to `origin/wasm-node-migration`.

### What changed

- Added `crates/obscura-platform`, a target-neutral semantic source shared by
  native `obscura-js` and portable `obscura-wasm`. It owns URL parsing/set/
  resolution, `document.domain` Public Suffix validation, legacy encoding
  labels/decoding/query encoding, and deterministic WebCrypto primitives
  (digest, HMAC, AES-GCM/CBC/CTR, PBKDF2, HKDF).
- `obscura-wasm` exposes a separately negotiated `platformOpAbiVersion() = 1`
  and a bounded JSON/base64 `platformOp(command, request)` bridge for the 15
  synchronous bootstrap operations. This remains separate from the stateful
  DOM handle/revision ABI.
- The host Node VM binds the same exact operations into page-realm
  `Deno.core.ops`, without leaking Node globals or host callbacks.
- `random_bytes` keeps entropy target-owned: the native/WASM boundary supplies
  `getrandom`, while the shared crypto crate has no entropy feature. In
  particular, `aes-gcm` is built with `default-features = false` and only
  `aes,alloc`, preventing an accidental standalone WASM entropy dependency.
- Platform limits: command 64 B, JSON request/response 12 MiB, binary input
  and output 8 MiB, random output 64 KiB, strings 1 MiB, KDF output 1 MiB.
  PBKDF2 also limits `iterations * ceil(outputBytes / digestBytes)` to
  1,000,000 work units. This closes the prior amplification path where valid
  independent iteration/output limits could require billions of HMAC blocks.

### Verification evidence

- `/workspaces/.obscura-tools/nextest/cargo-nextest nextest run --release
  -p obscura-platform -p obscura-wasm`: **54/54 PASS** (run id
  `b90495ed-70bd-4a63-ac9b-c1805d803474`).
- `CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2 cargo check --release
  -p obscura-platform --target wasm32-unknown-unknown`: PASS. This verifies
  the shared crate itself, not merely feature unification through the final
  WASM package.
- `CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2 cargo build --release
  -p obscura-wasm --target wasm32-unknown-unknown`: PASS. The raw binary is
  3,309,395 B, SHA-256
  `121810539ae16d70a266ec689af8a29ed5058b1fab22bdfdf9f42972bd95d486`.
  A Node wasm-bindgen wrapper generated in
  `/workspaces/obscura-platform-pkg.X0nw4m/` has a 2,845,598-B WASM payload,
  SHA-256 `4b248ee2804737020d1cd1627000782af3c2766c2c18d6662706457d9b597ef4`.
- `npm test --prefix node/wasm-v8-harness`: **39 pass, 0 fail, 3 optional
  real-artifact skips** after the PBKDF2 host-side preflight bound.
- Real backend suite with the generated wrapper and
  `/workspaces/obscura-node.node`: **42/42 PASS**, no skips. It covered the
  real platform bootstrap operations, host/WASM request/response limits,
  DOM bridge, embedded-V8 timeout/reuse, and isolation lifecycle.
- `/workspaces/.obscura-tools/nextest/cargo-nextest nextest run --release
  --features render -p obscura-js -p obscura-net`: **443/443 PASS** (run id
  `44e389f6-5898-4d46-ac47-9e5388ea5f62`). Existing warnings were in vendored
  `cosmic-text` and a pre-existing unused test import in `obscura-net`.

### Delivery decision and next step

The user explicitly deferred the standalone npm/Playwright/npx package.
No npm package launcher or CDP sidecar code was added in this milestone.
When resumed, it should be a package-owned CDP sidecar exposing explicit
ready/actual-port/shutdown lifecycle APIs; it must not wrap the existing Rust
CLI or the evaluate-only native addon.

The active portability prerequisite is now browser task scheduling: host
`queueUserTimer`/`cancelTimer`, posted-task wakeups, cancellation, reset and
close behavior, task/microtask ordering, and deadline-aware Worker teardown.
Only after that should parser and dynamic script execution be ported.
