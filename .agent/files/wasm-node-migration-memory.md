# Obscura Node and WebAssembly migration memory

## 2026-08-25 TypeScript browser package checkpoint

This section supersedes the old Node package layout and native-addon guidance
below. The implementation is on branch `wasm-node-migration`.

### Current package architecture

- The root is an npm workspace (`packages/*`) with strict TypeScript builds.
- `packages/browser` is the public `@obscura/browser` package. Its default
  `createBrowser()` API starts the bundled WASM browser in the Node process and
  opens no listening port. Each page Worker is a Node V8 isolate boundary and
  its page realm is a separate `vm` context.
- The package exports `@obscura/browser/cdp`, `/storage`, `/s3`, `/transport`,
  `/puppeteer`, and `/playwright`. Puppeteer uses the in-memory CDP transport.
  Playwright lazily opens a loopback CDP listener because its public connection
  API requires an endpoint. An external port is otherwise opt-in.
- `packages/protocol` owns the bounded, versioned request/event transport for a
  future remote browser service. The public browser API is intentionally
  placement-neutral even though only local placement is implemented today.
- `packages/storage` owns the generic `ProfileStore`, atomic/versioned local
  storage, encrypted profile containers, and a SigV4 S3-compatible adapter.
  Any object store can be supported by implementing `ProfileStore`; the API is
  not coupled to AWS.
- `packages/runtime` is the private TypeScript Worker/V8 host. The previous
  `node/obscura` and `node/wasm-v8-harness` JavaScript trees were migrated into
  the workspace. `crates/obscura-node` and all native-addon discovery/config
  paths were removed. `.node` inputs now fail with
  `ERR_OBSCURA_NATIVE_UNSUPPORTED`.
- The distributable includes the render-enabled wasm-bindgen artifact and does
  not download Chromium or another browser during installation.

### Chrome-style profile semantics

- A browser context owns the stable profile identity. The browser runtime may
  host many contexts; each context can use ephemeral state, the default local
  store, a caller-selected directory, an S3-compatible store, or an arbitrary
  `ProfileStore` implementation.
- `context.backup()` creates an explicit checkpoint. Dirty contexts use a
  debounced automatic checkpoint by default, and normal `close()` requires a
  final checkpoint. `close({ persist: "skip" })` is the deliberate force-close
  escape hatch.
- Writes use optimistic versions so two writers cannot silently overwrite one
  profile. The local adapter uses cross-process lock files, atomic rename,
  fsync, bounded reads, and restrictive permissions. The S3 adapter uses ETags
  and conditional writes. Profile containers optionally use AES-256-GCM.
- Rust `BrowserState::restore_context` validates the full versioned snapshot
  before mutation, keeps the context/page identities, replaces durable state,
  and increments generations so pre-restore work becomes stale. The WASM
  `contextRestore` ABI synchronizes restored cookies into every live page core.
- Current snapshot schema v1 persists cookies and context options. Full
  localStorage, sessionStorage, IndexedDB, cache, permissions, and service
  worker persistence are not implemented yet and must not be described as
  complete Chrome user-data-directory parity.

### Current verification evidence

- Strict TypeScript build for all four packages: passed.
- Public browser suite: 14 passed, 2 optional integration skips, zero failed.
  This includes a real save, browser destruction, and cookie restore through
  the rebuilt bundled WASM artifact. Direct screenshot capture also has a
  regression proving it works without opening a CDP listener.
- Private runtime suite: 62 passed, 6 real-artifact environment skips, zero
  failed. Protocol and storage suites: 6/6 each.
- Fresh tarball install with `--ignore-scripts`: passed direct in-process
  navigation; `processInfo()` reported no host or port.
- Render-enabled `obscura-wasm` release nextest: 86/86 passed.
- Render-enabled `obscura-cdp` release nextest: 203/203 passed with 3
  configured skips.
- The literal all-workspace `--features render` build cannot link
  `obscura-wasm` on the native target because Cargo feature unification pulls
  native V8 into its `cdylib` and V8's local-exec TLS relocations are invalid
  in a shared object. Verify WASM separately, then run the workspace gate with
  `--exclude obscura-wasm`.
- The workspace gate exposed and fixed a missing scoped `base64::Engine` import
  in the CDP IO tests. The portable CDP release suite then passed 54/54. The
  workspace-excluding-WASM rerun was externally terminated during V8 test
  binary linking (exit 143), so it has no final test summary.
- The exact release `obscura-cli` render build passed after the final Rust
  change. A final packed-tarball install contained 80 entries, included the
  WASM artifact and README, contained no `.node` file, navigated successfully,
  and opened no listening port. `npm audit` reported zero vulnerabilities.
- The 33/33 obstacle course is unavailable because the companion
  `../obscura-benchmark` checkout is absent.
- A separate Luna stress audit completed 30/30 complex data-URL navigations,
  20/20 multi-context isolation and cookie checks, 4/4 cookie profile restores,
  public-site navigation, timeout recovery, and idempotent close checks. Its
  no-listener screenshot defect was fixed and covered by the public suite.
  Remaining observed gaps are Promise-returning `evaluate()` support, redirect
  following in direct `Page.goto()`, and the bounded real-artifact test not
  completing within 20 seconds.
- The bundled WASM is 16,492,936 bytes with SHA-256
  `f03d26579c85330ef729be58e32b231f331af04080ea40980caa49fdab8b91fd`.

### Current commands

```bash
npm run build
npm test

cargo build --release -p obscura-wasm \
  --target wasm32-unknown-unknown --features render
wasm-bindgen --target nodejs --out-dir "$WASM_BINDGEN_OUT" \
  target/wasm32-unknown-unknown/release/obscura_wasm.wasm
npm run prepare:wasm -w @obscura/browser -- "$WASM_BINDGEN_OUT"

cargo nextest run --release --features render -p obscura-wasm
cargo nextest run --release --features render --workspace \
  --exclude obscura-wasm --no-fail-fast
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2 \
  cargo build --release -p obscura-cli --bins --features render
```

Latest verified implementation commit:
`8519ce8` (`feat: add bounded raw wasm cdp abi`).
The prior checkpoint was `c78d6a5` (`feat: route portable navigation network state through WASM`).
Earlier harness, portable rendering, resource discovery, PDF, and
boundary-hardening milestones remain recorded below.

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

Node is the implemented package Worker host and JavaScript CDP transport, but
the browser surface is still a bounded portability slice rather than Chromium
parity. The generated Deno bindings execute the same portable core, but
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

## Portable renderer and screenshot bridge (2026-08-14)

Implementation commit: `24cffdb648fd8736761d7a49350dd1fc821066c4`
(`feat: render screenshots in portable WASM`) on
`wasm-node-migration`, pushed to `origin/wasm-node-migration`.

### Implemented boundary

- `obscura-render/paint` is now the target-neutral raster stack. The native
  compatibility HTTP loader is isolated behind
  `obscura-render/native-resource-loader`; native
  `obscura-js/render` enables it explicitly.
- The wasm32 render graph retains layout, `tiny-skia`, text shaping,
  image/SVG decoding, paint, and PNG encoding, but contains no `ureq`,
  `mio`, `ring`, `rustls`, or `tokio`.
- WASM-safe timing and negative-cache stamps avoid calling unsupported
  `std::time::Instant` APIs in `wasm32-unknown-unknown`.
- `obscura-wasm` feature `render` exposes render ABI v1:
  `seedRenderResource`, `seedMissingRenderResource`, and
  `screenshotPng`. The live Rust DOM is laid out, painted, and PNG-encoded
  inside the WASM module. Node supplies only resource outcomes and persists
  returned bytes.
- The Node Worker/client bridge negotiates render ABI v1, enforces page
  generation/document-handle/revision compare-and-swap identity, copies byte
  inputs and outputs, validates limits and the PNG signature at both client
  and Worker boundaries, and never advertises a partial render method set as
  a complete renderer.
- Screenshot limits are 32,768 pixels per axis, 16,777,216 total pixels,
  16 MiB per seeded resource, 64 KiB per resource URL, and 128 MiB returned
  PNG. Scroll offsets must remain finite after conversion to the WASM `f32`
  ABI.

This is a real Node-V8 + Rust-WASM screenshot vertical slice. It is not yet a
complete browser migration: Node-owned navigation/fetch, parser and dynamic
script orchestration, remaining browser ops, PDF, CDP WebSocket transport,
Playwright, and the final `@obscura/browser` package remain.

### Artifact evidence

- Raw release WASM:
  `target/wasm32-unknown-unknown/release/obscura_wasm.wasm`,
  15,326,025 bytes, SHA-256
  `87da705b567df0095e4072996a2ed809dc688d0967ff4b2de1f5275cea78db2e`.
- Disposable Node wasm-bindgen package:
  `/workspaces/obscura-wasm-render-pkg.ulrWBl/`.
  - `obscura_wasm.js`: 22,194 bytes, SHA-256
    `b7034f42bd79ffd5a114e2b9af932bfd24b48273488fa66165a46a8650de5832`.
  - `obscura_wasm_bg.wasm`: 14,949,399 bytes, SHA-256
    `62d1b63a7a69fbbbbfd109c83425ee853205457fb0cef84fc265e5d80ff24af3`.
- Real Node Worker proof created a valid PNG from a styled live WASM document,
  verified its eight-byte signature and IHDR width/height, and used no native
  Obscura binary or addon.

### Verification evidence

- `cargo check --release -p obscura-render --target
  wasm32-unknown-unknown --features paint`: PASS.
- `cargo check --release -p obscura-wasm --target
  wasm32-unknown-unknown --features render`: PASS.
- `cargo check --release -p obscura-js --features render`: PASS, preserving
  the existing native resource-loader path.
- `cargo tree -p obscura-wasm --target wasm32-unknown-unknown --features
  render`: no `ureq`, `mio`, `ring`, `rustls`, or `tokio`.
- Focused release nextest:
  - `obscura-wasm --features render`: **43/43 passed**, run
    `f551aa18-5307-47de-b476-4bd4e612a04b`.
  - `obscura-render --features native-resource-loader`: **575/575 passed**,
    one configured skip, run `580d88a3-1e4f-4a79-9b76-d64916a316bf`.
- Node mock suite: **54 passed, 4 optional artifact skips, 0 failed**.
- Node suite with the real WASM wrapper: **56 passed, 2 legacy native-addon
  skips, 0 failed**. Both real-WASM direct ABI and full Worker screenshot
  paths ran.
- Full repository gate:
  `cargo-nextest nextest run --release --features render --no-fail-fast`:
  **1,450/1,450 passed, 4 configured skips, 0 failed**, run
  `e23ce170-50e1-4762-a87d-a13b03ad6091`; compile 25m42s, execution 51.246s.
- Exact required release build:
  `CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2 cargo build --release
  -p obscura-cli --bins --features render`: PASS in 3m15s.
  `target/release/obscura` is 93,575,920 bytes, SHA-256
  `602ea479a3012e3654e52d3d646adc42a2a851f055a4323a08bca200d6656105`.
- The companion `obscura-benchmark` repository is absent, so the 33/33
  obstacle course remains an unavailable external gate, not a pass.

Gemini 3.7 Flash was delegated a test-only render-bridge task with strict
single-file ownership. Codex independently reviewed its output, rejected
duplicated coverage and a second 128 MiB allocation, and retained only three
additive tests: partial-method capability reporting, result anti-aliasing, and
the real WASM-through-Worker screenshot path.

## Portable render-resource discovery checkpoint (2026-08-14)

Source commit: `f248863a2504ea002f38c0530a44462814f15c8e`
(`feat: discover render resources in portable WASM`). The remaining migration
work is decomposed into independently consumable packets in plan commit
`62031df93a810665bd33d2917254ad8b75c07130`.

### Implemented boundary

- Target-neutral CSS resource discovery now lives in `obscura-render/paint`.
  It classifies image and font requests, skips comments, strings, fragments,
  data URLs, variables and imports, handles nested blocks and UTF-8 safely,
  and preserves the native browser path.
- `obscura-dom` owns the shared document-base resolver. The first `<base>` is
  authoritative; an invalid, `data:`, or `javascript:` first base falls back
  to the document URL rather than consulting a later element.
- Portable render-resource ABI v1 exposes deterministic paged discovery and
  exact-profile seed/missing operations. Profiles cover no-CORS/include,
  CORS/same-origin and CORS/include. Page reset clears the outcome cache.
- The Node Worker/client bridge validates the complete capability set, limits,
  cursor progression, request profiles and page identity before and after each
  operation. Seed bytes are copied at both trust boundaries.
- The real-artifact integration test seeds a valid 2x3 PNG into one otherwise
  identical document and marks it missing in another, then proves that the
  WASM-produced screenshot bytes differ.

### Artifact evidence

- Raw release WASM: 15,363,565 bytes, SHA-256
  `50acd3870dd0e4d84f0d8ab633ea59e319330d24755310c0c7aa22a03249c0a6`.
- Disposable Node package: `/workspaces/obscura-resource-pkg.LNDqKH/`.
  - `obscura_wasm.js`: 24,855 bytes, SHA-256
    `1e668cb9a02b3690eacb55c8acb7b86012096e539e896f67249365145e1cc32f`.
  - `obscura_wasm_bg.wasm`: 14,983,898 bytes, SHA-256
    `9df31bcaab911c83ff78cb1efe04ad35c577927f66eb9a9186666a85c65c4cb7`.

### Verification evidence

- Focused release nextest:
  - `obscura-wasm --features render`: **46/46 passed**, run
    `4174c0df-45db-4c2f-8604-9516a70cd59a`.
  - `obscura-browser --features render`: **63/63 passed**, run
    `bd6afad2-3d33-46b2-a2bb-eb199a1e7c7a`.
- Node mock suite: **57 passed, 5 expected artifact skips, 0 failed**.
- Node suite with the fresh real WASM wrapper: **60 passed, 2 legacy
  native-addon skips, 0 failed**.
- Full repository render gate: **1,461/1,461 passed, 4 configured skips,
  0 failed**, run `fcb91513-e510-436a-bc29-dee02a9016fb`.
- Exact required CLI release build passed. The resulting verification binary
  is 93,532,528 bytes, SHA-256
  `136593b03d0c775ec1be6df0f215f708ee79389dbe2402a0de17c0113836855b`.
- Dependency inspection found no `deno_core`, `v8`, `tokio`, `mio`, `ureq`,
  `ring`, `rustls`, or `reqwest` in the portable WASM render graph.

### Unavailable external evidence

- The deterministic fixture runner produced all 63 Obscura PNGs, each
  nontrivial (5,723 to 36,167 bytes), with no Obscura log errors. Chromium
  comparisons could not run because the Python `playwright` package and a
  Chromium executable are absent.
- Representative top/bottom captures could not start because Python `numpy`
  is absent. No fidelity claim is made from those runs.
- `/workspaces/obscura-benchmark` is absent, so the required 33/33 obstacle
  course remains unavailable, not passed.

This checkpoint completes discovery and explicit resource seeding. It does not
yet supply Node-owned navigation/fetch/cookie orchestration, dynamic script
loading, portable PDF, CDP/WebSocket transport, Playwright compatibility, or
the final npm/npx package. Those are the remaining packets in
`.agent/files/remaining/`.

## Portable PDF checkpoint (2026-08-16)

Source commit: `ab36f8e6743b3efc822a4e709585018068ca96aa`
(`feat: generate portable WASM PDFs`).

### Implemented boundary

- Target-neutral pagination, page-range selection, raster budgets, JPEG image
  streams, xref construction and bounded PDF output now live in
  `obscura-render::pdf` and are shared by native `Page::raster_pdf` and the
  portable WASM path.
- WASM selects print media, lays out the live DOM with the seeded resource
  cache, captures bounded virtual page slices, and encodes the complete PDF
  inside the WASM module. Node only supplies options and transports bytes.
- PDF ABI v1 exposes `pdfAbiVersion()` and `pdf(optionsJson, documentHandle,
  revision)`. The Node bridge additionally checks Worker generation and
  validates identity before and after the call.
- Options are strict camelCase JSON with bounded viewport, paper, margins,
  scale and page-range fields. Both client and Worker enforce the 64 KiB
  options limit, 64 MiB output limit, `%PDF-` header, `%%EOF` trailer and
  deterministic byte copies.

### Verification evidence

- `obscura-wasm --features render` release nextest: **47/47 passed**, run
  `ddef262b-10e8-46f4-bd87-d609d6a21fde`.
- `obscura-browser --features render` release nextest: **63/63 passed**, run
  `fe231751-902f-4774-b008-2b756e1cd7db`.
- `obscura-render --features paint` release nextest: **589/589 passed, one
  configured skip**, run `5612bdf8-0696-4211-84d1-c8e65dc97f13`.
- Node mock suite: **62 passed, 6 expected skips, 0 failed**.
- Fresh real wasm-bindgen Node suite: **66 passed, 2 native-addon skips,
  0 failed**. The real PDF test generated the same three-page PDF twice,
  checked `%PDF-`, `%%EOF`, `/Count 3`, and a 100x80 point MediaBox after
  seeding a real image through the Packet R resource bridge.
- Fresh artifact hashes: raw WASM SHA-256
  `a46eacf0742912b17d4524fac07d577f784f7c921c809ae00d5ced0e55fe1e48`,
  wasm-bindgen wrapper SHA-256
  `3993dff39609e69f87d27134f16e90a78b13bd968ff9fb8d7c9e404f2593bbf9`,
  background WASM SHA-256
  `85a9104dfd94ac670126f8e24dd34e84bc59fbe6cc8c641f93ee5023db241239`.
- The wasm32 render dependency tree contains no `deno_core`, V8, Tokio,
  sockets, native HTTP client, rustls, ring or reqwest dependency.

This packet supplies portable PDF generation, not navigation/fetch
orchestration, dynamic script loading, CDP/WebSocket transport, Playwright
compatibility, or the final npm/npx package. Those remain in the subsequent
packets.

## Portable CDP navigation-network checkpoint (2026-08-17)

Source commit: `c78d6a5` (`feat: route portable navigation network state through
WASM`). This is the current pushed source checkpoint.

### Implemented boundary

- The Rust/WASM CDP target owns bounded per-session network enable state,
  request/response/loading-finished event queues, and response bodies capped at
  4 MiB and 128 retained entries.
- The Node worker performs HTTP navigation, streams the full response into the
  WASM navigation state, and copies only a bounded body into private metadata.
  The package CDP server removes that metadata from public action results,
  records it in WASM, emits events, and serves `Network.getResponseBody`.
- The package remains JavaScript plus WASM. Native `.node` modules, the native
  Obscura CLI, deno_core, and rusty_v8 are rejected or absent from the npm
  tarball.

### Verification

- Full release render gate: **1,491/1,491 passed, 4 configured skips**, run
  `5fc36316-6c3e-4f82-91d2-9c8b34e2cb39`.
- Portable WASM release nextest: **63/63 passed**, run
  `5299006f-7189-4fd1-a91a-a4b440932ef7`.
- Node host harness: **62 passed, 8 expected skips, 0 failed**.
- Real WASM package CDP suite: **6 passed, 1 optional Playwright skip, 0
  failed**. The real network test observed request/response/loading-finished
  events and retrieved the HTML body through `Network.getResponseBody`.
- A clean `@obscura/browser` tarball was installed into a temporary directory,
  connected through Playwright over CDP, navigated, evaluated a selector,
  captured a valid PNG, and produced a valid `%PDF-` document. The tarball
  contained no native addon or CLI binary.
- The exact repository CLI release build passed as a regression gate; the CLI
  is not used by the npm runtime.

### Remaining limits

This does not claim a complete browser. Network events currently describe
navigation metadata rather than every subresource, fetch, or XHR. Full
interception/cache/redirect credential policy, broad CDP domain parity, and
complete DOM mutation/event synchronization remain. The companion
`obscura-benchmark` repository is absent, so the required 33/33 obstacle course
is unavailable and is not counted as passed.

## Portable page-fetch network checkpoint (2026-08-17)

Source commit: `1304eb6` (`feat: route page fetch events through portable wasm cdp`).

The page-realm `fetch()` host adapter now assigns bounded request and loader IDs,
records Fetch events and response bodies in the Rust/WASM CDP queue, and emits
them through the package's external CDP session. `Network.getResponseBody` is
available for completed page fetches, including asynchronous work that finishes
after the original `Runtime.evaluate` action returns. Queue size and retained
metadata are bounded, and navigation/release generation changes discard stale
records.

Verification after this checkpoint:

- `obscura-wasm` release nextest: **64/64 passed**.
- Full release render nextest: **1,492/1,492 passed, 4 configured skips**.
- Exact release CLI build with render: passed.
- Node WASM harness: **62 passed, 8 expected skips, 0 failed**.
- Real `@obscura/browser` package suite: **6 passed, 1 optional Playwright skip,
  0 failed**, including asynchronous page-fetch Network events and response-body
  retrieval.

The remaining browser-parity work is still broader than this slice: page script
and subresource events, complete interception/cache/redirect policy, broader CDP
domains, DOM mutation/event synchronization, and the unavailable companion
obstacle course remain open.

## Portable script-network checkpoint (2026-08-17)

Source commit: `7322acf` (`feat: expose portable script network events`).

External classic script loads now use the same bounded WASM CDP network ingress
as page fetches. The Node adapter assigns a loader/request identity, records
`Network.requestWillBeSent`, `Network.responseReceived`, and
`Network.loadingFinished` with `type: "Script"`, retains response bytes only
within the existing cap, and preserves the package's external session ID.
Failed script responses are represented as bounded failed network records and
are never evaluated as source. Module and dynamic-import fetches share this
adapter path.

Verification:

- Full release render nextest: **1,492/1,492 passed, 4 configured skips**.
- Exact release CLI build with render: passed.
- Node WASM harness: **62 passed, 8 expected skips, 0 failed**.
- Real `@obscura/browser` package suite: **6 passed, 1 optional Playwright skip,
  0 failed**, including a real external script event and
  `Network.getResponseBody` assertion.

## Portable render-resource checkpoint (2026-08-17)

Source commit: `461ce77` (`feat: fetch render resources through portable wasm`).

The package now drives the existing WASM `renderResourceRequests` ABI before
`Page.captureScreenshot` and `Page.printToPDF`. The Worker validates every
image/font URL through the portable SSRF policy, sends the WASM cookie header,
follows bounded HTTP redirects, caps resource bytes at 16 MiB, records bounded
`Network` metadata/body events, seeds successful bytes into the WASM render
cache, and negative-seeds failed resources so a broken asset cannot abort a
capture. Preparation has a 5-second default deadline, is skipped for older
partial render cores, and runs entirely inside the Node/WASM package. The
package test covers a real HTTP PNG request, `type: "Image"`, response-body
retrieval, and a valid screenshot.

Verification after this checkpoint:

- Full release render nextest: **1,492/1,492 passed, 4 configured skips**,
  run `759a4938-7a4d-4a83-b0eb-df69b6cb5064`.
- Exact release CLI build with render: passed. The CLI remains a regression
  gate only and is not a runtime dependency of the npm package.
- Node WASM harness: **62 passed, 8 expected skips, 0 failed**.
- Real `@obscura/browser` package suite against the render-capable artifact:
  **6 passed, 1 optional Playwright skip, 0 failed**.

Remaining browser-parity work includes external stylesheet materialization and
stylesheet/import network events, complete interception/cache/redirect policy,
broader CDP domains, full DOM mutation/event synchronization, npm release
packaging, and the unavailable companion obstacle course. This checkpoint does
not claim a complete browser migration.

## Portable Fetch interception checkpoint (2026-08-17)

Source commit: `4c6d842` (`feat: move fetch interception into portable wasm cdp`).

`PortableCdp` now owns bounded `Fetch.enable`, `Fetch.disable`,
`Fetch.continueRequest`, `Fetch.fulfillRequest`, `Fetch.failRequest`, and
`Fetch.getResponseBody` state. It emits `Fetch.requestPaused` into the WASM CDP
event queue and drains opaque continue/fulfill/fail resolutions to the host.
Patterns, paused requests, response bodies, headers, and resolution queues are
bounded. Session detach, target close, page reset, and Worker shutdown clear or
resolve paused work. Node performs only the HTTP request and applies the
WASM-produced resolution; it does not own interception policy.

Verification after this checkpoint:

- Release `obscura-wasm` portable CDP tests: **17/17 passed**. The environment
  does not currently have `cargo-nextest`, so this focused fallback used
  `cargo test --release --features render -p obscura-wasm cdp::tests`.
- Real WASM `@obscura/browser` package suite: **10 passed, 1 optional
  Playwright skip, 0 failed**. It covers request pause, URL rewrite/continue,
  synthetic fulfill, failure, event delivery while idle, and response-body
  retrieval.
- Real WASM Node harness: **68 passed, 2 expected native-addon skips, 0
  failed**.
- Exact render CLI release build: passed.
- Final disposable render artifact used by the real gates: wrapper SHA256
  `b2685cb5e5569e2a0e9627c5be7d96c71c4463003597a34eb06b1a2c5b9a8025` and
  background WASM SHA256
  `89976616e4b01f967df88cabb1fcd212ebb1d6698eb6db834bc9ce3a157a6fc7`.

This is a Fetch interception milestone, not full browser parity. Static
navigation/resource interception, cache policy, redirects, response-stage
interception, and the companion obstacle course remain open. The full
workspace nextest gate must be rerun in an environment with cargo-nextest.

## Portable Fetch response-stage checkpoint (2026-08-17)

Source commit: `40552d8` (`feat: support response-stage portable fetch interception`).

Fetch patterns now carry an explicit `requestStage` (`Request` or `Response`).
The portable Rust CDP core matches the stage, includes response status and
headers in response-stage pause events, and stores a bounded response body for
`Fetch.getResponseBody`. The Node host buffers only within the existing
navigation response cap, then applies continue, fulfill, or fail resolutions
inside the same page-fetch lifecycle. The page realm receives only the
data-only response envelope.

Verification after this checkpoint:

- Rust focused release fallback: **2/2 portable Fetch CDP tests passed**.
  `cargo-nextest` is unavailable in this environment, so the equivalent
  `cargo test --release --features render -p obscura-wasm cdp::tests::portable_fetch`
  command was used and must not be reported as nextest.
- Fresh render-enabled wasm32 release build passed.
- Real `@obscura/browser` package suite: **10 passed, 1 optional Playwright
  skip, 0 failed**. It covers response-stage pause, status/headers,
  `Fetch.getResponseBody`, synthetic fulfillment, and page reuse.
- Real Node WASM harness: **68 passed, 2 expected native-addon skips, 0
  failed**.
- Exact release CLI render build passed as a repository regression gate; the
  native binary is not included in or required by the npm package.

Remaining limitations are unchanged for static parser/resource interception:
`Page.navigate` still executes its host fetch/resource pipeline as one bounded
operation, so a Fetch pause cannot yet safely suspend parser scripts,
stylesheets, navigation, or render-resource loads for an external CDP command.
Those paths need an action/re-entry protocol before they can share the same
interceptor without deadlocking the Worker request queue. Cache and redirect
policy, broader CDP domains, full Playwright parity, Deno package integration,
and the unavailable companion obstacle course remain open.

## Portable parser/resource Fetch checkpoint (2026-08-17)

Source commit: `ea0f2ea` (`feat: intercept portable navigation resources`).

The Node host now applies the portable Rust Fetch request policy to document
navigation, classic parser scripts, linked stylesheets and render-resource
loads. The Worker keeps host fetch records bounded and cancellable. Portable
CDP request/poll/record operations are allowed to re-enter while host I/O is
awaiting a pause; DOM and page operations remain serialized. During a paused
navigation, the package polls only Fetch control events and defers ordinary
Network metadata until commit, preserving Document-before-subresource event
ordering. Continue, URL/method/header rewrite, fulfill, fail, cancellation,
and page reset paths all use data-only envelopes.

Verification after this checkpoint:

- Real `@obscura/browser` package suite against the fresh render WASM wrapper:
  **10 passed, 1 optional Playwright skip, 0 failed**. The local fixture now
  proves request interception for a parser script, a stylesheet during
  navigation, and an image during screenshot preparation, including a
  continuation that unblocks the pending host operation.
- Real Node WASM harness against the same wrapper: **68 passed, 2 expected
  native-addon skips, 0 failed**.
- `node --check` passed for the changed Worker, package server, and real
  artifact test; `git diff --check` passed.
- The Rust response-stage Fetch tests and fresh wasm32 render build remain
  green from the preceding `40552d8` checkpoint.

The portable host still does not provide a complete browser: cache semantics,
redirect interception policy, response-stage parser/resource interception,
all Network/Fetch domains, full DOM mutation/event synchronization, complete
Playwright compatibility, Deno package integration, and the unavailable
companion obstacle course remain open.

## Portable resource response-stage checkpoint (2026-08-17)

Source commit: `b674f79` (`feat: extend Fetch response interception to resources`).

Parser scripts, linked stylesheets, navigation documents, and render-resource
loads now pass bounded response metadata and bytes through the same portable
Fetch response-stage ABI used by page `fetch()`. A response-stage pause can be
continued, fulfilled, or failed; `Fetch.getResponseBody` is available while a
resource is paused. The host buffers only within the existing navigation,
stylesheet, or render-resource caps and keeps response processing inside the
Worker deadline/cancellation lifecycle.

Verification:

- Fresh real render-WASM package suite: **10 passed, 1 optional Playwright
  skip, 0 failed**, including a stylesheet response-stage pause/body read and
  a render screenshot after continuation.
- Real Node WASM harness: **68 passed, 2 expected native-addon skips, 0
  failed** on the same Worker implementation.
- Node syntax and diff checks passed.

This still is not complete browser parity. Cache validation/revalidation,
redirect policy under interception, response-stage edge semantics for every
resource type, broader CDP domains, mutation/event synchronization, full
Playwright compatibility, Deno packaging, and the unavailable obstacle course
remain open.

## Portable bounded HTTP cache checkpoint (2026-08-17)

Source commit: `32cb905` (`feat: add portable bounded HTTP cache policy`).

The portable Rust CDP target now owns `Network.setCacheDisabled` policy and
`Network.clearBrowserCache` state. The Node adapter keeps a per-page bounded
HTTP response cache for successful GET/HEAD document, script, stylesheet, and
render-resource responses. Keys include URL, method, resource type, and
normalized request headers. Entries use deterministic LRU eviction with
128-entry and 16 MiB aggregate caps; `no-store`, `no-cache`, and `set-cookie`
responses are excluded. Request interception runs before lookup, and
response-stage interception still runs for cache hits. Cache clear and
disabling are propagated from the WASM CDP state to the host store.

Verification:

- Rust release fallback cache-policy test: **1/1 passed**. The environment
  lacks `cargo-nextest`; the equivalent release `cargo test` command was used.
- Fresh render-enabled wasm32 release build passed.
- Real package suite: **10 passed, 1 optional Playwright skip, 0 failed**.
  The fixture proves cache reuse, `Network.setCacheDisabled`, and
  `Network.clearBrowserCache` by counting script requests, alongside the
  existing request/response interception and screenshot coverage.
- Real Node WASM harness: **68 passed, 2 expected native-addon skips, 0
  failed**.
- Exact render CLI release build passed as a repository regression gate.

Page `fetch()`/XHR currently still use their own response path and are not yet
backed by this HTTP cache; adding that without changing CORS, credentials,
cookie, and response-stage ordering is a remaining bounded task. Full cache
revalidation/expiry/Vary semantics, redirect policy under interception,
broader CDP domains, full DOM mutation/event synchronization, complete
Playwright compatibility, Deno packaging, and the unavailable obstacle course
remain open.

## Shared CDP Emulation and Fetch checkpoint (2026-08-21)

Source commits: `1a3943c` (`feat: share portable emulation state`) and
`7261490` (`feat: share portable Fetch state`).

The reusable `obscura-cdp` portable state now owns page display metrics (bounded
viewport, device scale, mobile flag, emulated media, and focus emulation) and
dispatches `Emulation.setDeviceMetricsOverride`,
`Emulation.clearDeviceMetricsOverride`, `Emulation.setEmulatedMedia`,
`Emulation.setFocusEmulationEnabled`, and `Page.getLayoutMetrics` without
Tokio, sockets, V8, or native browser dependencies. The WASM dispatcher routes
these commands through that shared state while retaining a temporary target
mirror for legacy event/render paths.

The Fetch slice adds `portable_fetch.rs`. Shared state owns Fetch patterns,
paused request IDs, bounded one-shot continue/fulfill/fail resolutions, and
Fetch response-body lookup. The WASM adapter routes Fetch command handling to
that module, registers pauses and drains shared resolutions, while the host
continues to supply request metadata and perform network I/O.

Verification:

- Portable `obscura-cdp` tests: **27/27 passed**.
- `obscura-wasm` tests: **68/68 passed** without render and **74/74 passed**
  with render.
- Native-server `obscura-cdp` check passed.
- wasm32 render check passed.
- `git diff --check` passed.

The migration is not complete: local/session storage and document-cookie
runtime synchronization remain partial, DOM/accessibility/input/runtime and
navigation actions remain partly in the legacy dispatcher, event subscriptions
are not yet in shared CDP state, and the final ABI/package and
Playwright/Puppeteer acceptance gates remain open.

## Shared CDP context-cookie checkpoint (2026-08-21)

Source commit: `067525c` (`feat: share portable context cookie state`).

This slice adds `portable_storage.rs` and context-owned cookie
state to `obscura-cdp`. `Network.getAllCookies`, `Storage.getCookies`,
`Network.setCookies`, `Storage.setCookies`, `Network.deleteCookies`,
`Network.clearBrowserCookies`, and `Storage.clearDataForOrigin` now have a
transport-free dispatcher with bounded counts/bytes and expiry filtering. The
WASM adapter routes these commands through shared context state and mirrors
mutations into every legacy page core in the context while the remaining
navigation/document-cookie paths are migrated.

Verification: portable CDP **29/29 passed**, WASM **70/70 passed**,
render-enabled WASM **76/76 passed**, native-server CDP check passed, wasm32
render check passed, and `git diff --check` passed. `cargo-nextest` is not
installed in this environment, so these are the supported `cargo test`
fallback results rather than nextest claims.

## Shared CDP page-history checkpoint (2026-08-21)

Source commit: `9fdfed3` (`feat: share portable page history`).

`BrowserState` now owns bounded per-page history entries and reset state.
`Page.getNavigationHistory` and `Page.resetNavigationHistory` are dispatched
through the shared portable module, and shared navigation completion appends a
new entry while preserving exact CDP wire names such as `userTypedURL` and
`transitionType`. The WASM target mirror remains only for legacy host actions.

Verification: portable CDP **29/29 passed**, WASM **70/70 passed**,
render-enabled WASM **76/76 passed**, native-server CDP check passed, wasm32
render check passed, and `git diff --check` passed.

## Shared portable CDP router checkpoint (2026-08-21)

The transport-free `obscura-cdp` crate now exposes
`portable_dispatch::{dispatch_browser, dispatch_page}` as the single domain
selection layer for the migrated Browser/Target, Storage, Fetch, Emulation,
Network, and Page metadata commands. The WASM adapter no longer invokes each
portable domain dispatcher from its main page path or keeps the removed
per-domain routing helpers. It calls the shared router, then mirrors only the
legacy `ObscuraCore` fields still needed by DOM/runtime/render host actions.
Host-backed Runtime, DOM, Input, navigation, event queues, and capture remain
outside this state-only router by design until their action/backend contracts
are migrated.

Verification for this checkpoint:

- Portable `obscura-cdp`: **31/31 passed** with `--no-default-features
  --features portable`.
- `obscura-wasm`: **70/70 passed** without render and **76/76 passed** with
  render.
- Native-server `obscura-cdp` check passed.
- wasm32 render check passed.
- `git diff --check` passed.

The remaining C3 work is still substantial: move connection/session/event
ownership into the shared router, define portable DOM/Runtime/Input host
contracts, cut over navigation and render actions, remove the legacy target
maps, and run final package/Playwright/Puppeteer/concurrency gates.

## Shared portable host-action mapping checkpoint (2026-08-21)

The portable CDP crate now owns `portable_action::from_kind`, the data-only
mapping from navigation, Runtime, Input, screenshot/PDF, and isolate-wake
operations to `EngineAction`. `obscura-wasm` retains host execution and action
completion, but no longer defines the protocol-to-engine action variants.
Unknown kinds fail as bounded `CdpFailure::Unsupported` values before any host
queue side effect.

Verification: portable `obscura-cdp` **33/33 passed**, `obscura-wasm`
**70/70 passed**, and prior render/wasm32 checks remain green from the router
checkpoint. The remaining migration still includes portable DOM/runtime
backend contracts, shared event ownership, legacy target-map removal, and
final package/client acceptance gates.

## Shared portable DOM and Runtime checkpoint (2026-08-21)

The portable CDP crate now owns `portable_dom.rs` and `portable_runtime.rs`.
DOM command validation and wire shapes for `DOM.enable`, `DOM.disable`,
`DOM.getDocument`, selector queries, outer HTML, attributes, node description,
and child-node events are implemented against a small `DomBackend` contract.
Runtime enable/disable and the execution-context-created event are likewise
derived from shared page metadata. The WASM adapter supplies only
`ObscuraCore` DOM operations through `CoreDomBackend`; the legacy DOM and
Runtime lifecycle match arms were removed. Request shaping for navigation,
reload, evaluation, remote-object operations, and Input commands is also now
owned by `portable_action::from_request`.

Verification for this working checkpoint:

- Portable `obscura-cdp`: **37/37 passed**.
- `obscura-wasm`: **70/70 passed** without render.
- DOM request/response/event behavior remains covered by the existing WASM
  DOM test and new portable DOM/runtime unit tests.
- Render and wasm32 checks are still required before this checkpoint is
  committed.

Remaining C3 work includes DOMSnapshot/Accessibility, render wire handlers,
shared event/session ownership, navigation completion/lifecycle cutover,
legacy target-map removal, ABI/package finalization, and real client gates.

## Shared portable IO checkpoint (2026-08-21)

`obscura-cdp::portable_io::IoState` now owns bounded IO stream bytes,
connection ownership, eviction cleanup, `IO.read`/`IO.close` validation, and
connection teardown. The WASM adapter only decodes host-provided bytes,
registers them with the shared state, and forwards the request; its duplicate
IO command match and stream-owner map were removed. Evicted handles are also
removed from the owner map so repeated captures cannot grow metadata without
bound.

Verification: portable `obscura-cdp` **39/39 passed**, `obscura-wasm`
**70/70 passed**, render-enabled WASM **76/76 passed**, wasm32 render check
passed, native-server CDP check passed, and `git diff --check` passed.

Remaining C3 work is DOMSnapshot/Accessibility, render wire semantics, shared
event/session ownership, navigation lifecycle cutover, legacy target-map
removal, ABI/package finalization, and real client/concurrency gates.

## Shared portable render wire checkpoint (2026-08-21)

`obscura-cdp::portable_render` now owns screenshot/PDF command validation,
PNG/PDF response shapes, result-size bounds, PDF `ReturnAsStream` handling,
and stream ownership handoff. WASM supplies `CoreRenderBackend`, which calls
the existing in-module layout/paint/PNG/PDF implementation. The old render
command response helpers were removed from `obscura-wasm/src/cdp.rs`; the
non-render build still represents capture as a bounded host action.

Verification: portable `obscura-cdp` **41/41 passed**, `obscura-wasm`
**70/70 passed** without render and **76/76 passed** with render, native-server
CDP check passed, wasm32 render check passed, and `git diff --check` passed.

Remaining C3 work is DOMSnapshot/Accessibility, shared event/session
ownership, navigation lifecycle cutover, legacy target-map removal,
ABI/package finalization, and real Playwright/Puppeteer/concurrency gates.

## Shared portable event queue checkpoint (2026-08-21)

`BrowserState` now owns bounded per-connection `CdpEvent` queues, byte/count
eviction, polling with complete-frame byte limits, session-event cleanup, and
connection/terminal teardown cleanup. The WASM adapter's `Connection` keeps
only discovery and attachment metadata; target discovery, auto-attach,
network, Fetch, detach, and target-destroy notifications now enter the shared
queue. The old WASM-local `VecDeque<Value>` and queue helpers were removed.

Verification for this checkpoint:

- Portable `obscura-cdp`: **42/42 passed** with
  `--no-default-features --features portable`.
- `obscura-wasm`: **70/70 passed** without render and **76/76 passed** with
  render.
- wasm32 render check passed.
- `git diff --check` passed.

The queue checkpoint does not yet remove the WASM target/session maps or
numeric-to-wire identity maps. Remaining C3 work is DOMSnapshot/Accessibility,
shared session identity cutover, navigation lifecycle completion, legacy target
state removal, ABI/package finalization, and real Playwright/Puppeteer/
concurrency gates.

## Shared DOMSnapshot and Accessibility checkpoint (2026-08-22)

The portable CDP dispatcher now owns `DOMSnapshot.enable/disable`,
`DOMSnapshot.captureSnapshot`, `Accessibility.enable/disable`, and
`Accessibility.getFullAXTree` routing. `DomBackend` exposes backend-only hooks
for the payloads, and the WASM `CoreDomBackend` builds both responses from the
live DOM handles. Snapshot backend IDs match the WASM DOM node IDs, bounded
tree walks retain deterministic parent/index ordering, synthetic layout fields
retain the existing browser-use-compatible shape, and AX roles/names,
properties, parent IDs, and child IDs are emitted from the same tree.

Verification for this checkpoint:

- Portable `obscura-cdp`: **44/44 passed** with
  `--no-default-features --features portable`.
- `obscura-wasm`: **71/71 passed** without render and **77/77 passed** with
  render, including a live snapshot/AX command test.
- wasm32 render check passed.
- native-server CDP check passed.
- `git diff --check` passed.

Remaining C3 work is shared session identity cutover, navigation lifecycle
completion, legacy target-state removal, ABI/package finalization, and real
Playwright/Puppeteer/concurrency gates.

## Shared identity-map removal checkpoint (2026-08-22)

The WASM adapter no longer keeps duplicate `shared_connections`,
`shared_sessions`, `shared_contexts`, or `shared_pages` maps. Wire IDs are
validated and converted directly to the monotonic `BrowserState` identities:
`page-N` to `PageId(N)`, `context-N`/`default` to `ContextId(N)` and the
numeric suffix of `{target}-session-N` to `SessionId(N)`. Connection and stream
ownership uses the same numeric `ConnectionId` on both sides. The adapter still
keeps its wire-facing target/session metadata and host action records, but no
second identity source of truth remains.

Verification for this checkpoint:

- `obscura-wasm`: **71/71 passed** without render and **78/78 passed** with
  render, including a multi-context/page CdpEngine identity round-trip test.
- wasm32 render check passed.
- `git diff --check` passed.

Remaining C3 work is navigation lifecycle/action completion, reduction of the
legacy target/session metadata, raw-byte ABI/package finalization, and real
Playwright/Puppeteer/concurrency gates.

## Shared action identity checkpoint (2026-08-22)

The duplicate WASM-to-portable `shared_actions` map is removed. The bounded
host-action queue's numeric wire ID is now the same monotonic `EngineActionId`
owned by `BrowserState`; queueing, completion, stale-target cancellation, and
late completion rejection all derive that identity directly. A debug parity
assertion catches allocator drift during development.

Verification for this checkpoint:

- `obscura-wasm`: **72/72 passed** without render and **78/78 passed** with
  render.
- wasm32 render check passed.
- native-server CDP check passed.
- `git diff --check` passed.

Remaining C3 work is navigation lifecycle/action payload ownership, reduction
of the legacy target/session metadata, raw-byte ABI/package finalization, and
real Playwright/Puppeteer/concurrency gates.

## Shared host-action identity checkpoint (2026-08-22)

The WASM adapter no longer keeps a duplicate `shared_actions` map. The local
bounded host-action queue and `BrowserState` now share the same monotonic
numeric action ID; completion and stale-target cancellation derive the shared
`EngineActionId` directly. The wire action response remains unchanged for
existing hosts.

Verification for this checkpoint:

- `obscura-wasm`: **72/72 passed** without render and **78/78 passed** with
  render.
- wasm32 render check passed.
- native-server CDP check passed.
- `git diff --check` passed.

Remaining C3 work is navigation lifecycle/action payload ownership, reduction
of the legacy target/session metadata, raw-byte ABI/package finalization, and
real Playwright/Puppeteer/concurrency gates.
## Bounded raw CDP WASM ABI checkpoint (2026-08-22)

The WASM crate now exposes a transport-only raw byte surface around the shared
portable CDP state. `browserCreate`/`browserClose` keep up to 4096 independent
`PortableCdp` instances in a thread-local registry; `connectionOpen` and
`connectionClose` scope host connections to one instance. `cdpIngest` accepts
one bounded UTF-8 CDP request and returns one little-endian u32 length-prefixed
response frame. `cdpDrainEvents` and `cdpDrainActions` return independently
framed batches, with complete-frame byte limits and a 512-frame cap. Action
frames carry camelCase `actionId`, `generation`, `kind`, `targetId`,
`requestId`, `sessionId`, and `payload`; the completion batch validates the
echoed generation before calling the existing CDP completion path. Raw
responses remove the legacy inline `obscuraAction`
field, so host execution and response traffic cannot be accidentally consumed
twice. `cdpRawAbiVersion` is 2; the existing string `cdpAbiVersion` remains 1.

The registry is deliberately Node-independent and does not add V8, sockets,
filesystem access, or a JS package. Navigation/network/render host actions,
context persistence, and the Node/Playwright adapter still remain follow-up
work. The frame envelope is a bounded ABI slice, not a claim that the entire
browser has no host services.

Verification for this checkpoint:

- `obscura-cdp`: **44/44 passed** with
  `--no-default-features --features portable`.
- `obscura-wasm`: raw registry/action/event/completion test passed; render
  suite **79/79 passed**.
- wasm32 render check passed.
- native-server CDP check passed.
- `git diff --check` passed.

Remaining C3 work is navigation lifecycle/action payload ownership, reduction
of legacy target/session metadata, raw ABI hardening/real-artifact coverage,
Node package and Playwright integration, and real concurrency gates.

## Shared context/session/network state cutover checkpoint (2026-08-22)

The portable adapter now treats `BrowserState` as the source of truth for
durable browser contexts, live sessions, Network enablement, and Fetch
patterns. Duplicate `PortableCdp` context sets, connection/session target maps,
per-target network-enabled session sets, and per-target Fetch pattern maps were
removed. Session ownership, attachment, network events, and interception now
query the shared state directly. Target close and browser-context disposal keep
the existing detached-event ordering by capturing shared sessions before the
shared dispatcher removes pages.

`BrowserState` exposes a bounded versioned context snapshot (`schemaVersion: 1`)
containing only context options and cookies. Pages, sessions, action queues,
events, and target identities are intentionally not persisted. The raw WASM
surface exports `contextExport` and `contextImport`; imports validate the schema,
cookie limits, and byte limit before allocating a fresh context identity.

Verification for this checkpoint:

- portable `obscura-cdp`: **46/46 passed** with
  `--no-default-features --features portable`.
- `obscura-wasm`: **73/73 passed** without render and **79/79 passed** with
  render.
- wasm32 render check passed.
- native-server CDP check passed.
- `git diff --check` passed.
- Real release wasm-bindgen artifact (`obscura_wasm_bg.wasm`, SHA-256
  `78d41d8cefc183ddcd65581dc60bbb204038c487324eba56419c391d6dd000d4`) passed
  a Node smoke covering context export/import, multi-context target creation,
  CDP attach, action drain/completion, and browser/connection teardown.

Remaining C3 work is navigation lifecycle/action payload ownership, reduction
of remaining target metadata, raw ABI hardening, a thin Node transport and
Playwright/Puppeteer integration, and real concurrency/resource gates. The
companion `obscura-benchmark` obstacle course is still unavailable in this
workspace.

## Raw ABI Node transport cutover checkpoint (2026-08-22)

The Node Worker now prefers the bounded `cdpRawAbiVersion: 2` surface when the
WASM module exports the complete host-helper set. It creates one raw WASM
browser per persistent Worker, reuses that browser across CDP connections, and
never constructs the legacy `PortableCdp` object in raw mode. Requests, event
frames, action frames, and batched completions cross the Worker/WASM boundary
as bounded `Uint8Array` envelopes; the Worker decodes only the short-lived
records needed to route a host action or deliver an event. A cached export table
avoids rebinding functions on each command. Modules without the complete raw
helper ABI retain the legacy path for compatibility.

The raw host-helper ABI now includes stream ingress, network metadata, Fetch
interception/resolution, cache policy, response-cache clearing, and Fetch
cancellation. This keeps network/cache/interception state in WASM while Node
retains only HTTP response bytes and V8/network execution. The package CDP
adapter translates raw action metadata to its existing host-action executor,
then sends generation-checked batched completions back to WASM. Screenshot
resource preparation is performed before the raw render command so resource
Fetch interception remains observable.

The Worker/client also expose raw context snapshot export/import calls, so a
future persistence provider can move bounded snapshot bytes without recreating
context semantics in JavaScript.

Verification for this checkpoint:

- `obscura-wasm`: **73/73 passed** without render and **79/79 passed** with
  render.
- portable `obscura-cdp`: **46/46 passed**.
- wasm32 render check and native-server CDP check passed.
- npm package tests without a raw artifact: **9 passed, 2 skipped**.
- npm package tests with real release wasm-bindgen artifact:
  **10 passed, 1 skipped**. The real test covered navigation, evaluation,
  cookies, Network events and bodies, Fetch request/response interception,
  cache disable/clear, stylesheet/image resource interception, screenshot, and
  PDF.
- Artifact used for the raw route: `obscura_wasm_bg.wasm` SHA-256
  `1ca2b9f4be7e03c583e08b9f7e151b9e6a787c465afe7f8b09ff004e262ed6bb`;
  wrapper SHA-256
  `a91b9413b181ef8d5041a0e7848fb8eebea0db6067b1639d5f183fd33377652d`.
- `git diff --check` passed.

This is a transport cutover, not completion of C3.8/C3.9: the package still
has browser-level target/context bookkeeping and a compatibility routing table,
the WASM `PortableCdp` migration scaffolding remains in the crate, and
Playwright/Puppeteer/concurrency/resource gates plus the missing companion
obstacle course remain outstanding.

## Shared page Network/display ownership checkpoint (2026-08-22)

`crates/obscura-wasm/src/cdp.rs` no longer stores a second per-target copy of
response bodies, cache/header policy, viewport metrics, emulated media, or
focus emulation. Those bounded values now live only in `obscura_cdp::state::BrowserState`;
fallback command paths and render/layout adapters read and update that state
directly. Network and Fetch response bodies therefore have one bounded
eviction queue per page instead of duplicated `PortableCdp::Target` and
`BrowserState::PageNetworkState` buffers. This is a memory/resource fix and
also removes a source-of-truth split; it does not yet make Node share one
Worker across package targets.

Verification:

- `cargo test --offline -p obscura-wasm --no-default-features --quiet`: **73/73**.
- `cargo test --offline -p obscura-wasm --features render --quiet`: **79/79**.
- `cargo check --offline -p obscura-wasm --features render --target wasm32-unknown-unknown --quiet` passed.
- `git diff --check` passed.
